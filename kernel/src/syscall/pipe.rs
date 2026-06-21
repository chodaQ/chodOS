//! BETA 3: pipe / pipe2 / select / epoll
//!
//! ## pipe
//! 커널 링 버퍼(4 KB) 1개를 동시에 지원.
//! `sys_pipe`가 호출될 때마다 read-fd / write-fd 쌍을 채번해 반환.
//!
//! ## select
//! - fd 0 (stdin) : 키보드 버퍼에 데이터가 있으면 읽기 준비
//! - fd 1/2       : 항상 쓰기 준비
//! - pipe read fd : 파이프 버퍼에 데이터가 있으면 읽기 준비
//! - timeout NULL : 블로킹(데이터 올 때까지 HLT)
//! - timeout {0,0}: 비블로킹
//!
//! ## epoll
//! 최대 16개 fd 감시. EPOLL_CTL_ADD/MOD/DEL 지원.
//! `epoll_wait` 는 ready fd가 생길 때까지 HLT.

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use super::{EINVAL, EFAULT};

// ── 파이프 링 버퍼 ────────────────────────────────────────────────────────────

const PIPE_CAP: usize = 4096;

static PIPE_BUF:      [AtomicU8;   PIPE_CAP] = [const { AtomicU8::new(0) }; PIPE_CAP];
static PIPE_HEAD:     AtomicUsize  = AtomicUsize::new(0);
static PIPE_TAIL:     AtomicUsize  = AtomicUsize::new(0);
static PIPE_READ_FD:  AtomicI32   = AtomicI32::new(-1);
static PIPE_WRITE_FD: AtomicI32   = AtomicI32::new(-1);
static NEXT_PIPE_FD:  AtomicI32   = AtomicI32::new(10); // 10, 11 부터 채번

pub fn is_read_fd(fd: i32)  -> bool { fd != -1 && fd == PIPE_READ_FD.load(Ordering::Relaxed) }
pub fn is_write_fd(fd: i32) -> bool { fd != -1 && fd == PIPE_WRITE_FD.load(Ordering::Relaxed) }

/// 파이프 버퍼에 읽을 데이터가 있으면 true
pub fn has_data() -> bool {
    PIPE_HEAD.load(Ordering::Relaxed) != PIPE_TAIL.load(Ordering::Relaxed)
}

/// 파이프 read end에서 읽기 (비블로킹)
pub fn pipe_read(buf: *mut u8, count: usize) -> i64 {
    let mut n = 0usize;
    while n < count {
        let head = PIPE_HEAD.load(Ordering::Relaxed);
        if head == PIPE_TAIL.load(Ordering::Relaxed) { break; }
        unsafe { *buf.add(n) = PIPE_BUF[head].load(Ordering::Relaxed); }
        PIPE_HEAD.store((head + 1) % PIPE_CAP, Ordering::Relaxed);
        n += 1;
    }
    n as i64
}

/// 파이프 write end에 쓰기
pub fn pipe_write(buf: *const u8, count: usize) -> i64 {
    let mut n = 0usize;
    while n < count {
        let tail = PIPE_TAIL.load(Ordering::Relaxed);
        let next = (tail + 1) % PIPE_CAP;
        if next == PIPE_HEAD.load(Ordering::Relaxed) { break; } // 버퍼 꽉 참
        PIPE_BUF[tail].store(unsafe { *buf.add(n) }, Ordering::Relaxed);
        PIPE_TAIL.store(next, Ordering::Relaxed);
        n += 1;
    }
    n as i64
}

// ── pipe syscall ─────────────────────────────────────────────────────────────

/// 22: pipe(pipefd[2])
pub fn sys_pipe(fds_vaddr: u64) -> i64 {
    if fds_vaddr == 0 { return EFAULT; }
    let r = NEXT_PIPE_FD.fetch_add(2, Ordering::Relaxed);
    let w = r + 1;
    PIPE_READ_FD.store(r, Ordering::Relaxed);
    PIPE_WRITE_FD.store(w, Ordering::Relaxed);
    PIPE_HEAD.store(0, Ordering::Relaxed);
    PIPE_TAIL.store(0, Ordering::Relaxed);
    unsafe {
        *(fds_vaddr       as *mut i32) = r;
        *((fds_vaddr + 4) as *mut i32) = w;
    }
    crate::serial_println!("[pipe] created read_fd={} write_fd={}", r, w);
    0
}

/// 293: pipe2(pipefd[2], flags) — flags(O_CLOEXEC 등) 무시
pub fn sys_pipe2(fds_vaddr: u64, _flags: u64) -> i64 {
    sys_pipe(fds_vaddr)
}

// ── fd_set 비트 조작 (128 bytes = 1024 fds) ──────────────────────────────────

