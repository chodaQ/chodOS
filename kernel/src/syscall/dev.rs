//! BETA 14: /dev 가상 디바이스 파일시스템
//!
//! ## 지원 디바이스
//! - `/dev/null`  — 읽기: 0바이트, 쓰기: 버림
//! - `/dev/zero`  — 읽기: 0x00 무한, 쓰기: 버림
//! - `/dev/full`  — 쓰기: ENOSPC, 읽기: 0x00
//! - `/dev/random`, `/dev/urandom` — xorshift64 PRNG
//! - `/dev/tty`, `/dev/console` — stdin/stdout 터미널
//! - `/dev/pts/N` — pty slave (터미널로 동작)
//! - `/dev/stdin`, `/dev/stdout`, `/dev/stderr` — fd 0/1/2 매핑
//!
//! ## ioctl 지원 (터미널 fd + /dev/tty)
//! - TCGETS (0x5401): termios 읽기 (더미 데이터)
//! - TCSETS/W/F (0x5402~4): termios 쓰기 → 무시 (0)
//! - TIOCGWINSZ (0x5413): 윈도우 크기 (80×24)
//! - TIOCSWINSZ (0x5414): 윈도우 크기 설정 → 무시
//! - TIOCGPGRP (0x540F): 프로세스 그룹 ID
//! - TIOCSPGRP (0x5410): 프로세스 그룹 설정 → 무시
//! - FIONREAD (0x541B): 읽기 가능 바이트 수

use alloc::{string::String, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
use crate::process::handle::{Capability, Rights};

// ── DevKind ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub enum DevKind {
    Null,
    Zero,
    Full,
    Random,
    Tty,
    Pts(u32),
    Stdin,
    Stdout,
    Stderr,
}

// ── DevResource ───────────────────────────────────────────────────────────────

pub struct DevResource {
    pub kind: DevKind,
    pub path: String,
}

// ── PRNG (xorshift64) ─────────────────────────────────────────────────────────

static RNG: AtomicU64 = AtomicU64::new(0xdeadbeef_cafebabe);

fn rand_u64() -> u64 {
    // atomic xorshift64 (not cryptographic, just enough for /dev/urandom)
    let mut x = RNG.load(Ordering::Relaxed);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    RNG.store(x, Ordering::Relaxed);
    x
}

fn fill_random(buf: *mut u8, count: usize) {
    let mut i = 0usize;
    while i + 8 <= count {
        let v = rand_u64().to_le_bytes();
        unsafe { core::ptr::copy_nonoverlapping(v.as_ptr(), buf.add(i), 8); }
        i += 8;
    }
    if i < count {
        let v = rand_u64().to_le_bytes();
        unsafe { core::ptr::copy_nonoverlapping(v.as_ptr(), buf.add(i), count - i); }
    }
}

// ── 공개 API ─────────────────────────────────────────────────────────────────

/// `/dev/` 경로인지 확인.
pub fn is_dev(path: &str) -> bool {
    path == "/dev" || path.starts_with("/dev/")
}

/// `/dev/` 경로에서 DevKind 판별. None이면 미지원.
pub fn kind_of(path: &str) -> Option<DevKind> {
    match path {
        "/dev/null"               => Some(DevKind::Null),
        "/dev/zero"               => Some(DevKind::Zero),
        "/dev/full"               => Some(DevKind::Full),
        "/dev/random"             => Some(DevKind::Random),
        "/dev/urandom"            => Some(DevKind::Random),
        "/dev/tty"                => Some(DevKind::Tty),
        "/dev/console"            => Some(DevKind::Tty),
        "/dev/stdin"              => Some(DevKind::Stdin),
        "/dev/stdout"             => Some(DevKind::Stdout),
        "/dev/stderr"             => Some(DevKind::Stderr),
        p if p.starts_with("/dev/pts/") => {
            let n = p["/dev/pts/".len()..].parse::<u32>().unwrap_or(0);
            Some(DevKind::Pts(n))
        }
        _ => None,
    }
}

/// `/dev/` 경로 존재 여부.
pub fn exists(path: &str) -> bool {
    match path {
        "/dev" | "/dev/pts" => true,
        _ => kind_of(path).is_some(),
    }
}

/// `/dev/` 경로가 디렉토리인지.
pub fn is_dir(path: &str) -> bool {
    matches!(path, "/dev" | "/dev/pts")
}

/// `/dev` 디렉토리 목록.
pub fn list(path: &str) -> Vec<(String, bool)> {
    match path {
        "/dev" => alloc::vec![
            ("null".into(), false),
            ("zero".into(), false),
            ("full".into(), false),
            ("random".into(), false),
            ("urandom".into(), false),
            ("tty".into(), false),
            ("console".into(), false),
            ("stdin".into(), false),
            ("stdout".into(), false),
            ("stderr".into(), false),
            ("pts".into(), true),
        ],
        "/dev/pts" => alloc::vec![("0".into(), false)],
        _ => alloc::vec![],
    }
}

