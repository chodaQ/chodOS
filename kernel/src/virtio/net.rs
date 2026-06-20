//! VirtIO Net 드라이버 (ALPHA 11)
//!
//! ## 큐 구조
//!
//! ```
//! Queue 0 (receiveq):  driver가 빈 버퍼 pre-post → device가 수신 패킷으로 채움
//! Queue 1 (transmitq): driver가 패킷 넣음       → device가 전송하고 used ring에 완료 표시
//! ```
//!
//! ## TX 디스크립터 체인
//!
//! ```text
//! Desc[0]: virtio_net_hdr (10 bytes, READ | NEXT)
//! Desc[1]: Ethernet frame (READ)
//! ```
//!
//! ## RX 디스크립터 (pre-posted)
//!
//! ```text
//! Desc[i]: 1524-byte buffer (hdr + max-frame, WRITE)
//! ```
//!
//! ## QEMU 네트워크 (user-mode SLIRP)
//!
//! ```
//! Guest IP : 10.0.2.15
//! Gateway  : 10.0.2.2
//! DNS      : 10.0.2.3
//! ```

use alloc::{alloc::{alloc_zeroed, dealloc, Layout}, vec::Vec};
use core::sync::atomic::{fence, Ordering};

use crate::pci;
use super::queue::Queue;

// ── 상수 ────────────────────────────────────────────────────────────────────

const VIRTIO_VENDOR:     u16 = 0x1AF4;
const VIRTIO_NET_DEVICE: u16 = 0x1000;

const R_GUEST_FEATURES: u16 = 0x04;
const R_STATUS:         u16 = 0x12;
const R_MAC:            u16 = 0x14; // 6바이트 MAC

const STATUS_ACK:       u8 = 1;
const STATUS_DRIVER:    u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FAILED:    u8 = 128;

const VIRTQ_DESC_F_NEXT:  u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

/// VirtIO Net 헤더 크기 (legacy, MRG_RXBUF 없음)
const NET_HDR_LEN: usize = 10;
/// RX 버퍼 크기 = 헤더 + 최대 Ethernet 프레임
const RX_BUF_SIZE: usize = NET_HDR_LEN + 1514;
/// pre-post할 RX 버퍼 수
const RX_BUF_COUNT: usize = 8;

// ── VirtIO Net 헤더 ──────────────────────────────────────────────────────────

#[repr(C)]
struct NetHdr {
    flags:       u8,
    gso_type:    u8,
    hdr_len:     u16,
    gso_size:    u16,
    csum_start:  u16,
    csum_offset: u16,
}

// ── 드라이버 ─────────────────────────────────────────────────────────────────

pub struct VirtioNet {
    io_base:    u16,
    rxq:        Queue,
    txq:        Queue,
    pub mac:    [u8; 6],
    rx_bufs:    *mut u8,
    rx_layout:  Layout,
    rx_posted:  u16, // avail_idx when we finished pre-posting
}

unsafe impl Send for VirtioNet {}
unsafe impl Sync for VirtioNet {}

