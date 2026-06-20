//! BETA 2-1: 프로세스/스레드 syscall — Linux x86-64 ABI

use super::EINVAL;

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
    copy_str(&mut buf[130..195], b"0.1.0-BETA2");
    copy_str(&mut buf[195..260], b"#1 SMP MuKernel 2026");
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
        ARCH_SET_FS => { unsafe { wrmsr(MSR_FS_BASE, addr); } 0 }
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