/// `/dev/` stat: (mode, size).  mode: S_IFCHR|0o666 for devices, S_IFDIR|0o755 for dirs.
pub fn stat(path: &str) -> Option<(u32, i64)> {
    if is_dir(path) { return Some((0o040755, 0)); }
    kind_of(path).map(|_| (0o020666u32, 0i64)) // S_IFCHR | 0o666
}

/// `/dev/` 파일 오픈 → fd 반환. None이면 ENOENT.
pub fn open(path: &str) -> Option<u32> {
    let kind = kind_of(path)?;
    let rights = Rights::READ | Rights::WRITE;
    let fd = crate::syscall::fd::with_table_pub(|t| {
        let cap = Capability::new(DevResource { kind, path: path.into() }, rights);
        t.insert(cap).id
    });
    crate::serial_println!("[dev] open {:?} → fd {}", path, fd);
    Some(fd)
}

/// fd가 DevResource인지 확인.
pub fn is_dev_fd(fd: u32) -> bool {
    crate::syscall::fd::with_table_pub(|t| t.get::<DevResource>(fd, Rights::NONE).is_some())
}

/// DevResource fd에서 DevKind 조회.
pub fn kind_of_fd(fd: u32) -> Option<DevKind> {
    crate::syscall::fd::with_table_pub(|t| t.get::<DevResource>(fd, Rights::NONE).map(|d| d.kind))
}

/// `/dev` fd 읽기.
pub fn read(fd: u32, buf: *mut u8, count: usize) -> i64 {
    let kind = match kind_of_fd(fd) { Some(k) => k, None => return super::EBADF };
    if count == 0 { return 0; }
    match kind {
        DevKind::Null | DevKind::Full => 0,
        DevKind::Zero => {
            unsafe { core::ptr::write_bytes(buf, 0, count); }
            count as i64
        }
        DevKind::Random => {
            fill_random(buf, count);
            count as i64
        }
        DevKind::Tty | DevKind::Pts(_) | DevKind::Stdin => {
            // 키보드에서 읽기
            let byte = crate::kbd::read_key_blocking();
            unsafe { *buf = byte; }
            1
        }
        DevKind::Stdout | DevKind::Stderr => super::EBADF,
    }
}

/// `/dev` fd 쓰기.
pub fn write(fd: u32, buf: *const u8, count: usize) -> i64 {
    let kind = match kind_of_fd(fd) { Some(k) => k, None => return super::EBADF };
    if count == 0 { return 0; }
    match kind {
        DevKind::Null | DevKind::Zero | DevKind::Random => count as i64, // 버림
        DevKind::Full => -28, // ENOSPC
        DevKind::Tty | DevKind::Pts(_) | DevKind::Stdout | DevKind::Stderr => {
            let bytes = unsafe { core::slice::from_raw_parts(buf, count) };
            for &b in bytes { crate::serial::write_byte(b); }
            crate::term::write_bytes(bytes);
            count as i64
        }
        DevKind::Stdin => super::EBADF,
    }
}

/// `/dev` fd fstat.
pub fn fstat(fd: u32, stat_vaddr: u64) -> i64 {
    let kind = match kind_of_fd(fd) { Some(k) => k, None => return super::EBADF };
    let mode: u32 = match kind {
        _ => 0o020666, // S_IFCHR | 0o666
    };
    crate::syscall::fs::fill_stat_pub(stat_vaddr, 0, false);
    // 디바이스 모드로 덮어쓰기
    unsafe { *(stat_vaddr as *mut u32).add(4) = mode; } // st_mode offset
    0
}

// ── ioctl ─────────────────────────────────────────────────────────────────────

// termios 상수
const TCGETS:      u64 = 0x5401;
const TCSETS:      u64 = 0x5402;
const TCSETSW:     u64 = 0x5403;
const TCSETSF:     u64 = 0x5404;
const TIOCGPGRP:   u64 = 0x540F;
const TIOCSPGRP:   u64 = 0x5410;
const TIOCGWINSZ:  u64 = 0x5413;
const TIOCSWINSZ:  u64 = 0x5414;
const FIONREAD:    u64 = 0x541B;
const FIONBIO:     u64 = 0x5421;
const FIOCLEX:     u64 = 0x5451;
const FIONCLEX:    u64 = 0x5450;
const TIOCGPTN:    u64 = 0x80045430;
const TIOCSPTLCK:  u64 = 0x40045431;

