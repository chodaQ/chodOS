//! BETA 2-1: 메모리 관련 syscall — Linux x86-64 ABI
#![allow(dead_code)]

/// 10: mprotect(addr, len, prot) — BETA 16: 실제 PTE 권한 변경
///
/// prot: PROT_NONE=0, PROT_READ=1, PROT_WRITE=2, PROT_EXEC=4
pub fn sys_mprotect(addr: u64, len: u64, prot: u64) -> i64 {
    const EINVAL: i64 = -22;
    if addr & 0xFFF != 0 { return EINVAL; } // 페이지 정렬 필수
    if len == 0 { return 0; }

    let pages = (len as usize + 4095) / 4096;
    let cr3   = crate::process::userproc::current_cr3();
    if cr3 == 0 { return EINVAL; }

    // VMA 테이블 prot 갱신
    crate::process::vma::update_prot(addr, len, prot as u32);

    // 실제 PTE 권한 비트 변경
    unsafe { crate::paging::set_page_prot(cr3, addr, pages, prot as u32); }

    crate::serial_println!(
        "[mprotect] addr={:#x} len={} prot={:#x}",
        addr, len, prot,
    );
    0
}

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