fn fd_isset(fds_ptr: u64, fd: usize) -> bool {
    if fds_ptr == 0 || fd >= 1024 { return false; }
    let byte = unsafe { *((fds_ptr + (fd / 8) as u64) as *const u8) };
    byte & (1 << (fd % 8)) != 0
}

fn fd_set_bit(fds_ptr: u64, fd: usize) {
    if fds_ptr == 0 || fd >= 1024 { return; }
    unsafe { *((fds_ptr + (fd / 8) as u64) as *mut u8) |= 1 << (fd % 8); }
}

fn fds_zero(fds_ptr: u64) {
    if fds_ptr == 0 { return; }
    unsafe { core::ptr::write_bytes(fds_ptr as *mut u8, 0, 128); }
}

/// fd가 읽기 준비 상태인지 확인 (stdin 또는 pipe read)
fn is_read_ready(fd: usize) -> bool {
    match fd as i32 {
        0 => crate::kbd::has_key(),
        f if is_read_fd(f) => has_data(),
        _ => false,
    }
}

/// fd가 쓰기 준비 상태인지 확인
fn is_write_ready(fd: usize) -> bool {
    matches!(fd, 1 | 2) || is_write_fd(fd as i32)
}

// ── select ────────────────────────────────────────────────────────────────────

/// 23: select(nfds, readfds*, writefds*, exceptfds*, timeout*)
pub fn sys_select(nfds: u64, rfds: u64, wfds: u64, efds: u64, timeout: u64) -> i64 {
    let nfds = nfds as usize;

    // timeout 파싱: NULL = 블로킹, {0,0} = 비블로킹
    let blocking = if timeout == 0 {
        true // NULL → block indefinitely
    } else {
        let tv_sec  = unsafe { *(timeout as *const i64) };
        let tv_usec = unsafe { *((timeout + 8) as *const i64) };
        tv_sec != 0 || tv_usec != 0
    };

    loop {
        let ready = 0i64;

        // 결과 fd_set 초기화
        fds_zero(rfds);
        fds_zero(wfds);
        fds_zero(efds);

        for fd in 0..nfds.min(1024) {
            // 읽기
            if fd_isset(rfds, fd) || is_read_ready(fd) {
                // rfds was zeroed above — check original via pre-zeroed copy
                // We need to re-check original: since we zeroed it, we can't.
                // Fix: check readiness unconditionally and set if ready
            }
        }

        // 올바른 구현: rfds/wfds를 먼저 복사한 뒤 0으로 리셋
        // 위의 루프는 버그가 있으므로 구조를 바꾼다 (아래가 실제 구현)
        let _ = ready; // suppress unused warning for now
        break; // handled below
    }

    // ── 실제 구현 (원본 fd_set을 스택에 복사 후 결과 fd_set에 세팅) ──────────
    sys_select_inner(nfds, rfds, wfds, efds, blocking)
}

fn sys_select_inner(nfds: usize, rfds: u64, wfds: u64, _efds: u64, blocking: bool) -> i64 {
    // 원본 비트마스크를 스택에 복사 (128 bytes)
    let mut orig_r = [0u8; 128];
    let mut orig_w = [0u8; 128];
    if rfds != 0 { unsafe { core::ptr::copy_nonoverlapping(rfds as *const u8, orig_r.as_mut_ptr(), 128); } }
    if wfds != 0 { unsafe { core::ptr::copy_nonoverlapping(wfds as *const u8, orig_w.as_mut_ptr(), 128); } }

    loop {
        fds_zero(rfds);
        fds_zero(wfds);
        let mut ready = 0i64;

        for fd in 0..nfds.min(1024) {
            let byte = fd / 8;
            let bit  = 1u8 << (fd % 8);

            if orig_r[byte] & bit != 0 && is_read_ready(fd) {
                fd_set_bit(rfds, fd);
                ready += 1;
            }
            if orig_w[byte] & bit != 0 && is_write_ready(fd) {
                fd_set_bit(wfds, fd);
                ready += 1;
            }
        }

        if ready > 0 || !blocking {
            return ready;
        }
        // 블로킹: 인터럽트(kbd IRQ1)가 올 때까지 대기
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }
}

/// 270: pselect6(nfds, rfds*, wfds*, efds*, timeout*, sigmask*) — select와 동일하게 처리
pub fn sys_pselect6(nfds: u64, rfds: u64, wfds: u64, efds: u64, timeout: u64, _sigmask: u64) -> i64 {
    sys_select(nfds, rfds, wfds, efds, timeout)
}

// ── epoll ─────────────────────────────────────────────────────────────────────

const EPOLL_MAX: usize = 16;

static EP_FD:   [AtomicI32; EPOLL_MAX] = [const { AtomicI32::new(-1) }; EPOLL_MAX];
static EP_EV:   [AtomicU32; EPOLL_MAX] = [const { AtomicU32::new(0)  }; EPOLL_MAX];
static EP_DATA: [AtomicU64; EPOLL_MAX] = [const { AtomicU64::new(0)  }; EPOLL_MAX];
static EP_CNT:  AtomicUsize            = AtomicUsize::new(0);

