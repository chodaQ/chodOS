//! UART 16550 시리얼 드라이버
//!
//! ## UART란?
//! Universal Asynchronous Receiver-Transmitter (범용 비동기 송수신기).
//! IBM PC XT(1983)부터 내장된 직렬 통신 인터페이스.
//! 현대 PC에는 물리적 포트가 없어도 칩셋에 여전히 내장되어 있음.
//!
//! ## 왜 시리얼 출력을 먼저 구현하는가?
//! - VGA/Framebuffer는 초기화가 복잡함 (GOP, EDID, 해상도 협상 등)
//! - 시리얼은 레지스터 몇 개만 설정하면 바로 동작
//! - QEMU의 `-serial stdio` 옵션으로 호스트 터미널에 출력 가능
//! - 커널 개발의 핵심 디버깅 도구
//!
//! ## 하드웨어 주소
//! COM1: 0x3F8 (IBM PC AT 표준, 거의 모든 x86 시스템에서 동일)
//! COM2: 0x2F8
//! COM3: 0x3E8
//! COM4: 0x2E8

use core::fmt;
use core::fmt::Write;

// ==================== UART 레지스터 주소 ====================
// 모든 주소는 COM1 베이스(0x3F8)로부터의 오프셋

const COM1: u16 = 0x3F8; // COM1 베이스 주소

// DLAB(Divisor Latch Access Bit) = 0 일 때의 레지스터 매핑:
const REG_DATA: u16 = COM1 + 0; // 수신 버퍼(읽기) / 송신 홀딩 레지스터(쓰기)
const REG_IER: u16 = COM1 + 1; // Interrupt Enable Register

// DLAB = 1 일 때의 레지스터 매핑 (보드레이트 설정용):
// REG_DATA(+0) → Divisor Latch Low byte
// REG_IER(+1)  → Divisor Latch High byte

// DLAB 상태와 무관하게 고정된 레지스터:
#[allow(dead_code)] // 추후 인터럽트 기반 시리얼 구현 때 사용
const REG_IIR: u16 = COM1 + 2; // Interrupt Identification Register (읽기)
const REG_FCR: u16 = COM1 + 2; // FIFO Control Register (쓰기)
const REG_LCR: u16 = COM1 + 3; // Line Control Register
const REG_MCR: u16 = COM1 + 4; // Modem Control Register
const REG_LSR: u16 = COM1 + 5; // Line Status Register
#[allow(dead_code)] // 추후 흐름 제어(flow control) 구현 때 사용
const REG_MSR: u16 = COM1 + 6; // Modem Status Register

// LCR 비트 플래그
const LCR_DLAB: u8 = 0x80; // Divisor Latch Access Bit (7번 비트)
const LCR_8BIT: u8 = 0x03; // 데이터 8비트, 패리티 없음, 정지비트 1개 (8N1)

// LSR 비트 플래그
const LSR_THRE: u8 = 0x20; // Transmit Holding Register Empty
                            // 이 비트가 1 = 다음 바이트 쓸 준비 완료
const LSR_DR:   u8 = 0x01; // Data Ready — 이 비트가 1 = 수신 버퍼에 데이터 있음

// ==================== 포트 I/O 함수 ====================
//
// x86 아키텍처는 메모리(RAM)와 별도의 I/O 주소 공간을 가짐.
// Port-Mapped I/O (PMIO)라고 함.
// 일반 메모리 접근(mov 명령어)으로는 I/O 포트에 접근 불가.
// 반드시 IN/OUT 명령어를 사용해야 함.
//
// 이것은 Memory-Mapped I/O (MMIO)와 대조됨.
// MMIO는 일반 메모리 주소 공간의 특정 범위를 하드웨어 레지스터에 매핑.
// (예: PCI Express BAR, LAPIC 레지스터)

/// I/O 포트에 1바이트 쓰기 (x86 OUT 명령어)
///
/// # Safety
/// 잘못된 포트에 쓰면 하드웨어 오작동 가능.
/// 예: 0x70 포트(CMOS RTC)에 잘못된 값을 쓰면 시스템 시계가 망가짐.
#[inline]
unsafe fn outb(port: u16, value: u8) {
	// "out dx, al": dx 레지스터의 포트 번호로 al 레지스터의 값을 출력
	// options:
	//   nomem: 이 명령어가 메모리를 읽거나 쓰지 않음 (컴파일러 최적화 힌트)
	//   nostack: 스택 포인터를 변경하지 않음
	//   preserves_flags: EFLAGS 레지스터를 변경하지 않음
	// rax → 주로 계산 결과, 함수 반환값
	//rbx → 범용
	//rcx → 주로 반복 횟수 (loop 카운터)
	//rdx → 주로 포트 주소, 나눗셈 보조
	core::arch::asm!(
		"out dx, al",
		in("dx") port,
		in("al") value,
		options(nomem, nostack, preserves_flags)
	);
}

