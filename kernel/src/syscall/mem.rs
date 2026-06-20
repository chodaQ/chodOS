//! BETA 2-1: 메모리 관련 syscall — Linux x86-64 ABI

/// 10: mprotect(addr, len, prot) → stub 0 (페이지 보호 변경 미지원)
pub fn sys_mprotect(_addr: u64, _len: u64, _prot: u64) -> i64 { 0 }

/// 25: mremap(old_addr, old_size, new_size, flags, new_addr?) → ENOMEM
pub fn sys_mremap(_old: u64, _old_sz: u64, _new_sz: u64, _flags: u64) -> i64 {
    -12 // ENOMEM
}

/// 26: msync(addr, len, flags) → stub 0
pub fn sys_msync(_addr: u64, _len: u64, _flags: u64) -> i64 { 0 }

/// 27: mincore(addr, len, vec*) → ENOSYS (복잡한 기능, 미지원)
pub fn sys_mincore(_addr: u64, _len: u64, _vec: u64) -> i64 { -38 } // ENOSYS

/// 28: madvise(addr, length, advice) → stub 0
/// MADV_DONTNEED, MADV_FREE 등 힌트 → 모두 무시하고 성공 반환
pub fn sys_madvise(_addr: u64, _len: u64, _advice: u64) -> i64 { 0 }

/// 149: mlock(addr, len) → stub 0
pub fn sys_mlock(_addr: u64, _len: u64) -> i64 { 0 }

/// 150: munlock(addr, len) → stub 0
pub fn sys_munlock(_addr: u64, _len: u64) -> i64 { 0 }

/// 151: mlockall(flags) → stub 0
pub fn sys_mlockall(_flags: u64) -> i64 { 0 }

/// 152: munlockall() → stub 0
pub fn sys_munlockall() -> i64 { 0 }
