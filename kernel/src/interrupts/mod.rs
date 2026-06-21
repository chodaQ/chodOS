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

/// SYSCALL/SYSRET MSR 초기화 — musl-static 등 `syscall` 인스트럭션 사용 바이너리 지원
///
/// `paging::init()`이 TSS.RSP0을 설정한 직후에 호출해야 함.
/// `kern_rsp` = 커널 스택 최상단 (= TSS.RSP0 값).
pub fn init_syscall(kern_rsp: u64) {
    extern "C" { fn syscall_entry(); }
    unsafe {
        handlers::syscall_kern_rsp = kern_rsp;

        // IA32_EFER (0xC000_0080): SCE 비트(0) 세트 → SYSCALL/SYSRET 활성화
        let efer = rdmsr(0xC0000080);
        wrmsr(0xC0000080, efer | 1);

        // IA32_STAR (0xC000_0081):
        //   [47:32] = 0x0008 → SYSCALL: CS=0x08(kernel code), SS=0x10(kernel data)
        //   [63:48] = 0x0018 → SYSRET base (미사용 — 복귀는 IRETQ로)
        wrmsr(0xC0000081, (0x0018u64 << 48) | (0x0008u64 << 32));

        // IA32_LSTAR (0xC000_0082): syscall 진입점
        wrmsr(0xC0000082, syscall_entry as *const () as u64);

        // IA32_FMASK (0xC000_0084): syscall 진입 시 클리어할 RFLAGS 비트
        // 0x200 = IF → syscall 처리 중 인터럽트 비활성
        wrmsr(0xC0000084, 0x200);
    }
    crate::serial_println!(
        "[syscall] LSTAR={:#x}  (syscall 인스트럭션 지원 ON)",
        syscall_entry as *const () as usize
    );
}

unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    core::arch::asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") lo,
        out("edx") hi,
        options(nomem, nostack),
    );
    ((hi as u64) << 32) | lo as u64
}

unsafe fn wrmsr(msr: u32, val: u64) {
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") val as u32,
        in("edx") (val >> 32) as u32,
        options(nomem, nostack),
    );
}

/// AP 전용: GDT + IDT만 재로드 (PIC/STI는 BSP가 이미 설정)
pub fn ap_init() {
    gdt::ap_load();
    idt::ap_load();
    unsafe { core::arch::asm!("sti", options(nomem, nostack)); }
}

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
