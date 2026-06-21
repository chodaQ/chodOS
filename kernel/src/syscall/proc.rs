//! BETA 2-1 / BETA 3: 프로세스/스레드/시그널 syscall — Linux x86-64 ABI
#![allow(dead_code)]

use super::EINVAL;
use core::sync::atomic::{AtomicU64, Ordering};

// ── 시그널 테이블 (BETA 3) ────────────────────────────────────────────────────
//
// Linux x86-64 시그널 번호: 1=SIGHUP, 2=SIGINT, 9=SIGKILL, 15=SIGTERM ...
// SIG_DFL = 0, SIG_IGN = 1, 그 외 = 핸들러 가상주소

const NSIG: usize = 64;

// 핸들러 주소 (SIG_DFL=0, SIG_IGN=1, else=handler fn ptr)
static SIG_HANDLERS: [AtomicU64; NSIG] = [const { AtomicU64::new(0) }; NSIG];
// 블록된 시그널 마스크 (비트맵)
static SIG_MASK: AtomicU64 = AtomicU64::new(0);
// 대기 중인 시그널 (비트맵)
static SIG_PENDING: AtomicU64 = AtomicU64::new(0);

/// 시그널 핸들러를 가져온다 (SIG_DFL=0 포함)
pub fn sig_handler(sig: usize) -> u64 {
    if sig == 0 || sig >= NSIG { return 0; }
    SIG_HANDLERS[sig].load(Ordering::Relaxed)
}

/// 대기 중인 시그널을 한 개 꺼낸다 (번호 반환, 없으면 0)
pub fn sig_dequeue() -> usize {
    let pending = SIG_PENDING.load(Ordering::Relaxed);
    let mask    = SIG_MASK.load(Ordering::Relaxed);
    let deliver = pending & !mask; // 블록되지 않은 것
    if deliver == 0 { return 0; }
    let sig = deliver.trailing_zeros() as usize + 1;
    SIG_PENDING.fetch_and(!(1u64 << (sig - 1)), Ordering::Relaxed);
    sig
}

/// SIGINT(2) 전달 — kbd ETX(Ctrl+C) 처리 시 호출
pub fn deliver_sigint() {
    SIG_PENDING.fetch_or(1 << 1, Ordering::Relaxed); // bit 1 = SIGINT
}

// ── MSR helpers ───────────────────────────────────────────────────────────────

unsafe fn wrmsr(msr: u32, val: u64) {
    let lo = (val & 0xFFFF_FFFF) as u32;
    let hi = (val >> 32) as u32;
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr, in("eax") lo, in("edx") hi,
        options(nomem, nostack),
    );
}

unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32; let hi: u32;
    core::arch::asm!(
        "rdmsr",
        out("eax") lo, out("edx") hi, in("ecx") msr,
        options(nomem, nostack),
    );
    (hi as u64) << 32 | lo as u64
}

// ── utsname helper ────────────────────────────────────────────────────────────

fn copy_str(dst: &mut [u8], src: &[u8]) {
    let n = src.len().min(dst.len().saturating_sub(1));
    dst[..n].copy_from_slice(&src[..n]);
    if n < dst.len() { dst[n] = 0; }
}

// ── 구현 ──────────────────────────────────────────────────────────────────────

/// 24: sched_yield() → 스케줄러 양보
pub fn sys_sched_yield() -> i64 {
    unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    0
}

/// 35: nanosleep(req*, rem*) — spin 기반 근사
pub fn sys_nanosleep(req_vaddr: u64, _rem_vaddr: u64) -> i64 {
    if req_vaddr == 0 { return -14; } // EFAULT
    let (secs, nsecs) = unsafe {
        let s = *(req_vaddr as *const i64);
        let n = *((req_vaddr + 8) as *const i64);
        (s, n)
    };
    if secs < 0 || nsecs < 0 || nsecs >= 1_000_000_000 { return EINVAL; }
    // 각 pause ≈ 10ns, 최대 1초만 실제 spin (이상이면 skip)
    let ns_total = (secs.min(1) as u64) * 1_000_000_000 + nsecs as u64;
    let iters = ns_total / 10;
    for _ in 0..iters {
        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }
    0
}

/// 63: uname(utsname*) → MuKernel 커널 정보
pub fn sys_uname(buf_vaddr: u64) -> i64 {
    // struct utsname: 6 × 65 bytes = 390 bytes
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_vaddr as *mut u8, 390) };
    buf.fill(0);
    copy_str(&mut buf[  0.. 65], b"MuKernel");
    copy_str(&mut buf[ 65..130], b"mukernel");
    copy_str(&mut buf[130..195], b"0.1.0-BETA5");
    // version 필드: CPU 수 동적 반영
    {
        let ver = alloc::format!("#1 SMP MuKernel 2026 ({} core(s))", crate::smp::cpu_count());
        copy_str(&mut buf[195..260], ver.as_bytes());
    }
    copy_str(&mut buf[260..325], b"x86_64");
    copy_str(&mut buf[325..390], b"(none)");
    0
}

