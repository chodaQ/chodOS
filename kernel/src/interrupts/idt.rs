//! IDT (Interrupt Descriptor Table) 설정
//!
//! ## IDT 게이트 디스크립터 (16바이트)
//!
//! ```
//! bits 0:15   = handler[15:0]
//! bits 16:31  = 코드 세그먼트 셀렉터 (CS = 0x08)
//! bits 32:34  = IST 인덱스 (0 = 전용 스택 없음)
//! bits 35:39  = 예약 (0)
//! bits 40:47  = 타입 + 속성
//!                 0x8E = 인터럽트 게이트 (IF 클리어, 하드웨어 IRQ에 사용)
//!                 0x8F = 트랩 게이트    (IF 유지,   CPU 예외에 사용)
//! bits 48:63  = handler[31:16]
//! bits 64:95  = handler[63:32]
//! bits 96:127 = 예약 (0)
//! ```
//!
//! ## 인터럽트 게이트 vs 트랩 게이트
//!
//! 인터럽트 게이트 (0x8E): 진입 시 RFLAGS.IF = 0 (인터럽트 비활성화)
//!   → 하드웨어 IRQ: 핸들러 실행 중 같은 IRQ가 재진입하는 것을 방지
//!
//! 트랩 게이트 (0x8F): RFLAGS.IF 변경 없음
//!   → CPU 예외: 예외 핸들러 안에서도 다른 인터럽트 허용 (디버깅 편의)

use core::{arch::asm, mem};
use super::gdt::KERNEL_CODE_SEL;

const INT_GATE:       u8 = 0x8E; // 인터럽트 게이트 (진입 시 IF 클리어, DPL=0)
const TRAP_GATE:      u8 = 0x8F; // 트랩 게이트 (IF 유지, DPL=0)
/// 트랩 게이트 DPL=3: ring3의 `int 0x80` 소프트웨어 인터럽트 허용
/// DPL이 3이어야 ring3 코드가 `int 0x80`을 발생시킬 수 있음.
/// DPL=0이면 ring3에서 int 0x80 시 #GP 발생.
const TRAP_GATE_DPL3: u8 = 0xEF; // P=1, DPL=3, Type=0xF (trap gate)

/// IDT 게이트 디스크립터 (16바이트)
#[derive(Clone, Copy)]
#[repr(C, packed)]
struct IdtEntry {
    offset_low:  u16,
    selector:    u16,
    ist:         u8,  // bits[2:0] = IST 인덱스 (0 = 전용 스택 없음)
    type_attr:   u8,
    offset_mid:  u16,
    offset_high: u32,
    _reserved:   u32,
}

impl IdtEntry {
    const fn absent() -> Self {
        Self { offset_low: 0, selector: 0, ist: 0, type_attr: 0,
               offset_mid: 0, offset_high: 0, _reserved: 0 }
    }

    fn set(&mut self, handler: u64, gate: u8, ist: u8) {
        self.offset_low  = handler as u16;
        self.offset_mid  = (handler >> 16) as u16;
        self.offset_high = (handler >> 32) as u32;
        self.selector    = KERNEL_CODE_SEL;
        self.ist         = ist & 0x7;
        self.type_attr   = gate;
        self._reserved   = 0;
    }
}

#[repr(C, align(16))]
struct Idt([IdtEntry; 256]);

#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base:  u64,
}

static mut IDT: Idt = Idt([IdtEntry::absent(); 256]);

// handlers.rs의 global_asm에서 정의된 어셈블리 스텁 선언
extern "C" {
    fn isr0();  fn isr1();  fn isr2();  fn isr3();
    fn isr4();  fn isr5();  fn isr6();  fn isr7();
    fn isr8();  fn isr9();  fn isr10(); fn isr11();
    fn isr12(); fn isr13(); fn isr14(); fn isr15();
    fn isr16(); fn isr17(); fn isr18(); fn isr19();
    fn isr20(); fn isr21(); fn isr22(); fn isr23();
    fn isr24(); fn isr25(); fn isr26(); fn isr27();
    fn isr28(); fn isr29(); fn isr30(); fn isr31();
    fn isr32();  // 타이머 IRQ0 — 선점형 스케줄러
    fn isr33();  // 키보드 IRQ1
    fn isr44();  // 마우스 IRQ12 (PIC2, 벡터 0x2C)
    fn isr64();  // 자발적 양보 (int 0x40, yield_now)
    fn isr128(); // 소프트웨어 인터럽트 (syscall: int 0x80)
}

pub fn init() {
    unsafe {
        // ── CPU 예외 (벡터 0-31): 트랩 게이트 ────────────────────────────
        // 트랩 게이트를 쓰는 이유: 예외 핸들러 안에서도 타이머 인터럽트 등
        // 다른 인터럽트가 처리될 수 있어야 함 (특히 디버그 예외).
        // IST = 0: 전용 스택 없음 (현재 커널 스택 그대로 사용).
        let stubs: [unsafe extern "C" fn(); 32] = [
            isr0,  isr1,  isr2,  isr3,  isr4,  isr5,  isr6,  isr7,
            isr8,  isr9,  isr10, isr11, isr12, isr13, isr14, isr15,
            isr16, isr17, isr18, isr19, isr20, isr21, isr22, isr23,
            isr24, isr25, isr26, isr27, isr28, isr29, isr30, isr31,
        ];
        for (vec, &stub) in stubs.iter().enumerate() {
            IDT.0[vec].set(stub as u64, TRAP_GATE, 0);
        }

        // ── 하드웨어 IRQ: 인터럽트 게이트 ────────────────────────────────
        // 인터럽트 게이트: 핸들러 실행 중 IF=0 → 같은 IRQ 재진입 방지.
        // PIC 리매핑 후: IRQ0 → 벡터 0x20, IRQ1 → 벡터 0x21.
        IDT.0[0x20].set(isr32 as u64, INT_GATE, 0); // IRQ0  = 타이머
        IDT.0[0x21].set(isr33 as u64, INT_GATE, 0); // IRQ1  = 키보드
        IDT.0[0x2C].set(isr44 as u64, INT_GATE, 0); // IRQ12 = 마우스 (PIC2)

        // ── 자발적 양보: yield_now() = int 0x40 (ALPHA M1) ───────────────
        // INT_GATE, DPL=0: 커널 코드(ring0)만 발생 가능.
        // 인터럽트 게이트이므로 IF=0 → voluntary_yield 실행 중 재진입 방지.
        IDT.0[0x40].set(isr64 as u64, INT_GATE, 0);

        // ── 소프트웨어 인터럽트: syscall (벡터 0x80) ─────────────────────
        // TRAP_GATE_DPL3: ring3 코드가 `int 0x80`을 실행할 수 있게 DPL=3.
        // 트랩 게이트를 쓰는 이유: syscall 처리 중 타이머 인터럽트 허용.
        IDT.0[0x80].set(isr128 as u64, TRAP_GATE_DPL3, 0);

        // ── IDTR 로드 ──────────────────────────────────────────────────────
        let idtr = IdtPointer {
            limit: (mem::size_of::<Idt>() - 1) as u16,
            base:  IDT.0.as_ptr() as u64,
        };
        asm!("lidt [{}]", in(reg) &idtr, options(readonly, nostack, preserves_flags));
    }

    crate::serial_println!("[idt] IDT loaded (32 exceptions + IRQ0/IRQ1/IRQ12 + int 0x40/0x80)");
}
