/// IPv4 네트워크 스택 — ARP / ICMP / UDP (ALPHA 12)
///
/// TCP는 구현하지 않았다. ipv4.rs에 PROTO_TCP 상수가 있는 것은 수신 패킷의
/// 프로토콜 번호를 식별하기 위한 것일 뿐, TCP 상태 머신은 없다.
///
/// ## 레이어 구조
///
/// ```
/// Application (demo)
///     │
/// UDP / ICMP (net::udp, net::icmp)
///     │
/// IPv4 (net::ipv4) + ARP (net::arp)
///     │
/// VirtIO Net (virtio::VirtioNet) — Ethernet 프레임 TX/RX
/// ```
///
/// ## 지원 기능
/// - ARP: 요청/응답, ARP table 캐시, ARP request 수신 시 자동 응답
/// - IPv4: 헤더 빌드/파싱, 체크섬
/// - ICMP: Echo request/reply (ping)
/// - UDP: 단방향 송신

pub mod arp;
pub mod checksum;
pub mod icmp;
pub mod ipv4;
pub mod udp;

use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use crate::virtio::VirtioNet;
use arp::ArpTable;

// ── 수신 패킷 디스패치 결과 ────────────────────────────────────────────────────

pub enum Packet {
    IcmpEchoReply { src: [u8; 4], id: u16, seq: u16 },
    Udp           { src: [u8; 4], src_port: u16, dst_port: u16, data: Vec<u8> },
}

// ── 네트워크 스택 ─────────────────────────────────────────────────────────────

pub struct NetworkStack {
    nic:       VirtioNet,
    pub mac:   [u8; 6],
    pub ip:    [u8; 4],
    arp_cache: ArpTable,
    ip_id:     u16,
    icmp_seq:  u16,
    icmp_id:   u16,
}

impl NetworkStack {
    pub fn new(nic: VirtioNet, ip: [u8; 4]) -> Self {
        let mac = nic.mac;
        Self {
            nic,
            mac,
            ip,
            arp_cache: ArpTable::new(),
            ip_id:    1,
            icmp_seq: 0,
            icmp_id:  0x4D55, // 'MU' — 식별자
        }
    }

    // ── ARP ──────────────────────────────────────────────────────────────────

    /// `target_ip`에 대한 MAC 주소를 반환. 캐시 miss 시 ARP request 전송 후 ~1초 폴링.
    pub fn arp_resolve(&mut self, target_ip: [u8; 4]) -> Option<[u8; 6]> {
        if let Some(mac) = self.arp_cache.lookup(target_ip) {
            return Some(mac);
        }
        // ARP request 전송 (raw Ethernet 프레임)
        let req = arp::build_request(self.mac, self.ip, target_ip);
        self.nic.send(&req);

        let deadline = tick() + 18; // ~1초
        loop {
            if let Some(frame) = self.nic.try_recv() {
                if let Some(pkt) = arp::parse(&frame) {
                    if pkt.op == 2 { // ARP reply
                        self.arp_cache.insert(pkt.sender_ip, pkt.sender_mac);
                        if pkt.sender_ip == target_ip {
                            return Some(pkt.sender_mac);
                        }
                    }
                }
            }
            if tick() > deadline { return None; }
            pause();
        }
    }

    // ── ICMP Ping ────────────────────────────────────────────────────────────

    /// `dst` 호스트에 ICMP Echo Request 전송 후 reply 기다림.
    ///
    /// 성공 시 `Some(seq)`, 타임아웃 시 `None`.
    pub fn ping(&mut self, dst: [u8; 4]) -> Option<u16> {
        let dst_mac = self.arp_resolve(dst)?;

        let seq = self.icmp_seq;
        self.icmp_seq = self.icmp_seq.wrapping_add(1);

        let icmp_msg = icmp::build_echo_request(self.icmp_id, seq, b"MuKernel!");
        let total_len = 20 + icmp_msg.len() as u16;
        let ip_hdr = ipv4::build_header(ipv4::PROTO_ICMP, self.ip, dst, total_len, self.ip_id);
        self.ip_id = self.ip_id.wrapping_add(1);

        let mut payload = Vec::with_capacity(20 + icmp_msg.len());
        payload.extend_from_slice(&ip_hdr);
        payload.extend_from_slice(&icmp_msg);
        self.send_frame(dst_mac, ipv4::ETHERTYPE_IPV4, &payload);

        // RX 폴링: ICMP Echo Reply 대기
        let deadline = tick() + 18;
        loop {
            if let Some(frame) = self.nic.try_recv() {
                if let Some(pkt) = self.parse_frame(frame) {
                    if let Packet::IcmpEchoReply { id, seq: reply_seq, .. } = pkt {
                        if id == self.icmp_id && reply_seq == seq {
                            return Some(seq);
                        }
                    }
                }
            }
            if tick() > deadline { return None; }
            pause();
        }
    }

