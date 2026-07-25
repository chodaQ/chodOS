//! 커널 힙 할당자 (Kernel Heap Allocator)
//!
//! ## 역할
//! Rust의 `alloc` 크레이트(Box, Vec, String 등)가 사용하는
//! 전역 힙 메모리를 관리함.
//!
//! ## #[global_allocator]
//! Rust 런타임은 `Box::new()`, `Vec::push()` 등이 메모리를 요청할 때
//! `#[global_allocator]`로 지정된 타입의 `GlobalAlloc` 구현을 호출함.
//! 우리는 `linked_list_allocator::LockedHeap`을 사용함.
//!
//! ## linked_list_allocator 동작 방식
//! - 힙을 하나의 큰 연속 메모리 블록으로 받음
//! - 해제된 블록들을 연결 리스트로 관리
//! - 할당 시: 리스트에서 크기가 맞는 블록을 찾아 분할
//! - 해제 시: 블록을 리스트에 추가하고 인접한 블록과 병합(coalescing)
//!
//! ## 한계 (향후 개선 포인트)
//! - 단편화: 많은 할당/해제 반복 시 작은 블록들이 흩어짐
//! - Milestone 5 이후 Slab Allocator로 교체 예정

use linked_list_allocator::LockedHeap;
use super::frame;

/// 전역 힙 할당자
///
/// `LockedHeap`: 스핀락으로 보호된 linked list 힙 할당자.
/// `empty()`: 아직 초기화 전 상태. `init()` 호출 전 할당 시도 → panic.
///
/// `#[global_allocator]`: Rust에게 이 타입을 전역 메모리 할당자로 등록.
/// 이 속성이 있으면 `alloc::alloc::alloc()`, `alloc::alloc::dealloc()` 등이
/// 이 구현으로 라우팅됨.
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// 힙에 할당할 연속 프레임 수 (32MB = 8192 × 4KB)
///
/// 실험 35(kernel_stack 재사용 안전성 문제) 이후 4MB→32MB로 확대 —
/// kernel_stack(프로세스당 64KB)을 회수하지 않기로 했으므로 반복적인
/// spawn/kill이 많은 워크로드(PE-4/CFS 벤치마크 반복 시행 등)에서 힙이
/// 더 오래 버티도록 여유를 크게 늘렸다. 사용 가능한 물리 메모리가 총
/// 230MB 정도라 32MB는 무리 없는 수준(근본 수정은 아니고 유예 — 자세한
/// 내용은 `Process::reap()` 문서 주석과 EXPERIMENTS.md 실험 35 참고).
const HEAP_FRAMES: usize = 8192; // 32MB

/// 커널 힙 초기화
///
/// 1. 프레임 할당자에서 연속된 `HEAP_FRAMES`개의 물리 프레임을 확보
/// 2. HHDM 오프셋을 더해 가상 주소로 변환
/// 3. `LockedHeap`에 해당 가상 주소 범위를 힙으로 등록
///
/// # Panics
/// 연속 프레임이 부족하면 panic (부팅 실패).
pub fn init(hhdm_offset: u64) {
    let heap_size = HEAP_FRAMES * frame::PAGE_SIZE as usize;

    // 연속된 물리 프레임 확보.
    // linked_list_allocator는 연속된 메모리 범위가 필요함.
    // alloc_contiguous()는 비트맵에서 연속 free 프레임을 탐색.
    let heap_phys = frame::alloc_contiguous(HEAP_FRAMES)
        .expect("[heap] failed: not enough contiguous physical frames");

    // 물리 주소 → 가상 주소 변환
    // HHDM(Higher Half Direct Map): limine이 설정한 직접 매핑
    //   가상 주소 = 물리 주소 + HHDM 오프셋
    // 이 범위는 limine이 이미 페이지 테이블에 매핑해 두었음
    let heap_virt = (heap_phys + hhdm_offset) as usize;

    crate::serial_println!(
        "[mem] heap: {} MB at phys=0x{:x} virt=0x{:x}",
        heap_size / (1024 * 1024),
        heap_phys,
        heap_virt
    );

    // 힙 할당자에 메모리 범위 등록
    // Safety: 이 가상 주소 범위가 유효하고, 다른 용도로 사용되지 않음을 보장해야 함.
    // - phys_start는 frame allocator가 used로 마킹했으므로 이중 할당 없음
    // - HHDM 매핑은 limine이 보장
    // - 이 함수는 커널 초기화 중 단 한 번만 호출됨
    unsafe {
        ALLOCATOR.lock().init(heap_virt as *mut u8, heap_size);
    }
}
