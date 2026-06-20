//! 메모리 서브시스템 (Memory Subsystem)
//!
//! ## 초기화 순서
//! ```
//! memory::init(hhdm_offset, memmap_entries)
//!   ├── HHDM_OFFSET 전역 저장
//!   ├── frame::init()   → 물리 프레임 비트맵 구성
//!   └── heap::init()    → 커널 힙 활성화 (이후 Box/Vec 사용 가능)
//! ```
//!
//! ## HHDM (Higher Half Direct Map)
//! limine은 부팅 시 물리 메모리 전체를 상위 절반 가상 주소에 직접 매핑함.
//!   가상 주소 = 물리 주소 + HHDM_OFFSET
//!
//! 이를 통해 물리 주소를 알고 있으면 어떤 메모리든 가상 주소로 접근 가능.
//! 커널 힙, 페이지 테이블 조작 등 모든 물리 메모리 접근에 사용됨.

pub mod frame;
pub mod heap;

use core::sync::atomic::{AtomicU64, Ordering};
use limine::memmap;

// ==================== HHDM 오프셋 전역 ====================

/// HHDM 오프셋 (phys + HHDM_OFFSET = virt)
///
/// `AtomicU64`: 나중에 멀티코어 환경에서 안전하게 읽기 위해 Atomic 사용.
/// 초기화 후에는 읽기만 하므로 Relaxed ordering으로 충분.
static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);

/// HHDM 오프셋 반환
#[allow(dead_code)] // Milestone 3 페이지 테이블 구현 때 사용
pub fn hhdm_offset() -> u64 {
    HHDM_OFFSET.load(Ordering::Relaxed)
}

/// 물리 주소 → 가상 주소 변환
///
/// HHDM 매핑이 활성화된 후에만 사용 가능 (memory::init() 이후).
#[allow(dead_code)] // Milestone 3 페이지 테이블 구현 때 사용
#[inline]
pub fn phys_to_virt(phys: u64) -> u64 {
    phys + hhdm_offset()
}

// ==================== 초기화 ====================

/// 메모리 서브시스템 초기화 진입점
///
/// `hhdm_offset`: limine HhdmRequest 응답에서 받은 HHDM 시작 오프셋.
/// `memmap_entries`: limine MemmapRequest 응답에서 받은 물리 메모리 맵.
///
/// 이 함수 반환 후:
/// - 물리 프레임 할당 (`frame::alloc_frame()`) 사용 가능
/// - 커널 힙 (`Box::new()`, `Vec::new()` 등) 사용 가능
pub fn init(hhdm_offset: u64, memmap_entries: &[&memmap::Entry]) {
    // 1. HHDM 오프셋 전역 저장
    // 이후 phys_to_virt() 호출이 올바른 값을 반환하게 됨
    HHDM_OFFSET.store(hhdm_offset, Ordering::Relaxed);

    crate::serial_println!("[mem] HHDM offset: 0x{:x}", hhdm_offset);

    // 2. 물리 프레임 할당자 초기화
    // limine 메모리 맵 기반으로 usable 프레임을 free로 마킹
    frame::init(memmap_entries);

    // 3. 커널 힙 초기화
    // frame allocator에서 연속 프레임을 받아 linked_list_allocator에 등록
    heap::init(hhdm_offset);

    crate::serial_println!("[mem] memory subsystem ready");
}