/// I/O 포트에서 1바이트 읽기 (x86 IN 명령어)
///
/// # Safety
/// 잘못된 포트에서 읽으면 예측 불가능한 값 반환.
#[inline]
unsafe fn inb(port: u16) -> u8 {
	let value: u8;
	// "in al, dx": dx 레지스터의 포트 번호에서 al 레지스터로 값을 읽음
	core::arch::asm!(
		"in al, dx",
		out("al") value,
		in("dx") port,
		options(nomem, nostack, preserves_flags)
	);
	value
}

// ==================== UART 초기화 ====================

/// UART 16550 초기화 시퀀스
///
/// 이 시퀀스는 UART 16550A 데이터시트를 따름.
/// 순서가 중요함 — 잘못된 순서로 설정하면 UART가 오동작함.
pub fn init() {
	unsafe {
		// ── 단계 1: 인터럽트 비활성화 ──────────────────────────────────
		// UART가 인터럽트를 발생시키기 전에 인터럽트 핸들러(IDT)를 설정해야 함.
		// 지금은 IDT가 없으므로 UART 인터럽트를 모두 끔.
		outb(REG_IER, 0x00);

		// ── 단계 2: DLAB 활성화 (보드레이트 설정 모드) ─────────────────
		// LCR의 비트 7(DLAB)을 1로 설정하면 +0, +1 오프셋 레지스터가
		// 데이터/IER 대신 Divisor Latch(보드레이트 분주기)로 재매핑됨.
		outb(REG_LCR, LCR_DLAB);

		// ── 단계 3: 보드레이트 설정 (115200 baud) ──────────────────────
		// UART 기준 클럭: 1.8432 MHz
		// 실제 보드레이트 = 기준 클럭 / (16 × Divisor)
		// 115200 = 1,843,200 / (16 × 1)  → Divisor = 1
		//
		// 다른 보드레이트:
		//   9600   baud → Divisor = 12 (0x000C)
		//   38400  baud → Divisor = 3  (0x0003)
		//   115200 baud → Divisor = 1  (0x0001)  ← 최고속도
		outb(COM1 + 0, 0x01); // Divisor Low  byte = 1
		outb(COM1 + 1, 0x00); // Divisor High byte = 0

		// ── 단계 4: 데이터 포맷 설정 (DLAB 해제 + 8N1) ─────────────────
		// LCR = 0x03:
		//   비트 0-1: 11 = 데이터 8비트
		//   비트 2  :  0 = 정지비트 1개
		//   비트 3-5: 000 = 패리티 없음
		//   비트 7  :  0 = DLAB 해제 (다시 DATA/IER 레지스터로 복귀)
		//
		// 8N1이란? 8 data bits, No parity, 1 stop bit.
		// 가장 흔한 시리얼 통신 포맷.
		outb(REG_LCR, LCR_8BIT);

		// ── 단계 5: FIFO 활성화 및 클리어 ──────────────────────────────
		// FCR = 0xC7:
		//   비트 0  : 1 = FIFO 활성화 (없으면 한 번에 1바이트만 버퍼)
		//   비트 1  : 1 = 수신 FIFO 클리어
		//   비트 2  : 1 = 송신 FIFO 클리어
		//   비트 6-7: 11 = 인터럽트 트리거 레벨 = 14바이트
		//             (FIFO에 14바이트 이상 쌓이면 인터럽트)
		outb(REG_FCR, 0xC7);

		// ── 단계 6: Modem Control Register 설정 ─────────────────────────
		// MCR = 0x0B:
		//   비트 0: 1 = DTR (Data Terminal Ready) — "나 준비됐다"
		//   비트 1: 1 = RTS (Request To Send)     — "보내도 된다"
		//   비트 3: 1 = OUT2 (인터럽트 게이트 활성화 — IRQ4 연결)
		outb(REG_MCR, 0x0B);

		// ── 단계 7: 루프백 자가진단 ─────────────────────────────────────
		// MCR 비트 4 = 1: 루프백 모드 (TX 출력이 내부적으로 RX로 연결됨)
		// 테스트 바이트를 보내고 동일한 값이 수신되는지 확인.
		// UART 하드웨어가 정상인지 체크.
		outb(REG_MCR, 0x1E); // 루프백 모드 활성화
		outb(REG_DATA, 0xAE); // 테스트 바이트 전송

		// 수신된 값이 보낸 값과 같은지 확인
		// QEMU에서는 항상 성공. 실제 하드웨어에서는 실패할 수 있음.
		if inb(REG_DATA) != 0xAE {
		// TODO: 하드웨어 없음 또는 고장 상태 처리
		// 지금은 그냥 계속 진행 (시리얼 없이도 커널은 동작해야 함)
		return;
		}

		// ── 단계 8: 정상 동작 모드 복귀 ─────────────────────────────────
		// MCR = 0x0F: 루프백 해제, OUT1 + OUT2 + RTS + DTR 모두 활성화
		outb(REG_MCR, 0x0F);
	}
}

// ==================== 바이트 출력 ====================

