//! 비트맵 물리 프레임 할당자 (Bitmap Physical Frame Allocator)
//!
//! ## 설계 원칙
//!
//! 물리 메모리를 4KB 페이지(프레임) 단위로 관리.
//! 비트맵의 각 비트가 프레임 하나를 나타냄:
//!   1 = free  (사용 가능)
//!   0 = used  (사용 중 또는 접근 불가)
//!
//! ## 왜 비트맵인가?
//! - 구현 단순, 이해하기 쉬움
//! - 최대 8GB 메모리를 256KB 비트맵으로 관리 (8GB / 4KB / 8 bits = 256KB)
//! - 할당: O(n/64) — 64비트 단위로 스캔하므로 n/64배 빠름
//! - 해제: O(1)
//!
//! ## 한계 (향후 개선 포인트)
//! - Buddy System 대비 내부 단편화 없지만, 연속 할당 탐색이 느림
//! - 멀티코어 환경에서 락 없이 unsafe하게 접근 중 (단일코어 가정)
//! - Milestone 3에서 Buddy Allocator로 교체 예정

use core::sync::atomic::{AtomicU64, Ordering};
use limine::memmap;

// ==================== 상수 ====================

/// 페이지(프레임) 크기: 4KB
pub const PAGE_SIZE: u64 = 4096;

/// 비트맵으로 관리 가능한 최대 프레임 수
/// 32768(워드 수) × 64(비트/워드) = 2,097,152 프레임 = 8GiB
const BITMAP_WORDS: usize = 32768;
const MAX_FRAMES: usize = BITMAP_WORDS * 64;

// ==================== 전역 상태 ====================

/// 프레임 비트맵 (1 = free, 0 = used)
///
/// `static mut`: 커널 초기화 단계에서 단일 코어가 초기화 전용으로 접근.
/// 멀티코어 지원 시 뮤텍스로 교체 필요.
///
/// `.bss` 섹션에 배치 → limine이 0으로 채워줌 → 초기 상태: 전부 used
static mut BITMAP: [u64; BITMAP_WORDS] = [0u64; BITMAP_WORDS];

/// 현재 free 프레임 수 (통계용)
static FREE_FRAMES: AtomicU64 = AtomicU64::new(0);

/// 전체 usable 프레임 수 (통계용)
static TOTAL_USABLE: AtomicU64 = AtomicU64::new(0);

// ==================== 비트맵 헬퍼 ====================

/// 특정 프레임을 free로 마킹
#[inline]
unsafe fn set_free(frame: usize) {
    BITMAP[frame / 64] |= 1u64 << (frame % 64);
}

/// 특정 프레임을 used로 마킹
#[inline]
unsafe fn set_used(frame: usize) {
    BITMAP[frame / 64] &= !(1u64 << (frame % 64));
}

/// 특정 프레임이 free인지 확인
#[inline]
unsafe fn is_free(frame: usize) -> bool {
    BITMAP[frame / 64] & (1u64 << (frame % 64)) != 0
}

// ==================== 초기화 ====================

/// 프레임 할당자 초기화
///
/// limine 메모리 맵을 스캔해서 `MEMMAP_USABLE` 영역의 프레임만 free로 마킹.
/// 나머지(커널, 펌웨어, ACPI, limine 등)는 이미 0(used)이므로 건드리지 않음.
///
/// limine이 메모리 맵을 이미 정리해주기 때문에:
/// - `MEMMAP_EXECUTABLE_AND_MODULES` = 커널 + 모듈 (건드리지 않음)
/// - `MEMMAP_BOOTLOADER_RECLAIMABLE` = 나중에 회수 가능 (지금은 건드리지 않음)
/// - `MEMMAP_USABLE` = 우리가 자유롭게 쓸 수 있는 RAM
pub fn init(memmap_entries: &[&memmap::Entry]) {
    let mut total: u64 = 0;
    let mut free: u64 = 0;

    for entry in memmap_entries {
        if entry.type_ != memmap::MEMMAP_USABLE {
            continue;
        }

        // 이 usable 영역의 시작/끝 프레임 인덱스
        // 끝 주소는 반드시 PAGE_SIZE 배수여야 함 (limine이 보장)
        let start_frame = (entry.base / PAGE_SIZE) as usize;
        let end_frame = ((entry.base + entry.length) / PAGE_SIZE) as usize;

        for frame in start_frame..end_frame.min(MAX_FRAMES) {
            unsafe { set_free(frame) };
            free += 1;
        }
        total += entry.length / PAGE_SIZE;
    }

    FREE_FRAMES.store(free, Ordering::Relaxed);
    TOTAL_USABLE.store(total, Ordering::Relaxed);

    crate::serial_println!(
        "[mem] frame allocator: {} MB usable ({} frames free)",
        free * PAGE_SIZE / (1024 * 1024),
        free
    );
}

