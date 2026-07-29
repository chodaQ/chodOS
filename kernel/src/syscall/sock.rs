//! BETA 3: socket syscall 스텁
//!
//! 커널 내부에 virtio-net + IPv4 스택(ARP/ICMP/UDP)이 있지만
//! 유저스페이스 소켓 API는 아직 연결되지 않았음. TCP는 미구현.
//! 이 파일은 musl/glibc 프로그램이 네트워크 syscall을 호출할 때
//! 크래시하지 않도록 의미 있는 errno를 반환한다.

// ── 소켓 errno ────────────────────────────────────────────────────────────────

const EAFNOSUPPORT: i64  = -97;   // Address family not supported
const EPROTONOSUPPORT: i64 = -93; // Protocol not supported
const ECONNREFUSED: i64  = -111;  // Connection refused
const ENOTCONN: i64      = -107;  // Transport endpoint not connected
const EOPNOTSUPP: i64    = -95;   // Operation not supported

// ── 가짜 소켓 fd 풀 ──────────────────────────────────────────────────────────

use core::sync::atomic::{AtomicI32, Ordering};
static SOCK_NEXT_FD: AtomicI32 = AtomicI32::new(20); // fd 20번 이상 사용

pub fn is_sock_fd(fd: i32) -> bool {
    fd >= 20
}

// ── socket 구현 ──────────────────────────────────────────────────────────────

/// 41: socket(domain, type, protocol) → 가짜 소켓 fd
///
/// AF_INET(2) / AF_INET6(10) / AF_UNIX(1) 모두 허용.
/// 추후 유저스페이스 TCP/UDP 연결 시 실제 구현으로 교체.
pub fn sys_socket(domain: u64, sock_type: u64, _protocol: u64) -> i64 {
    // AF_UNIX = 1, AF_INET = 2, AF_INET6 = 10 허용
    match domain {
        1 | 2 | 10 => {}
        _ => return EAFNOSUPPORT,
    }
    // SOCK_STREAM = 1, SOCK_DGRAM = 2, SOCK_RAW = 3 (SOCK_NONBLOCK = 0x800 마스크 제거)
    let base_type = sock_type & !0x800;
    match base_type {
        1 | 2 | 3 => {}
        _ => return EPROTONOSUPPORT,
    }
    let fd = SOCK_NEXT_FD.fetch_add(1, Ordering::Relaxed);
    crate::serial_println!("[sock] socket(domain={}, type={}) → fd {}", domain, sock_type, fd);
    fd as i64
}

/// 42: connect(sockfd, addr*, addrlen) → ECONNREFUSED
/// 유저스페이스 TCP 미구현 — ECONNREFUSED 반환
pub fn sys_connect(sockfd: u64, _addr: u64, _addrlen: u64) -> i64 {
    crate::serial_println!("[sock] connect(fd={}) → ECONNREFUSED (no userspace TCP)", sockfd);
    ECONNREFUSED
}

/// 43: accept(sockfd, addr*, addrlen*) → EOPNOTSUPP
pub fn sys_accept(_sockfd: u64, _addr: u64, _addrlen: u64) -> i64 {
    EOPNOTSUPP
}

/// 49: bind(sockfd, addr*, addrlen) → 0 (stub)
pub fn sys_bind(_sockfd: u64, _addr: u64, _addrlen: u64) -> i64 { 0 }

/// 50: listen(sockfd, backlog) → 0 (stub)
pub fn sys_listen(_sockfd: u64, _backlog: u64) -> i64 { 0 }

/// 51: getsockname(sockfd, addr*, addrlen*) → 0, 주소 0으로 초기화
pub fn sys_getsockname(_sockfd: u64, addr: u64, addrlen: u64) -> i64 {
    if addr != 0 && addrlen != 0 {
        let len = unsafe { *(addrlen as *const u32) } as usize;
        unsafe { core::ptr::write_bytes(addr as *mut u8, 0, len.min(128)); }
    }
    0
}

/// 52: getpeername(sockfd, addr*, addrlen*) → ENOTCONN
pub fn sys_getpeername(_sockfd: u64, _addr: u64, _addrlen: u64) -> i64 {
    ENOTCONN
}

/// 44: sendto(sockfd, buf*, len, flags, dest_addr*, addrlen) → ENOTCONN
pub fn sys_sendto(_sockfd: u64, _buf: u64, _len: u64,
                  _flags: u64, _addr: u64, _addrlen: u64) -> i64 {
    ENOTCONN
}

/// 45: recvfrom(sockfd, buf*, len, flags, src_addr*, addrlen*) → ENOTCONN
pub fn sys_recvfrom(_sockfd: u64, _buf: u64, _len: u64,
                    _flags: u64, _addr: u64, _addrlen: u64) -> i64 {
    ENOTCONN
}

/// 46: sendmsg(sockfd, msghdr*, flags) → ENOTCONN
pub fn sys_sendmsg(_sockfd: u64, _msg: u64, _flags: u64) -> i64 { ENOTCONN }

/// 47: recvmsg(sockfd, msghdr*, flags) → ENOTCONN
pub fn sys_recvmsg(_sockfd: u64, _msg: u64, _flags: u64) -> i64 { ENOTCONN }

/// 48: shutdown(sockfd, how) → 0
pub fn sys_shutdown(_sockfd: u64, _how: u64) -> i64 { 0 }

/// 54: setsockopt(sockfd, level, optname, optval*, optlen) → 0
pub fn sys_setsockopt(_sockfd: u64, _level: u64, _optname: u64,
                      _optval: u64, _optlen: u64) -> i64 { 0 }

/// 55: getsockopt(sockfd, level, optname, optval*, optlen*) → 0
pub fn sys_getsockopt(_sockfd: u64, level: u64, optname: u64,
                      optval: u64, optlen: u64) -> i64 {
    // SO_ERROR(4), SO_TYPE(3) 등 → 0 반환
    if optval != 0 && optlen != 0 {
        let len = unsafe { *(optlen as *const u32) } as usize;
        unsafe { core::ptr::write_bytes(optval as *mut u8, 0, len.min(128)); }
    }
    let _ = (level, optname);
    0
}