    // ── UDP 송신 ──────────────────────────────────────────────────────────────

    /// UDP 데이터그램 전송. ARP resolve 포함.
    pub fn udp_send(
        &mut self,
        dst_ip:   [u8; 4],
        dst_port: u16,
        src_port: u16,
        data:     &[u8],
    ) -> bool {
        let Some(dst_mac) = self.arp_resolve(dst_ip) else { return false; };

        let udp_seg = udp::build(src_port, dst_port, data);
        let total_len = 20 + udp_seg.len() as u16;
        let ip_hdr = ipv4::build_header(ipv4::PROTO_UDP, self.ip, dst_ip, total_len, self.ip_id);
        self.ip_id = self.ip_id.wrapping_add(1);

        let mut payload = Vec::with_capacity(20 + udp_seg.len());
        payload.extend_from_slice(&ip_hdr);
        payload.extend_from_slice(&udp_seg);
        self.send_frame(dst_mac, ipv4::ETHERTYPE_IPV4, &payload)
    }

    // ── 내부 헬퍼 ────────────────────────────────────────────────────────────

    fn send_frame(&mut self, dst_mac: [u8; 6], ethertype: u16, payload: &[u8]) -> bool {
        let mut frame = Vec::with_capacity(14 + payload.len());
        frame.extend_from_slice(&dst_mac);
        frame.extend_from_slice(&self.mac);
        frame.push((ethertype >> 8) as u8);
        frame.push(ethertype as u8);
        frame.extend_from_slice(payload);
        self.nic.send(&frame)
    }

    /// Ethernet 프레임을 파싱하고 필요 시 자동 응답 (ARP request → reply).
    fn parse_frame(&mut self, frame: Vec<u8>) -> Option<Packet> {
        if frame.len() < 14 { return None; }
        let ethertype = ((frame[12] as u16) << 8) | frame[13] as u16;

        match ethertype {
            // ── ARP ──────────────────────────────────────────────────────────
            arp::ETHERTYPE_ARP => {
                let pkt = arp::parse(&frame)?;
                if pkt.op == 1 && pkt.target_ip == self.ip {
                    // ARP request for our IP → send reply
                    let reply = arp::build_reply(
                        self.mac, self.ip,
                        pkt.sender_mac, pkt.sender_ip,
                    );
                    self.nic.send(&reply);
                } else if pkt.op == 2 {
                    self.arp_cache.insert(pkt.sender_ip, pkt.sender_mac);
                }
                None // ARP는 상위 레이어 Packet으로 올리지 않음
            }

            // ── IPv4 ─────────────────────────────────────────────────────────
            ipv4::ETHERTYPE_IPV4 => {
                let ip_data = &frame[14..];
                let (proto, src_ip, _dst, ihl) = ipv4::parse_header(ip_data)?;
                let payload = &ip_data[ihl..];

                match proto {
                    ipv4::PROTO_ICMP => {
                        let (id, seq) = icmp::parse_echo_reply(payload)?;
                        Some(Packet::IcmpEchoReply { src: src_ip, id, seq })
                    }
                    ipv4::PROTO_UDP => {
                        let (src_port, dst_port, data) = udp::parse(payload)?;
                        Some(Packet::Udp {
                            src: src_ip,
                            src_port,
                            dst_port,
                            data: data.to_vec(),
                        })
                    }
                    _ => None,
                }
            }

            _ => None,
        }
    }

    /// Non-blocking poll: 수신 프레임 1개 처리 후 상위 레이어 패킷이 있으면 반환.
    pub fn poll(&mut self) -> Option<Packet> {
        let frame = self.nic.try_recv()?;
        self.parse_frame(frame)
    }
}

#[inline] fn tick() -> u64 {
    crate::interrupts::handlers::TICK.load(Ordering::Relaxed)
}

#[inline] fn pause() {
    unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
}