// ==================== 할당 / 해제 ====================

/// 물리 프레임 1개 할당
///
/// 비트맵을 64비트 단위로 스캔해서 첫 번째 free 프레임을 찾아 반환.
/// 반환값: 해당 프레임의 물리 주소 (4KB 정렬)
#[allow(dead_code)] // alloc_contiguous() 위주로 쓰지만 단일 프레임 할당용으로 공개
pub fn alloc_frame() -> Option<u64> {
    // BITMAP.iter_mut() 대신 raw 포인터를 사용.
    // static mut에 대한 가변 참조(&mut)는 Rust 2024부터 경고가 됨.
    // raw 포인터(*mut u64)는 참조 규칙 밖에 있어 경고 없이 접근 가능.
    // Safety: 단일 코어, 인터럽트 없는 초기화 단계에서만 호출됨.
    unsafe {
        let bitmap = core::ptr::addr_of_mut!(BITMAP);
        for word_idx in 0..BITMAP_WORDS {
            let word_ptr = (*bitmap).as_mut_ptr().add(word_idx);
            let word_val = word_ptr.read();
            if word_val == 0 {
                continue; // 이 64개 프레임은 전부 used
            }
            // trailing_zeros(): 최하위 set 비트 위치 = 가장 낮은 주소의 free 프레임
            let bit = word_val.trailing_zeros() as usize;
            word_ptr.write(word_val & !(1u64 << bit)); // used 마킹
            FREE_FRAMES.fetch_sub(1, Ordering::Relaxed);

            let frame_idx = word_idx * 64 + bit;
            return Some(frame_idx as u64 * PAGE_SIZE);
        }
    }
    None // 물리 메모리 고갈
}

/// 연속된 `count`개의 물리 프레임을 한 번에 할당
///
/// 연결 리스트 힙 할당자는 연속된 물리 메모리가 필요하므로 이 함수를 사용.
/// 반환값: 첫 번째 프레임의 물리 주소 (해당 주소부터 count * 4KB가 연속)
///
/// 성능: O(MAX_FRAMES) — 최악의 경우 전체 비트맵 순회
pub fn alloc_contiguous(count: usize) -> Option<u64> {
    if count == 0 {
        return None;
    }

    unsafe {
        let mut run_start = 0usize; // 현재 연속 구간 시작
        let mut run_len = 0usize;   // 현재 연속 구간 길이

        for frame in 0..MAX_FRAMES {
            if is_free(frame) {
                if run_len == 0 {
                    run_start = frame;
                }
                run_len += 1;

                if run_len == count {
                    // count개 연속 free 프레임 발견 → 전부 used로 마킹
                    for i in run_start..run_start + count {
                        set_used(i);
                    }
                    FREE_FRAMES.fetch_sub(count as u64, Ordering::Relaxed);
                    return Some(run_start as u64 * PAGE_SIZE);
                }
            } else {
                // 연속이 끊김 → 처음부터 다시
                run_len = 0;
            }
        }
    }
    None // 연속된 count개 프레임 없음
}

/// 물리 프레임 해제 (반환)
///
/// `phys_addr`은 반드시 PAGE_SIZE 배수여야 함.
#[allow(dead_code)] // 프로세스 종료 시 메모리 반환 때 사용
pub fn free_frame(phys_addr: u64) {
    let frame = (phys_addr / PAGE_SIZE) as usize;
    if frame < MAX_FRAMES {
        unsafe {
            // 이미 free인데 또 free하는 double-free는 검사하지 않음 (성능 우선)
            // 나중에 debug_assert!로 추가 가능
            if !is_free(frame) {
                set_free(frame);
                FREE_FRAMES.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

// ==================== 통계 ====================

pub fn free_frame_count() -> u64 {
    FREE_FRAMES.load(Ordering::Relaxed)
}

#[allow(dead_code)]
pub fn total_usable_frames() -> u64 {
    TOTAL_USABLE.load(Ordering::Relaxed)
}