/// 102: getuid() → 항상 0 (root)
pub fn sys_getuid() -> i64 { 0 }

/// 104: getgid() → 항상 0 (root)
pub fn sys_getgid() -> i64 { 0 }

/// 107: geteuid() → 항상 0
pub fn sys_geteuid() -> i64 { 0 }

/// 108: getegid() → 항상 0
pub fn sys_getegid() -> i64 { 0 }

/// 110: getppid() → 0 (init)
pub fn sys_getppid() -> i64 { 0 }

/// 111: getpgrp() → 현재 pid
pub fn sys_getpgrp() -> i64 {
    crate::process::scheduler::current_pid() as i64
}

/// 112: setsid() → 현재 pid 반환
pub fn sys_setsid() -> i64 {
    crate::process::scheduler::current_pid() as i64
}

/// 135: personality(persona) → 항상 0 (PER_LINUX)
pub fn sys_personality(_persona: u64) -> i64 { 0 }

/// 158: arch_prctl(code, addr)
///
/// glibc/musl이 TLS 설정에 필수로 호출하는 syscall.
/// ARCH_SET_FS → MSR 0xC000_0100 (FS.Base) 에 addr 기록.
pub fn sys_arch_prctl(code: u64, addr: u64) -> i64 {
    const ARCH_SET_GS: u64 = 0x1001;
    const ARCH_SET_FS: u64 = 0x1002;
    const ARCH_GET_FS: u64 = 0x1003;
    const ARCH_GET_GS: u64 = 0x1004;
    const MSR_FS_BASE: u32 = 0xC000_0100;
    const MSR_GS_BASE: u32 = 0xC000_0101;

    match code {
        ARCH_SET_FS => {
            unsafe { wrmsr(MSR_FS_BASE, addr); }
            // BETA 11: UserProc에도 저장 (컨텍스트 스위치 시 복원용)
            let t   = crate::process::userproc::table_pub();
            let cur = crate::process::userproc::current_idx();
            if let Some(ref mut p) = t[cur] { p.fs_base = addr; }
            0
        }
        ARCH_GET_FS => {
            let val = unsafe { rdmsr(MSR_FS_BASE) };
            unsafe { *(addr as *mut u64) = val; }
            0
        }
        ARCH_SET_GS => { unsafe { wrmsr(MSR_GS_BASE, addr); } 0 }
        ARCH_GET_GS => {
            let val = unsafe { rdmsr(MSR_GS_BASE) };
            unsafe { *(addr as *mut u64) = val; }
            0
        }
        _ => EINVAL,
    }
}

/// 160: setrlimit(resource, rlimit*) → stub 0
pub fn sys_setrlimit(_res: u64, _lim_vaddr: u64) -> i64 { 0 }

/// 162: sync() → stub 0
pub fn sys_sync() -> i64 { 0 }

/// 186: gettid() → 현재 pid (스레드 없음)
pub fn sys_gettid() -> i64 {
    crate::process::scheduler::current_pid() as i64
}

/// 218: set_tid_address(tidptr*) → tid 반환 (futex 관련, glibc 초기화)
pub fn sys_set_tid_address(_tidptr: u64) -> i64 {
    crate::process::scheduler::current_pid() as i64
}

/// 231: exit_group(code) — exit()와 동일하게 longjmp
pub fn sys_exit_group(code: u64) -> i64 {
    super::sys_exit_impl(code)
}

// ── signal 구현 (BETA 3) ──────────────────────────────────────────────────────

/// 13: rt_sigaction(signum, act*, oldact*, sigsetsize)
/// sigaction 구조체 레이아웃: sa_handler(u64) + sa_flags(u64) + sa_restorer(u64) + sa_mask(u64×16)
pub fn sys_rt_sigaction(signum: u64, act: u64, oldact: u64, _size: u64) -> i64 {
    let sig = signum as usize;
    if sig == 0 || sig >= NSIG { return EINVAL; }

    // 이전 핸들러 반환
    if oldact != 0 {
        let old_handler = SIG_HANDLERS[sig].load(Ordering::Relaxed);
        unsafe {
            core::ptr::write_bytes(oldact as *mut u8, 0, 32);
            *(oldact as *mut u64) = old_handler;
        }
    }
    // 새 핸들러 저장
    if act != 0 {
        let handler = unsafe { *(act as *const u64) };
        SIG_HANDLERS[sig].store(handler, Ordering::Relaxed);
    }
    0
}

