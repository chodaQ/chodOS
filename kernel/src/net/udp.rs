/// UDP 헤더 빌더 / 파서
///
/// 체크섬은 0 (IPv4에서는 선택 사항).

use alloc::vec::Vec;

/// UDP 데이터그램(헤더+페이로드) 빌드.
pub fn build(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let length = (8 + payload.len()) as u16;
    let mut seg = Vec::with_capacity(8 + payload.len());
    seg.push((src_port >> 8) as u8); seg.push(src_port as u8);
    seg.push((dst_port >> 8) as u8); seg.push(dst_port as u8);
    seg.push((length  >> 8) as u8);  seg.push(length as u8);
    seg.push(0); seg.push(0);        // checksum = 0 (optional in IPv4)
    seg.extend_from_slice(payload);
    seg
}

/// UDP 데이터그램 파싱. (src_port, dst_port, payload) 반환.
pub fn parse(data: &[u8]) -> Option<(u16, u16, &[u8])> {
    if data.len() < 8 { return None; }
    let src  = ((data[0] as u16) << 8) | data[1] as u16;
    let dst  = ((data[2] as u16) << 8) | data[3] as u16;
    let len  = ((data[4] as u16) << 8) | data[5] as u16;
    let plen = (len as usize).checked_sub(8)?;
    if data.len() < 8 + plen { return None; }
    Some((src, dst, &data[8..8 + plen]))
}