/// 송신 버퍼가 비워질 때까지 스핀 대기 (Busy-Wait)
///
/// UART 16550의 최대 속도는 115200 baud ≈ 11,520 바이트/초 ≈ 87마이크로초/바이트.
/// 이전 바이트 전송이 끝나기 전에 새 바이트를 쓰면 덮어씌워짐.
///
/// LSR(Line Status Register)의 THRE 비트가 1이 되면 다음 바이트 쓰기 가능.
///
/// 스케줄러가 없는 지금은 바쁜 대기(busy-wait) 외에 방법이 없음.
/// 추후 DMA 또는 인터럽트 기반으로 교체 예정.
#[inline]
fn wait_for_empty_transmit() {
	unsafe {
		// LSR의 THRE(Transmit Holding Register Empty) 비트가 1이 될 때까지 폴링
		while (inb(REG_LSR) & LSR_THRE) == 0 {
		// 아무것도 안 하고 대기 (컴파일러 최적화로 루프가 제거되지 않도록
		// core::hint::spin_loop()를 넣는 것이 더 바람직하지만, 단순화를 위해 생략)
		}
	}
}

/// COM1에서 1바이트 논블로킹 읽기 — 데이터 없으면 None 즉시 반환
pub fn try_read_byte() -> Option<u8> {
    unsafe {
        if inb(REG_LSR) & LSR_DR != 0 {
            Some(inb(REG_DATA))
        } else {
            None
        }
    }
}

/// COM1에서 1바이트 블로킹 읽기 (ALPHA 15 셸 stdin)
///
/// LSR.DR(Data Ready) 비트가 설정될 때까지 폴링 후 REG_DATA에서 읽음.
/// ring0에서 호출됨 — 타이머 IRQ는 계속 처리됨 (STI 상태).
pub fn read_byte_blocking() -> u8 {
    unsafe {
        loop {
            if inb(REG_LSR) & LSR_DR != 0 {
                return inb(REG_DATA);
            }
            core::arch::asm!("pause", options(nomem, nostack, preserves_flags));
        }
    }
}

/// 1바이트를 시리얼 포트로 출력
///
/// `\n`을 `\r\n`으로 자동 변환:
/// 터미널은 CR(0x0D, 캐리지 리턴)이 없으면 다음 줄의 같은 열에서 시작함.
/// 즉, CR 없이 LF만 보내면 텍스트가 계단식으로 출력됨.
pub fn write_byte(byte: u8) {
	if byte == b'\n' {
		// LF 전에 CR을 먼저 전송
		wait_for_empty_transmit();
		unsafe { outb(REG_DATA, b'\r') };
	}
	wait_for_empty_transmit();
	unsafe { outb(REG_DATA, byte) };
}

// ==================== fmt::Write 트레이트 구현 ====================
//
// fmt::Write를 구현하면 write!() / writeln!() 매크로를 사용할 수 있음.
// 또한 format_args!()를 통해 std의 println!과 동일한 포맷팅 문법 사용 가능.

/// 시리얼 포트 출력 핸들 (zero-size type — 런타임 비용 없음)
pub struct SerialWriter;

impl fmt::Write for SerialWriter {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		// UTF-8 문자열을 바이트 단위로 순회하여 전송
		// 한국어 등 멀티바이트 문자는 각 바이트가 순서대로 전송됨
		// (터미널이 UTF-8을 지원하면 올바르게 표시됨)
		for byte in s.bytes() {
		write_byte(byte);
		}
		Ok(())
	}
}

// ==================== 내부 출력 함수 (매크로에서 호출) ====================

/// format_args!()로 만들어진 포맷 인수를 시리얼로 출력
///
/// `pub`이어야 하는 이유: `#[macro_export]` 매크로는 크레이트 루트에서 확장되므로
/// 다른 모듈에서도 이 함수에 접근할 수 있어야 함.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
	// write_fmt: fmt::Write 트레이트의 메서드
	// format_args!()가 만든 인수를 SerialWriter.write_str()로 전달
	SerialWriter.write_fmt(args).unwrap();
}

// ==================== 공개 매크로 ====================
//
// `#[macro_export]`: 매크로를 크레이트 루트에 등록.
// 크레이트 내 어디서든 `serial_print!`, `serial_println!`으로 접근 가능.
// (import 불필요 — `use crate::serial_println;` 없이도 바로 사용)
//
// `$crate`: 매크로가 정의된 크레이트를 가리키는 특수 변수.
// `crate::` 대신 `$crate::` 를 쓰는 이유:
// 나중에 이 크레이트를 lib으로 분리해도 올바른 경로를 가리키게 하기 위함.

/// 시리얼 포트에 포맷 문자열 출력 (줄바꿈 없음)
///
/// 사용법: `serial_print!("x = {}", x);`
#[macro_export]
macro_rules! serial_print {
	($($arg:tt)*) => {
		$crate::serial::_print(format_args!($($arg)*))
	};
}

/// 시리얼 포트에 포맷 문자열 출력 + 줄바꿈
///
/// 사용법: `serial_println!("Hello, {}!", name);`
#[macro_export]
macro_rules! serial_println {
	// 인수 없을 때: 빈 줄 출력
	() => ($crate::serial_print!("\n"));
	// 포맷 문자열 + 인수: 포맷 후 줄바꿈
	($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
	($fmt:expr, $($arg:tt)*) => (
		$crate::serial_print!(concat!($fmt, "\n"), $($arg)*)
	);
}