/// 14: rt_sigprocmask(how, set*, oldset*, sigsetsize)
pub fn sys_rt_sigprocmask(how: u64, set: u64, oldset: u64, _size: u64) -> i64 {
    const SIG_BLOCK:   u64 = 0;
    const SIG_UNBLOCK: u64 = 1;
    const SIG_SETMASK: u64 = 2;

    let cur = SIG_MASK.load(Ordering::Relaxed);
    if oldset != 0 {
        unsafe { *(oldset as *mut u64) = cur; }
    }
    if set != 0 {
        let new_bits = unsafe { *(set as *const u64) };
        let next = match how {
            SIG_BLOCK   => cur | new_bits,
            SIG_UNBLOCK => cur & !new_bits,
            SIG_SETMASK => new_bits,
            _           => return EINVAL,
        };
        SIG_MASK.store(next, Ordering::Relaxed);
    }
    0
}

/// 15: rt_sigreturn() — 시그널 컨텍스트 복원 (단순 stub)
pub fn sys_rt_sigreturn() -> i64 { 0 }

/// 62: kill(pid, sig) — 시그널 전송 (같은 프로세스만 지원)
pub fn sys_kill(_pid: u64, sig: u64) -> i64 {
    let s = sig as usize;
    if s == 0 || s >= NSIG { return EINVAL; }
    SIG_PENDING.fetch_or(1u64 << (s - 1), Ordering::Relaxed);
    crate::serial_println!("[sig] kill(sig={}) → pending={:#x}",
        s, SIG_PENDING.load(Ordering::Relaxed));
    0
}

/// 7: poll(fds*, nfds, timeout)
/// struct pollfd: fd(i32) + events(i16) + revents(i16) = 8 bytes
/// timeout: -1 = 블로킹, 0 = 비블로킹, >0 = 밀리초
pub fn sys_poll(fds_vaddr: u64, nfds: u64, timeout: u64) -> i64 {
    const POLLIN:  i16 = 0x0001;
    const POLLOUT: i16 = 0x0004;
    const POLLERR: i16 = 0x0008;

    let blocking = timeout == u64::MAX || timeout != 0; // -1 as u64 or > 0

    loop {
        let mut ready = 0i64;
        for i in 0..nfds as usize {
            let base = fds_vaddr + (i * 8) as u64;
            let fd     = unsafe { *(base       as *const i32) };
            let events = unsafe { *((base + 4) as *const i16) };
            let rptr   = (base + 6) as *mut i16;

            let revents: i16 = match fd {
                0 => if events & POLLIN != 0 && crate::kbd::has_key() { POLLIN } else { 0 },
                1 | 2 => events & POLLOUT,
                f if crate::syscall::pipe::is_read_fd(f) => {
                    if events & POLLIN != 0 && crate::syscall::pipe::has_data() { POLLIN } else { 0 }
                }
                f if crate::syscall::pipe::is_write_fd(f) => events & POLLOUT,
                f if crate::syscall::sock::is_sock_fd(f)  => POLLERR, // 미연결 소켓
                _ => 0,
            };
            unsafe { *rptr = revents; }
            if revents != 0 { ready += 1; }
        }
        if ready > 0 || !blocking { return ready; }
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }
}

// ── futex ────────────────────────────────────────────────────────────────────

/// 202: futex(uaddr*, op, val, timeout*, uaddr2*, val3)
///
/// 싱글코어 싱글스레드 환경 → FUTEX_WAIT는 즉시 EAGAIN 또는 0,
/// FUTEX_WAKE는 0 반환.
pub fn sys_futex(uaddr: u64, op: u64, val: u64,
                 _timeout: u64, _uaddr2: u64, _val3: u64) -> i64 {
    const FUTEX_WAIT:         u64 = 0;
    const FUTEX_WAKE:         u64 = 1;
    const FUTEX_REQUEUE:      u64 = 3;
    const FUTEX_PRIVATE_FLAG: u64 = 128;
    const FUTEX_CLOCK_RT:     u64 = 256;

    let op_kind = op & !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_RT);

    match op_kind {
        FUTEX_WAIT => {
            // *uaddr != val 이면 즉시 EAGAIN
            if uaddr == 0 { return -14; } // EFAULT
            let cur = unsafe { *(uaddr as *const u32) };
            if cur != val as u32 {
                return -11; // EAGAIN
            }
            // 싱글스레드: 다른 쪽이 깨울 수 없으므로 즉시 0 반환
            0
        }
        FUTEX_WAKE => {
            // 깨울 웨이터 없음 → 0
            0
        }
        FUTEX_REQUEUE => 0,
        _ => 0, // 나머지 op도 허용 (FUTEX_WAKE_OP 등)
    }
}