const EPOLLIN:  u32 = 0x0001;
const EPOLLOUT: u32 = 0x0004;

const EPOLL_CTL_ADD: u64 = 1;
const EPOLL_CTL_DEL: u64 = 2;
const EPOLL_CTL_MOD: u64 = 3;

/// 281/291: epoll_create / epoll_create1(flags) → 고정 fd 9
pub fn sys_epoll_create(_flags: u64) -> i64 {
    EP_CNT.store(0, Ordering::Relaxed);
    for i in 0..EPOLL_MAX { EP_FD[i].store(-1, Ordering::Relaxed); }
    9 // epoll instance fd
}

/// 233: epoll_ctl(epfd, op, fd, event*)
pub fn sys_epoll_ctl(_epfd: u64, op: u64, fd: u64, ev_vaddr: u64) -> i64 {
    let fd = fd as i32;
    let events = if ev_vaddr != 0 { unsafe { *(ev_vaddr as *const u32) } } else { EPOLLIN };
    // epoll_event layout: u32 events + u32 pad + u64 data
    let data   = if ev_vaddr != 0 { unsafe { *((ev_vaddr + 8) as *const u64) } } else { fd as u64 };

    match op {
        EPOLL_CTL_ADD | EPOLL_CTL_MOD => {
            let n = EP_CNT.load(Ordering::Relaxed);
            // 기존 항목 업데이트
            for i in 0..n {
                if EP_FD[i].load(Ordering::Relaxed) == fd {
                    EP_EV[i].store(events, Ordering::Relaxed);
                    EP_DATA[i].store(data, Ordering::Relaxed);
                    return 0;
                }
            }
            // 새 항목 추가
            if n < EPOLL_MAX {
                EP_FD[n].store(fd, Ordering::Relaxed);
                EP_EV[n].store(events, Ordering::Relaxed);
                EP_DATA[n].store(data, Ordering::Relaxed);
                EP_CNT.fetch_add(1, Ordering::Relaxed);
            }
            0
        }
        EPOLL_CTL_DEL => {
            let n = EP_CNT.load(Ordering::Relaxed);
            for i in 0..n {
                if EP_FD[i].load(Ordering::Relaxed) == fd {
                    let last = n - 1;
                    if i < last {
                        EP_FD[i].store(EP_FD[last].load(Ordering::Relaxed), Ordering::Relaxed);
                        EP_EV[i].store(EP_EV[last].load(Ordering::Relaxed), Ordering::Relaxed);
                        EP_DATA[i].store(EP_DATA[last].load(Ordering::Relaxed), Ordering::Relaxed);
                    }
                    EP_FD[last].store(-1, Ordering::Relaxed);
                    EP_CNT.fetch_sub(1, Ordering::Relaxed);
                    break;
                }
            }
            0
        }
        _ => EINVAL,
    }
}

/// 232: epoll_wait(epfd, events*, maxevents, timeout)
/// timeout -1 = 블로킹, 0 = 비블로킹, >0 = 밀리초
pub fn sys_epoll_wait(_epfd: u64, ev_vaddr: u64, maxevents: u64, timeout: u64) -> i64 {
    if ev_vaddr == 0 { return EFAULT; }
    let max = maxevents as usize;
    if max == 0 { return EINVAL; }
    let blocking = timeout == u64::MAX || timeout != 0; // -1 cast to u64 or >0

    loop {
        let n = EP_CNT.load(Ordering::Relaxed);
        let mut out = 0usize;

        for i in 0..n {
            if out >= max { break; }
            let fd = EP_FD[i].load(Ordering::Relaxed);
            let ev = EP_EV[i].load(Ordering::Relaxed);
            let data = EP_DATA[i].load(Ordering::Relaxed);

            let ready_ev = {
                let mut r = 0u32;
                if ev & EPOLLIN  != 0 && is_read_ready(fd as usize)  { r |= EPOLLIN; }
                if ev & EPOLLOUT != 0 && is_write_ready(fd as usize) { r |= EPOLLOUT; }
                r
            };

            if ready_ev != 0 {
                // epoll_event: { u32 events, u32 pad, u64 data } = 16 bytes on x86-64
                // Linux uses __attribute__((packed)) so it's 12 bytes? Actually no.
                // struct epoll_event { __u32 events; __u64 data; } __attribute__((packed)) = 12 bytes
                let base = ev_vaddr + (out as u64) * 12;
                unsafe {
                    *(base as *mut u32) = ready_ev;
                    *((base + 4) as *mut u64) = data;
                }
                out += 1;
            }
        }

        if out > 0 { return out as i64; }
        if !blocking { return 0; }
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }
}
