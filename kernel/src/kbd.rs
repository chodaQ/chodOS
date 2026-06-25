//! PS/2 키보드 드라이버 — 스캔코드 Set 1 → ASCII 변환
//!
//! IRQ1 핸들러가 `push_key()`로 ASCII를 버퍼에 쌓으면,
//! `sys_read(fd=0)`가 `read_key_blocking()`으로 꺼낸다.
//! 시리얼(COM1)과 병행 동작 — GUI 모드/텍스트 모드 모두 지원.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

// ── 링 버퍼 ──────────────────────────────────────────────────────────────────

const BUF_CAP: usize = 64;

// no_std에서 AtomicU8 배열을 const 초기화
struct KbdBuf {
    buf:  [AtomicU8;   BUF_CAP],
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: 단일 코어 커널 — 인터럽트와 main이 번갈아 접근
unsafe impl Sync for KbdBuf {}

static KBD: KbdBuf = KbdBuf {
    buf:  [const { AtomicU8::new(0) }; BUF_CAP],
    head: AtomicUsize::new(0),
    tail: AtomicUsize::new(0),
};

pub fn push_key(ascii: u8) {
    let tail = KBD.tail.load(Ordering::Relaxed);
    let next = (tail + 1) % BUF_CAP;
    if next == KBD.head.load(Ordering::Relaxed) { return; } // 버퍼 풀
    KBD.buf[tail].store(ascii, Ordering::Relaxed);
    KBD.tail.store(next, Ordering::Relaxed);
    // BETA-X 4: 포그라운드 앱 직통 경로에도 push (IRQ-safe AtomicU64 링 버퍼)
    crate::input_direct::push_key(ascii);
}

pub fn try_pop() -> Option<u8> {
    let head = KBD.head.load(Ordering::Relaxed);
    if head == KBD.tail.load(Ordering::Relaxed) { return None; }
    let byte = KBD.buf[head].load(Ordering::Relaxed);
    KBD.head.store((head + 1) % BUF_CAP, Ordering::Relaxed);
    Some(byte)
}

/// 버퍼에 데이터가 있는지 확인 (소비하지 않음) — select/poll/epoll용
pub fn has_key() -> bool {
    KBD.head.load(Ordering::Relaxed) != KBD.tail.load(Ordering::Relaxed)
}

/// 키보드 버퍼 또는 시리얼(COM1)에서 한 바이트를 블로킹으로 읽는다.
pub fn read_key_blocking() -> u8 {
    loop {
        if let Some(k) = try_pop() { return k; }
        if let Some(b) = crate::serial::try_read_byte() { return b; }
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }
}

// ── Shift / Caps Lock 상태 ────────────────────────────────────────────────────

static SHIFT:    AtomicBool = AtomicBool::new(false);
static CAPS:     AtomicBool = AtomicBool::new(false);

// ── 스캔코드 Set 1 → ASCII 테이블 (US QWERTY) ────────────────────────────────
//
// 인덱스 = 스캔코드(make code, 0x00-0x7F)
// 0x00 = 없음(특수키), 나머지 = ASCII 값
// Shift 테이블은 숫자/기호 행과 알파벳 대문자를 포함

const LOWER: [u8; 128] = {
    let mut t = [0u8; 128];
    // 0x01: ESC
    t[0x01] = 0x1B;
    // 숫자 행
    t[0x02] = b'1'; t[0x03] = b'2'; t[0x04] = b'3'; t[0x05] = b'4';
    t[0x06] = b'5'; t[0x07] = b'6'; t[0x08] = b'7'; t[0x09] = b'8';
    t[0x0A] = b'9'; t[0x0B] = b'0'; t[0x0C] = b'-'; t[0x0D] = b'=';
    // 0x0E: Backspace, 0x0F: Tab
    t[0x0E] = 0x08; t[0x0F] = b'\t';
    // QWERTY 상단
    t[0x10] = b'q'; t[0x11] = b'w'; t[0x12] = b'e'; t[0x13] = b'r';
    t[0x14] = b't'; t[0x15] = b'y'; t[0x16] = b'u'; t[0x17] = b'i';
    t[0x18] = b'o'; t[0x19] = b'p'; t[0x1A] = b'['; t[0x1B] = b']';
    // 0x1C: Enter, 0x1D: LCtrl
    t[0x1C] = b'\n';
    // QWERTY 중단
    t[0x1E] = b'a'; t[0x1F] = b's'; t[0x20] = b'd'; t[0x21] = b'f';
    t[0x22] = b'g'; t[0x23] = b'h'; t[0x24] = b'j'; t[0x25] = b'k';
    t[0x26] = b'l'; t[0x27] = b';'; t[0x28] = b'\'';
    t[0x29] = b'`'; t[0x2B] = b'\\';
    // 0x2A: L-Shift, 0x36: R-Shift (별도 처리)
    // QWERTY 하단
    t[0x2C] = b'z'; t[0x2D] = b'x'; t[0x2E] = b'c'; t[0x2F] = b'v';
    t[0x30] = b'b'; t[0x31] = b'n'; t[0x32] = b'm';
    t[0x33] = b','; t[0x34] = b'.'; t[0x35] = b'/';
    // 0x39: Space
    t[0x39] = b' ';
    // 숫자패드 *
    t[0x37] = b'*';
    t
};

const UPPER: [u8; 128] = {
    let mut t = [0u8; 128];
    // Shift + 숫자 행 → 특수문자
    t[0x02] = b'!'; t[0x03] = b'@'; t[0x04] = b'#'; t[0x05] = b'$';
    t[0x06] = b'%'; t[0x07] = b'^'; t[0x08] = b'&'; t[0x09] = b'*';
    t[0x0A] = b'('; t[0x0B] = b')'; t[0x0C] = b'_'; t[0x0D] = b'+';
    t[0x0E] = 0x08; t[0x0F] = b'\t';
    // Shift + 알파벳 → 대문자
    t[0x10] = b'Q'; t[0x11] = b'W'; t[0x12] = b'E'; t[0x13] = b'R';
    t[0x14] = b'T'; t[0x15] = b'Y'; t[0x16] = b'U'; t[0x17] = b'I';
    t[0x18] = b'O'; t[0x19] = b'P'; t[0x1A] = b'{'; t[0x1B] = b'}';
    t[0x1C] = b'\n';
    t[0x1E] = b'A'; t[0x1F] = b'S'; t[0x20] = b'D'; t[0x21] = b'F';
    t[0x22] = b'G'; t[0x23] = b'H'; t[0x24] = b'J'; t[0x25] = b'K';
    t[0x26] = b'L'; t[0x27] = b':'; t[0x28] = b'"';
    t[0x29] = b'~'; t[0x2B] = b'|';
    t[0x2C] = b'Z'; t[0x2D] = b'X'; t[0x2E] = b'C'; t[0x2F] = b'V';
    t[0x30] = b'B'; t[0x31] = b'N'; t[0x32] = b'M';
    t[0x33] = b'<'; t[0x34] = b'>'; t[0x35] = b'?';
    t[0x39] = b' ';
    t[0x37] = b'*';
    t[0x01] = 0x1B;
    t
};

/// IRQ1 핸들러에서 호출 — 스캔코드를 ASCII로 변환해 버퍼에 push
pub fn handle_scancode(scancode: u8) {
    let is_break = scancode >= 0x80;
    let make     = (scancode & 0x7F) as usize;

    // Shift: 0x2A(L), 0x36(R)
    if make == 0x2A || make == 0x36 {
        SHIFT.store(!is_break, Ordering::Relaxed);
        return;
    }
    // Caps Lock 토글 (make only)
    if !is_break && make == 0x3A {
        let c = CAPS.load(Ordering::Relaxed);
        CAPS.store(!c, Ordering::Relaxed);
        return;
    }

    if is_break || make >= 128 { return; }

    let shift = SHIFT.load(Ordering::Relaxed);
    let caps  = CAPS.load(Ordering::Relaxed);

    // 알파벳 키: Shift XOR Caps → 대문자
    let is_alpha = matches!(make,
        0x10..=0x19 | 0x1E..=0x26 | 0x2C..=0x32);
    let use_upper = if is_alpha { shift ^ caps } else { shift };

    let ascii = if use_upper { UPPER[make] } else { LOWER[make] };
    if ascii != 0 {
        push_key(ascii);
    }
}