/// termios 구조체 (Linux x86-64, 36 bytes).
///
/// 합리적인 기본값으로 채움 — isatty() / tcgetattr()가 성공하게 만듦.
fn fill_termios(ptr: u64) {
    if ptr == 0 { return; }
    // c_iflag: ICRNL|IXON = 0x500
    // c_oflag: OPOST|ONLCR = 0x05
    // c_cflag: B38400|CS8|CREAD|HUPCL = 0x00001CB2  (B38400=4097<<4? no)
    // Simpler: use values from a real Linux tty
    let termios: [u8; 36] = [
        // c_iflag (u32 LE): ICRNL(0x100)|IXON(0x400) = 0x0500
        0x00, 0x05, 0x00, 0x00,
        // c_oflag (u32 LE): OPOST(1)|ONLCR(4) = 0x05
        0x05, 0x00, 0x00, 0x00,
        // c_cflag (u32 LE): B38400(0xF<<8=0xF00? no, B38400=15<<9?
        // Use 0x4BF: CS8|CREAD|HUPCL (no baud, glibc reads it)
        0xBF, 0x04, 0x00, 0x00,
        // c_lflag (u32 LE): ISIG|ICANON|ECHO|ECHOE|ECHOK|IEXTEN = 0x8A3B
        0x3B, 0x8A, 0x00, 0x00,
        // c_line (u8): N_TTY = 0
        0x00,
        // c_cc[19]: VINTR=^C(3), VQUIT=^\(28), VERASE=DEL(127), VKILL=^U(21),
        //           VEOF=^D(4), VTIME=0, VMIN=1, VSWTC=0,
        //           VSTART=^Q(17), VSTOP=^S(19), VSUSP=^Z(26),
        //           VEOL=0, VREPRINT=^R(18), VDISCARD=^O(15),
        //           VWERASE=^W(23), VLNEXT=^V(22), VEOL2=0, 0, 0
        3, 28, 127, 21, 4, 0, 1, 0, 17, 19, 26, 0, 18, 15, 23, 22, 0, 0, 0,
    ];
    unsafe { core::ptr::copy_nonoverlapping(termios.as_ptr(), ptr as *mut u8, 36); }
}

/// winsize 구조체 채우기 (80×24).
fn fill_winsize(ptr: u64) {
    if ptr == 0 { return; }
    // ws_row(u16), ws_col(u16), ws_xpixel(u16), ws_ypixel(u16)
    let ws: [u16; 4] = [24, 80, 0, 0];
    unsafe {
        core::ptr::copy_nonoverlapping(ws.as_ptr() as *const u8, ptr as *mut u8, 8);
    }
}

/// ioctl 처리 — fd 0/1/2 및 DevResource fd.
///
/// fd가 0/1/2인 경우도 터미널로 취급 (isatty 지원).
pub fn sys_ioctl(fd: u64, request: u64, arg: u64) -> i64 {
    // fd 0/1/2: 터미널로 가정
    let is_tty_fd = fd <= 2
        || kind_of_fd(fd as u32).map(|k| matches!(k,
            DevKind::Tty | DevKind::Pts(_) |
            DevKind::Stdin | DevKind::Stdout | DevKind::Stderr
        )).unwrap_or(false);

    // 항상 0 반환 (ignore) 계열
    match request {
        TCSETS | TCSETSW | TCSETSF => return 0,
        TIOCSWINSZ => return 0,
        TIOCSPGRP  => return 0,
        FIONBIO | FIOCLEX | FIONCLEX => return 0,
        TIOCSPTLCK => return 0,
        _ => {}
    }

    if is_tty_fd {
        match request {
            TCGETS => {
                fill_termios(arg);
                return 0;
            }
            TIOCGWINSZ => {
                fill_winsize(arg);
                return 0;
            }
            TIOCGPGRP => {
                if arg != 0 {
                    unsafe { *(arg as *mut u32) = crate::process::userproc::current_pid() as u32; }
                }
                return 0;
            }
            FIONREAD => {
                if arg != 0 { unsafe { *(arg as *mut u32) = 0; } }
                return 0;
            }
            TIOCGPTN => {
                if arg != 0 { unsafe { *(arg as *mut u32) = 0; } }
                return 0;
            }
            _ => {
                crate::serial_println!("[dev] ioctl fd={} req={:#x} → 0 (stub)", fd, request);
                return 0; // 알 수 없는 tty ioctl → 무시
            }
        }
    }

    // 비-터미널 fd에 터미널 ioctl → ENOTTY
    match request {
        TCGETS | TIOCGWINSZ | TIOCGPGRP | FIONREAD => super::ENOTTY,
        // 파일 관련 ioctl은 성공
        FIONBIO | FIOCLEX | FIONCLEX => 0,
        _ => super::ENOTTY,
    }
}
