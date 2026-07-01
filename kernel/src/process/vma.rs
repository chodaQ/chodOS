//! BETA 16~17: VMA (Virtual Memory Area) 테이블
//!
//! ## 역할
//!
//! mmap된 모든 영역을 추적: 주소, 크기, 권한(prot), lazy 여부.
//!
//! - **BETA 16 mprotect**: VMA에서 prot 조회 → `paging::set_page_prot()` 호출
//! - **BETA 17 demand paging**: lazy=true인 VMA에 첫 접근 시 #PF →
//!   `try_demand_page()`가 단일 페이지 동적 할당 후 retry

use spin::Mutex;

// ── 상수 ─────────────────────────────────────────────────────────────────────

pub const PROT_NONE:  u32 = 0;
pub const PROT_READ:  u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC:  u32 = 4;

const MAX_VMA: usize = 128;

// ── VmaEntry ──────────────────────────────────────────────────────────────────

#[derive(Copy, Clone)]
pub struct VmaEntry {
    pub base:  u64,
    pub pages: usize,
    /// Linux PROT_* 플래그 조합 (PROT_READ | PROT_WRITE | PROT_EXEC)
    pub prot:  u32,
    /// true = demand paging (첫 접근 시 #PF → 동적 할당)
    /// false = eager (mmap 시점에 이미 물리 페이지 매핑됨)
    pub lazy:  bool,
    pub valid: bool,
}

impl VmaEntry {
    const fn empty() -> Self {
        VmaEntry { base: 0, pages: 0, prot: 0, lazy: false, valid: false }
    }
}

// ── 전역 VMA 테이블 ───────────────────────────────────────────────────────────
//
// 단순화: 단일 전역 테이블 (프로세스별 분리는 이후 단계로 미룸)

static VMA_TABLE: Mutex<[VmaEntry; MAX_VMA]> = Mutex::new([const { VmaEntry::empty() }; MAX_VMA]);

// ── CRUD ──────────────────────────────────────────────────────────────────────

/// VMA 등록. 같은 base가 이미 있으면 덮어씀.
pub fn insert(base: u64, pages: usize, prot: u32, lazy: bool) {
    let mut t = VMA_TABLE.lock();
    for e in t.iter_mut() {
        if e.valid && e.base == base {
            *e = VmaEntry { base, pages, prot, lazy, valid: true };
            return;
        }
    }
    for e in t.iter_mut() {
        if !e.valid {
            *e = VmaEntry { base, pages, prot, lazy, valid: true };
            return;
        }
    }
    // 가득 참 — pages 가장 적은 슬롯 교체
    let mut min_i = 0;
    for i in 1..MAX_VMA {
        if t[i].pages < t[min_i].pages { min_i = i; }
    }
    t[min_i] = VmaEntry { base, pages, prot, lazy, valid: true };
}

/// `addr`로 시작하는 VMA 제거. 해제된 항목 반환.
pub fn remove(addr: u64) -> Option<VmaEntry> {
    let mut t = VMA_TABLE.lock();
    for e in t.iter_mut() {
        if e.valid && e.base == addr {
            e.valid = false;
            return Some(*e);
        }
    }
    None
}

/// `vaddr`을 포함하는 VMA 검색.
pub fn find(vaddr: u64) -> Option<VmaEntry> {
    let t = VMA_TABLE.lock();
    for e in t.iter() {
        if e.valid && vaddr >= e.base && vaddr < e.base + e.pages as u64 * 4096 {
            return Some(*e);
        }
    }
    None
}

/// `[addr, addr+len)` 범위와 겹치는 VMA들의 prot 갱신.
/// 갱신된 VMA가 하나 이상이면 true.
pub fn update_prot(addr: u64, len: u64, new_prot: u32) -> bool {
    let mut t = VMA_TABLE.lock();
    let end = addr + len;
    let mut found = false;
    for e in t.iter_mut() {
        if !e.valid { continue; }
        let e_end = e.base + e.pages as u64 * 4096;
        if addr < e_end && end > e.base {
            e.prot = new_prot;
            found = true;
        }
    }
    found
}

// ── Demand Paging ─────────────────────────────────────────────────────────────

/// #PF(not-present, 유저 모드) 핸들러에서 호출.
///
/// `cr2`: 폴트 발생 가상 주소
/// `cr3`: 현재 페이지 디렉터리 물리 주소
///
/// - VMA가 존재하고 `lazy=true`이면 → 단일 페이지 동적 할당 + PTE 설정 → `true`
/// - 해당 VMA 없음 또는 `lazy=false`이면 → `false` (→ SIGSEGV)
pub fn try_demand_page(cr2: u64, cr3: u64) -> bool {
    let entry = match find(cr2) {
        Some(e) if e.lazy => e,
        _ => return false,
    };
    let page_va  = cr2 & !0xFFF;
    let writable = entry.prot & PROT_WRITE != 0;
    unsafe { crate::paging::demand_alloc_page(cr3, page_va, writable); }
    crate::serial_println!(
        "[demand] 페이지 할당: va={:#x} prot={:#x}",
        page_va, entry.prot,
    );
    true
}
