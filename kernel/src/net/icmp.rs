/// ICMP Echo Request / Reply (IPv4)
///
/// ICMP over IPv4, Protocol = 1.
/// Echo Request: type=8, code=0
/// Echo Reply:   type=0, code=0

use alloc::vec::Vec;
use super::checksum::internet_checksum;

pub const TYPE_ECHO_REPLY:   u8 = 0;
pub const TYPE_ECHO_REQUEST: u8 = 8;

/// ICMP Echo Request 메시지(헤더+페이로드) 빌드. 체크섬 자동 계산.
pub fn build_echo_request(id: u16, seq: u16, payload: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(8 + payload.len());
    msg.push(TYPE_ECHO_REQUEST); // type
    msg.push(0);                 // code
    msg.push(0); msg.push(0);   // checksum placeholder
    msg.push((id  >> 8) as u8); msg.push(id  as u8);
    msg.push((seq >> 8) as u8); msg.push(seq as u8);
    msg.extend_from_slice(payload);
    let csum = internet_checksum(&msg);
    msg[2] = (csum >> 8) as u8;
    msg[3] = csum as u8;
    msg
}

/// ICMP Echo Reply 파싱. type=0 이면 (id, seq) 반환.
pub fn parse_echo_reply(data: &[u8]) -> Option<(u16, u16)> {
    if data.len() < 8 { return None; }
    if data[0] != TYPE_ECHO_REPLY { return None; }
    let id  = ((data[4] as u16) << 8) | data[5] as u16;
    let seq = ((data[6] as u16) << 8) | data[7] as u16;
    Some((id, seq))
}
