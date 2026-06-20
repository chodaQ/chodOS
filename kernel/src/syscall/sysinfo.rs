//! BETA 2-1: 시스템 정보 syscall — Linux x86-64 ABI

use super::EINVAL;

// ── 구조체 정의 ──────────────────────────────────────────────────────────────

#[repr(C)]
struct Timeval  { tv_sec: i64, tv_usec: i64 }

#[repr(C)]
struct Timespec { tv_sec: i64, tv_nsec: i64 }

/// struct sysinfo (Linux x86-64)
#[repr(C)]
struct SysInfo {
    uptime:    i64,
    loads:     [u64; 3],
    totalram:  u64,
    freeram:   u64,
    sharedram: u64,
    bufferram: u64,
    totalswap: u64,
    freeswap:  u64,
    procs:     u16,
    _pad:      [u8; 6],
    totalhigh: u64,
    freehigh:  u64,
    mem_unit:  u32,
    _extra:    [u8; 20],
}

/// struct rlimit
#[repr(C)]
struct Rlimit { rlim_cur: u64, rlim_max: u64 }

const RLIM_INFINITY: u64 = u64::MAX;

// ── 틱 카운터 접근 ────────────────────────────────────────────────────────────

fn uptime_ms() -> u64 { 0 }

// ── 구현 ──────────────────────────────────────────────────────────────────────

/// 96: gettimeofday(timeval*, timezone*) → 부팅 이후 경과 시간 (epoch 미지원)
pub fn sys_gettimeofday(tv_vaddr: u64, _tz_vaddr: u64) -> i64 {
    if tv_vaddr != 0 {
        let ms = uptime_ms();
        let tv = unsafe { &mut *(tv_vaddr as *mut Timeval) };
        tv.tv_sec  = (ms / 1000) as i64;
        tv.tv_usec = ((ms % 1000) * 1000) as i64;
    }
    0
}

/// 97: getrlimit(resource, rlimit*)
pub fn sys_getrlimit(_resource: u64, rlim_vaddr: u64) -> i64 {
    if rlim_vaddr != 0 {
        let r = unsafe { &mut *(rlim_vaddr as *mut Rlimit) };
        r.rlim_cur = RLIM_INFINITY;
        r.rlim_max = RLIM_INFINITY;
    }
    0
}

/// 99: sysinfo(info*)
pub fn sys_sysinfo(info_vaddr: u64) -> i64 {
    let s = unsafe { &mut *(info_vaddr as *mut SysInfo) };
    let ms = uptime_ms();
    *s = SysInfo {
        uptime:    (ms / 1000) as i64,
        loads:     [0; 3],
        totalram:  256 * 1024 * 1024,  // 256MB
        freeram:   128 * 1024 * 1024,  // 128MB (추정)
        sharedram: 0,
        bufferram: 0,
        totalswap: 0,
        freeswap:  0,
        procs:     1,
        _pad:      [0; 6],
        totalhigh: 0,
        freehigh:  0,
        mem_unit:  1,
        _extra:    [0; 20],
    };
    0
}

/// 228: clock_gettime(clockid, timespec*)
///
/// CLOCK_REALTIME(0) / CLOCK_MONOTONIC(1) / CLOCK_PROCESS_CPUTIME_ID(2) /
/// CLOCK_THREAD_CPUTIME_ID(3) — 모두 부팅 이후 단조 시간으로 반환
pub fn sys_clock_gettime(clockid: u64, tp_vaddr: u64) -> i64 {
    if tp_vaddr == 0 { return -14; } // EFAULT
    // 알려진 clock ID만 허용
    match clockid {
        0 | 1 | 2 | 3 | 4 | 6 | 7 => {}
        _ => return EINVAL,
    }
    let ms = uptime_ms();
    let tp = unsafe { &mut *(tp_vaddr as *mut Timespec) };
    tp.tv_sec  = (ms / 1000) as i64;
    tp.tv_nsec = ((ms % 1000) * 1_000_000) as i64;
    0
}

/// 229: clock_getres(clockid, timespec*) → 10ms 해상도 반환
pub fn sys_clock_getres(_clockid: u64, tp_vaddr: u64) -> i64 {
    if tp_vaddr != 0 {
        let tp = unsafe { &mut *(tp_vaddr as *mut Timespec) };
        tp.tv_sec  = 0;
        tp.tv_nsec = 10_000_000; // 10ms
    }
    0
}

/// 234: tgkill(tgid, tid, sig) → stub 0 (시그널 미지원)
pub fn sys_tgkill(_tgid: u64, _tid: u64, _sig: u64) -> i64 { 0 }

/// 435: clone3 → ENOSYS (musl 폴백 유도)
pub fn sys_clone3(_args: u64, _size: u64) -> i64 { -38 }
