//! 인터럽트 서브시스템
//!
//! ## 초기화 순서 (반드시 이 순서를 지켜야 함)
//!
//! 1. `gdt::init()` — GDT + TSS 로드 (CS/SS 세그먼트 설정)
//! 2. `idt::init()` — IDT 로드 (예외/IRQ 핸들러 등록)
//! 3. `pic::init()` — 8259A PIC 리매핑 + IRQ 마스크 설정
//! 4. `sti`         — 인터럽트 활성화 (IF 플래그 세트)
//!
//! GDT 이전에 IDT를 로드하면 CS 셀렉터가 맞지 않을 수 있고,
//! IDT 이전에 STI하면 핸들러 없는 인터럽트가 트리플 폴트를 유발함.

pub mod gdt;
pub mod handlers;
pub mod idt;
pub mod pic;

pub use handlers::TICK;

/// 인터럽트 서브시스템 전체 초기화
///
/// `memory::init()` 이후에 호출해야 함 (시리얼 출력이 필요하기 때문).
pub fn init() {
    gdt::init();
    idt::init();
    pic::init();

    // STI: RFLAGS.IF = 1 → CPU가 마스크되지 않은 인터럽트를 처리하기 시작
    // 이 명령어 이후부터 타이머/키보드 인터럽트가 실제로 발생함
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack));
    }

    crate::serial_println!("[interrupts] interrupts enabled (STI)");
}
