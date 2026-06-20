/// ARP (Address Resolution Protocol)
///
/// Ethernet 프레임 전체(42 bytes)를 직접 빌드/파싱.
/// EtherType = 0x0806.

pub const ETHERTYPE_ARP: u16 = 0x0806;

// ── ARP 파싱 결과 ─────────────────────────────────────────────────────────────

pub struct ArpPacket {
    pub op:         u16,    // 1=request, 2=reply
    pub sender_mac: [u8; 6],
    pub sender_ip:  [u8; 4],
    pub target_mac: [u8; 6],
    pub target_ip:  [u8; 4],
}

/// Ethernet 프레임에서 ARP 파싱 (EtherType 확인 포함)
pub fn parse(frame: &[u8]) -> Option<ArpPacket> {
    if frame.len() < 42 { return None; }
    let etype = ((frame[12] as u16) << 8) | frame[13] as u16;
    if etype != ETHERTYPE_ARP { return None; }
    // ARP payload starts at frame[14]
    let a = &frame[14..];
    let op = ((a[6] as u16) << 8) | a[7] as u16;
    let mut sender_mac = [0u8; 6]; sender_mac.copy_from_slice(&a[8..14]);
    let mut sender_ip  = [0u8; 4]; sender_ip.copy_from_slice(&a[14..18]);
    let mut target_mac = [0u8; 6]; target_mac.copy_from_slice(&a[18..24]);
    let mut target_ip  = [0u8; 4]; target_ip.copy_from_slice(&a[24..28]);
    Some(ArpPacket { op, sender_mac, sender_ip, target_mac, target_ip })
}

// ── ARP 프레임 빌더 ───────────────────────────────────────────────────────────

fn build_frame(
    dst_mac:    [u8; 6],
    src_mac:    [u8; 6],
    op:         u16,
    sender_mac: [u8; 6],
    sender_ip:  [u8; 4],
    target_mac: [u8; 6],
    target_ip:  [u8; 4],
) -> [u8; 42] {
    let mut f = [0u8; 42];
    // Ethernet header
    f[0..6].copy_from_slice(&dst_mac);
    f[6..12].copy_from_slice(&src_mac);
    f[12] = 0x08; f[13] = 0x06;
    // ARP payload
    f[14] = 0x00; f[15] = 0x01; // hw_type = Ethernet
    f[16] = 0x08; f[17] = 0x00; // proto = IPv4
    f[18] = 6;                   // hw_size
    f[19] = 4;                   // proto_size
    f[20] = (op >> 8) as u8; f[21] = op as u8;
    f[22..28].copy_from_slice(&sender_mac);
    f[28..32].copy_from_slice(&sender_ip);
    f[32..38].copy_from_slice(&target_mac);
    f[38..42].copy_from_slice(&target_ip);
    f
}

/// ARP request: who has `target_ip`? tell `src_ip` (src_mac)
pub fn build_request(src_mac: [u8; 6], src_ip: [u8; 4], target_ip: [u8; 4]) -> [u8; 42] {
    build_frame(
        [0xFF; 6], src_mac,
        1,
        src_mac, src_ip,
        [0u8; 6], target_ip,
    )
}

/// ARP reply: `src_ip` is at `src_mac`, directed to `dst_mac`/`dst_ip`
pub fn build_reply(
    src_mac: [u8; 6], src_ip: [u8; 4],
    dst_mac: [u8; 6], dst_ip: [u8; 4],
) -> [u8; 42] {
    build_frame(dst_mac, src_mac, 2, src_mac, src_ip, dst_mac, dst_ip)
}

// ── ARP 캐시 테이블 ───────────────────────────────────────────────────────────

pub struct ArpTable {
    entries: [([u8; 4], [u8; 6]); 16],
    len:     usize,
}

impl ArpTable {
    pub const fn new() -> Self {
        Self { entries: [([0u8; 4], [0u8; 6]); 16], len: 0 }
    }

    pub fn lookup(&self, ip: [u8; 4]) -> Option<[u8; 6]> {
        for i in 0..self.len {
            if self.entries[i].0 == ip { return Some(self.entries[i].1); }
        }
        None
    }

    pub fn insert(&mut self, ip: [u8; 4], mac: [u8; 6]) {
        for i in 0..self.len {
            if self.entries[i].0 == ip {
                self.entries[i].1 = mac;
                return;
            }
        }
        if self.len < 16 {
            self.entries[self.len] = (ip, mac);
            self.len += 1;
        } else {
            // 가장 오래된 항목 덮어쓰기 (FIFO eviction)
            self.entries.rotate_left(1);
            self.entries[15] = (ip, mac);
        }
    }
}
