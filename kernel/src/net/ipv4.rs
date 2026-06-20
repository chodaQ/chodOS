/// IPv4 헤더 빌더 / 파서 (options 없음, IHL=5, 20 bytes)

use super::checksum::internet_checksum;

pub const PROTO_ICMP: u8 = 1;
pub const PROTO_UDP:  u8 = 17;
pub const PROTO_TCP:  u8 = 6;

pub const ETHERTYPE_IPV4: u16 = 0x0800;

/// IPv4 헤더(20 bytes)를 빌드하고 체크섬을 채워 반환.
///
/// `total_len` = IP 헤더(20) + 페이로드 길이.
pub fn build_header(
    proto:       u8,
    src:         [u8; 4],
    dst:         [u8; 4],
    total_len:   u16,
    id:          u16,
) -> [u8; 20] {
    let mut h = [0u8; 20];
    h[0] = 0x45;                          // version=4, IHL=5 (20 bytes)
    h[1] = 0x00;                          // DSCP/ECN
    h[2] = (total_len >> 8) as u8;
    h[3] = total_len as u8;
    h[4] = (id >> 8) as u8;
    h[5] = id as u8;
    h[6] = 0x40; h[7] = 0x00;            // flags=DF, frag_offset=0
    h[8] = 64;                            // TTL
    h[9] = proto;
    // h[10..12] = checksum (0 for calculation)
    h[12..16].copy_from_slice(&src);
    h[16..20].copy_from_slice(&dst);
    let csum = internet_checksum(&h);
    h[10] = (csum >> 8) as u8;
    h[11] = csum as u8;
    h
}

/// Ethernet 페이로드(IP 헤더 + 데이터)에서 (proto, src_ip, dst_ip, ihl)을 파싱.
///
/// `ihl`은 IP 헤더 길이(bytes). 페이로드는 `data[ihl..]`.
pub fn parse_header(data: &[u8]) -> Option<(u8, [u8; 4], [u8; 4], usize)> {
    if data.len() < 20 { return None; }
    if data[0] >> 4 != 4 { return None; } // IPv4만 처리
    let ihl = ((data[0] & 0xF) as usize) * 4;
    if data.len() < ihl { return None; }
    let proto = data[9];
    let mut src = [0u8; 4]; src.copy_from_slice(&data[12..16]);
    let mut dst = [0u8; 4]; dst.copy_from_slice(&data[16..20]);
    Some((proto, src, dst, ihl))
}
