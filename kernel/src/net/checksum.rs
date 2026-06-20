/// RFC 1071 인터넷 체크섬 (ones-complement sum의 ones-complement)
///
/// IP 헤더, ICMP, UDP 등 모든 인터넷 프로토콜 체크섬에 사용.
/// 체크섬 필드를 0으로 채운 상태로 계산해서 넣으면 됨.
pub fn internet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += ((data[i] as u32) << 8) | (data[i + 1] as u32);
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8; // 홀수 byte → 하위 byte = 0 padding
    }
    // carry fold: 최대 2번이면 수렴
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}
