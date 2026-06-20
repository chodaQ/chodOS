//! 8259A PIC (Programmable Interrupt Controller) 드라이버
//!
//! ## 하드웨어 구조
//!
//! PC에는 두 개의 8259A PIC가 계단식(cascade)으로 연결됨:
//! - 마스터 PIC: IRQ0-7  → 슬레이브의 IRQ2에 연결
//! - 슬레이브 PIC: IRQ8-15
//!
//! ```
//! 마스터 PIC (0x20/0x21)       슬레이브 PIC (0xA0/0xA1)
//! IRQ0 = PIT 타이머             IRQ8  = RTC
//! IRQ1 = 키보드                 IRQ9  = (재지정 가능)
//! IRQ2 = 슬레이브 캐스케이드    IRQ10 = (재지정 가능)
//! IRQ3 = COM2                   IRQ11 = (재지정 가능)
//! IRQ4 = COM1                   IRQ12 = PS/2 마우스
//! IRQ5 = LPT2                   IRQ13 = FPU
//! IRQ6 = 플로피                 IRQ14 = ATA 기본
//! IRQ7 = LPT1                   IRQ15 = ATA 보조
//! ```
//!
//! ## 왜 리매핑이 필요한가?
//!
//! 기본값: 마스터 IRQ0-7 → 인터럽트 벡터 0x08-0x0F
//! 문제:   CPU 예외(#DE=0, #DF=8, #GP=13, #PF=14)와 충돌!
//! 해결:   마스터 → 0x20-0x27, 슬레이브 → 0x28-0x2F 로 이동

const PIC1_CMD:  u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD:  u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const PIC_EOI:   u8 = 0x20; // End-Of-Interrupt 명령

// ICW = Initialization Command Word (초기화 명령어 순서)
const ICW1_INIT: u8 = 0x11; // 엣지 트리거, 캐스케이드, ICW4 필요
const ICW4_8086: u8 = 0x01; // 8086 모드 (8080 모드 아님)

/// 마스터 PIC IRQ0-7 → 벡터 0x20-0x27
pub const PIC1_OFFSET: u8 = 0x20;
/// 슬레이브 PIC IRQ8-15 → 벡터 0x28-0x2F
pub const PIC2_OFFSET: u8 = 0x28;

fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al",
            in("dx") port, in("al") val,
            options(nomem, nostack, preserves_flags));
    }
}

fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe {
        core::arch::asm!("in al, dx",
            out("al") v, in("dx") port,
            options(nomem, nostack, preserves_flags));
    }
    v
}

/// 짧은 I/O 지연 (포트 0x80 = POST 진단 포트)
///
/// 오래된 하드웨어에서 PIC 초기화 명령어 사이에 지연이 필요함.
/// QEMU에서는 사실 불필요하지만, 실제 하드웨어 호환성을 위해 유지.
fn io_wait() {
    outb(0x80, 0);
}

/// 8259A PIC 초기화 및 IRQ 리매핑
///
/// 초기화 후 활성화되는 IRQ:
/// - IRQ0 (타이머, 벡터 0x20)
/// - IRQ1 (키보드, 벡터 0x21)
/// 나머지는 모두 마스킹(비활성화).
pub fn init() {
    // 현재 마스크 저장 (필요하면 나중에 복원)
    let _mask1 = inb(PIC1_DATA);
    let _mask2 = inb(PIC2_DATA);

    // ── ICW1: 초기화 시작 ─────────────────────────────────────────────
    outb(PIC1_CMD,  ICW1_INIT); io_wait();
    outb(PIC2_CMD,  ICW1_INIT); io_wait();

    // ── ICW2: 벡터 오프셋 설정 ────────────────────────────────────────
    outb(PIC1_DATA, PIC1_OFFSET); io_wait(); // 마스터: IRQ0-7 → 0x20-0x27
    outb(PIC2_DATA, PIC2_OFFSET); io_wait(); // 슬레이브: IRQ8-15 → 0x28-0x2F

    // ── ICW3: 캐스케이드 설정 ─────────────────────────────────────────
    outb(PIC1_DATA, 0x04); io_wait(); // 마스터: IRQ2에 슬레이브 연결 (비트 2 = 1)
    outb(PIC2_DATA, 0x02); io_wait(); // 슬레이브: 캐스케이드 ID = 2

    // ── ICW4: 8086 모드 ───────────────────────────────────────────────
    outb(PIC1_DATA, ICW4_8086); io_wait();
    outb(PIC2_DATA, ICW4_8086); io_wait();

    // ── 인터럽트 마스크 설정 ──────────────────────────────────────────
    // 마스터 마스크: 0xF8 = 1111_1000b
    //   bit0(IRQ0)=0 타이머, bit1(IRQ1)=0 키보드, bit2(IRQ2)=0 캐스케이드 활성화
    //   IRQ2가 열려야 PIC2(슬레이브) IRQ8-15가 전달됨
    // 슬레이브 마스크: 0xEF = 1110_1111b
    //   bit4(IRQ12)=0 마우스 활성화, 나머지 차단
    outb(PIC1_DATA, 0xF8);
    outb(PIC2_DATA, 0xEF);

    crate::serial_println!("[pic] 8259A initialized: IRQ0(timer) IRQ1(kbd) IRQ2(cascade) IRQ12(mouse)");
}

/// 마스터 PIC에 EOI 전송 (IRQ0-7 처리 후 호출)
#[inline]
pub fn eoi_master() {
    outb(PIC1_CMD, PIC_EOI);
}

/// 슬레이브 PIC에 EOI 전송 (IRQ8-15 처리 후 호출)
///
/// 슬레이브 IRQ는 마스터 IRQ2를 통해 전달되므로,
/// 슬레이브와 마스터 양쪽에 모두 EOI를 보내야 함.
#[allow(dead_code)]
#[inline]
pub fn eoi_slave() {
    outb(PIC2_CMD, PIC_EOI);
    outb(PIC1_CMD, PIC_EOI);
}