impl VirtioNet {
    pub fn init() -> Option<Self> {
        let (bus, dev) = pci::find_device(VIRTIO_VENDOR, VIRTIO_NET_DEVICE)?;
        crate::serial_println!("[virtio-net] found at PCI {:02x}:{:02x}", bus, dev);

        pci::enable_io_and_busmaster(bus, dev);
        let io_base = pci::bar_io_base(bus, dev, 0)?;
        crate::serial_println!("[virtio-net] BAR0 I/O = 0x{:04x}", io_base);

        unsafe {
            // 리셋 → ACK|DRIVER
            outb(io_base + R_STATUS, 0);
            outb(io_base + R_STATUS, STATUS_ACK | STATUS_DRIVER);
            outl(io_base + R_GUEST_FEATURES, 0); // feature 없음 (최소 동작)

            // MAC 주소 읽기
            let mut mac = [0u8; 6];
            for i in 0..6usize {
                mac[i] = pci::inb(io_base + R_MAC + i as u16);
            }
            crate::serial_println!(
                "[virtio-net] MAC = {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            );

            // 큐 0 (RX) 초기화
            let rxq = Queue::init(io_base, 0)?;
            crate::serial_println!("[virtio-net] rxq size={}", rxq.qsize);

            // 큐 1 (TX) 초기화
            let txq = Queue::init(io_base, 1)?;
            crate::serial_println!("[virtio-net] txq size={}", txq.qsize);

            // DRIVER_OK
            outb(io_base + R_STATUS, STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK);

            // RX 버퍼 힙 할당
            let rx_layout = Layout::from_size_align(RX_BUF_COUNT * RX_BUF_SIZE, 64).unwrap();
            let rx_bufs   = alloc_zeroed(rx_layout);
            if rx_bufs.is_null() {
                crate::serial_println!("[virtio-net] RX buf alloc failed");
                outb(io_base + R_STATUS, STATUS_FAILED);
                return None;
            }

            let mut net = VirtioNet {
                io_base, rxq, txq, mac, rx_bufs, rx_layout, rx_posted: 0,
            };

            // RX 버퍼 pre-post
            net.post_rx_all();

            Some(net)
        }
    }

    // ── RX ───────────────────────────────────────────────────────────────────

    /// RX 버퍼 전체를 큐에 등록
    fn post_rx_all(&mut self) {
        let hhdm = crate::memory::hhdm_offset();
        unsafe {
            for i in 0..RX_BUF_COUNT {
                let virt = self.rx_bufs as u64 + (i * RX_BUF_SIZE) as u64;
                let phys = virt - hhdm;
                let slot = self.rxq.ai as usize % self.rxq.qsize;
                self.rxq.write_desc(i, phys, RX_BUF_SIZE as u32, VIRTQ_DESC_F_WRITE, 0);
                self.rxq.avail_put(slot, i as u16);
                self.rxq.ai = self.rxq.ai.wrapping_add(1);
            }
            self.rxq.avail_commit(self.rxq.ai);
            self.rx_posted = self.rxq.ai;
        }
        // 큐 0 알림
        unsafe { pci::outw(self.io_base + 0x10, 0); }
        crate::serial_println!("[virtio-net] pre-posted {} RX buffers", RX_BUF_COUNT);
    }

    /// 수신된 패킷이 있으면 Ethernet 프레임(헤더 이후)을 반환.
    pub fn try_recv(&mut self) -> Option<Vec<u8>> {
        unsafe {
            fence(Ordering::SeqCst);
            let used_idx = self.rxq.used_idx();
            if used_idx == self.rxq.lu { return None; }

            let slot   = self.rxq.lu as usize % self.rxq.qsize;
            let buf_id = self.rxq.used_id(slot) as usize;
            let total  = self.rxq.used_len(slot) as usize;

            self.rxq.lu = self.rxq.lu.wrapping_add(1);

            // 헤더 이후가 실제 Ethernet 프레임
            let pkt_len = total.saturating_sub(NET_HDR_LEN);
            let pkt_ptr = self.rx_bufs.add(buf_id * RX_BUF_SIZE + NET_HDR_LEN);

            let mut pkt = Vec::with_capacity(pkt_len);
            for i in 0..pkt_len {
                pkt.push(*pkt_ptr.add(i));
            }

            // 버퍼 재사용: 해당 desc를 avail ring에 다시 등록
            let virt  = self.rx_bufs as u64 + (buf_id * RX_BUF_SIZE) as u64;
            let phys  = virt - crate::memory::hhdm_offset();
            let aslot = self.rxq.ai as usize % self.rxq.qsize;
            self.rxq.write_desc(buf_id, phys, RX_BUF_SIZE as u32, VIRTQ_DESC_F_WRITE, 0);
            self.rxq.avail_put(aslot, buf_id as u16);
            self.rxq.ai = self.rxq.ai.wrapping_add(1);
            fence(Ordering::SeqCst);
            self.rxq.avail_commit(self.rxq.ai);
            pci::outw(self.io_base + 0x10, 0); // 큐 0 알림

            Some(pkt)
        }
    }

    // ── TX ───────────────────────────────────────────────────────────────────

    /// Ethernet 프레임 전송 (헤더는 드라이버가 prepend).
    ///
    /// 2-descriptor 체인: [NetHdr | NEXT] → [frame]
    /// 완료까지 폴링 (최대 ~500ms).
    pub fn send(&mut self, frame: &[u8]) -> bool {
        if frame.is_empty() { return false; }
        unsafe {
            let hhdm = crate::memory::hhdm_offset();

            let hdr  = NetHdr { flags: 0, gso_type: 0, hdr_len: 0, gso_size: 0,
                                 csum_start: 0, csum_offset: 0 };
            let hdr_phys   = (&hdr as *const NetHdr as u64).wrapping_sub(hhdm);
            let frame_phys = (frame.as_ptr() as u64).wrapping_sub(hhdm);

            // Desc[d0]: 헤더
            let d0 = self.txq.nd as usize;
            self.txq.nd = (self.txq.nd + 1) % self.txq.qsize as u16;
            // Desc[d1]: 프레임
            let d1 = self.txq.nd as usize;
            self.txq.nd = (self.txq.nd + 1) % self.txq.qsize as u16;

            self.txq.write_desc(d0, hdr_phys, NET_HDR_LEN as u32, VIRTQ_DESC_F_NEXT, d1 as u16);
            self.txq.write_desc(d1, frame_phys, frame.len() as u32, 0, 0);

            let slot = self.txq.ai as usize % self.txq.qsize;
            self.txq.avail_put(slot, d0 as u16);
            self.txq.ai = self.txq.ai.wrapping_add(1);
            fence(Ordering::SeqCst);
            self.txq.avail_commit(self.txq.ai);

            // 큐 1 알림
            pci::outw(self.io_base + 0x10, 1);

            // 완료 폴링 (TX used ring 진행 확인)
            let deadline = crate::interrupts::handlers::TICK.load(Ordering::Relaxed) + 9;
            loop {
                fence(Ordering::SeqCst);
                if self.txq.used_idx() != self.txq.lu {
                    self.txq.lu = self.txq.lu.wrapping_add(1);
                    return true;
                }
                if crate::interrupts::handlers::TICK.load(Ordering::Relaxed) > deadline {
                    return false;
                }
                core::arch::asm!("pause", options(nomem, nostack));
            }
        }
    }
}

impl Drop for VirtioNet {
    fn drop(&mut self) {
        unsafe {
            outb(self.io_base + R_STATUS, 0);
            if !self.rx_bufs.is_null() {
                dealloc(self.rx_bufs, self.rx_layout);
            }
        }
    }
}

// ── ARP 패킷 빌더 ────────────────────────────────────────────────────────────

/// ARP request 프레임 (42 bytes) 빌드
///
/// ```
/// who has `dst_ip`? tell `src_ip` (src_mac)
/// ```
pub fn build_arp_request(
    src_mac: [u8; 6],
    src_ip:  [u8; 4],
    dst_ip:  [u8; 4],
) -> [u8; 42] {
    let mut f = [0u8; 42];
    // Ethernet header
    f[0..6].copy_from_slice(&[0xFF; 6]);          // dst: broadcast
    f[6..12].copy_from_slice(&src_mac);            // src
    f[12] = 0x08; f[13] = 0x06;                   // EtherType: ARP
    // ARP payload
    f[14] = 0x00; f[15] = 0x01;                   // hw_type: Ethernet
    f[16] = 0x08; f[17] = 0x00;                   // proto: IPv4
    f[18] = 0x06;                                  // hw_size
    f[19] = 0x04;                                  // proto_size
    f[20] = 0x00; f[21] = 0x01;                   // op: request
    f[22..28].copy_from_slice(&src_mac);           // sender MAC
    f[28..32].copy_from_slice(&src_ip);            // sender IP
    // target MAC = 0 (unknown)
    f[38..42].copy_from_slice(&dst_ip);            // target IP
    f
}

/// Ethernet 프레임 EtherType 추출
pub fn ethertype(frame: &[u8]) -> u16 {
    if frame.len() < 14 { return 0; }
    ((frame[12] as u16) << 8) | frame[13] as u16
}

/// ARP 응답에서 발신자 MAC + IP 추출
pub fn parse_arp_reply(frame: &[u8]) -> Option<([u8; 6], [u8; 4])> {
    if frame.len() < 42 { return None; }
    if ethertype(frame) != 0x0806 { return None; }
    let op = ((frame[20] as u16) << 8) | frame[21] as u16;
    if op != 2 { return None; } // op=2: ARP reply
    let mut mac = [0u8; 6]; mac.copy_from_slice(&frame[22..28]);
    let mut ip  = [0u8; 4]; ip.copy_from_slice(&frame[28..32]);
    Some((mac, ip))
}

// ── 포트 I/O ────────────────────────────────────────────────────────────────

#[inline] unsafe fn outb(p: u16, v: u8)  { pci::outb(p, v); }
#[inline] unsafe fn outl(p: u16, v: u32) { pci::outl(p, v); }
