//! PS/2 마우스 드라이버 (i8042 컨트롤러, IRQ12)
//!
//! 패킷 형식 (Set 1, 3바이트):
//!   byte0: [Yo|Xo|Ys|Xs|1|M|R|L]  (Ys/Xs=부호, Yo/Xo=오버플로, bit3=항상 1)
//!   byte1: X 이동량 (부호: byte0[4])
//!   byte2: Y 이동량 (부호: byte0[5], 위 = 양수)

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicUsize, Ordering};

const DATA_PORT: u16 = 0x60;
const CMD_PORT:  u16 = 0x64;

pub static MOUSE_X:   AtomicI32 = AtomicI32::new(0);
pub static MOUSE_Y:   AtomicI32 = AtomicI32::new(0);
pub static MOUSE_BTN: AtomicU8  = AtomicU8::new(0);

// 클릭 엣지 감지용: 이전 버튼 상태
static PREV_BTN: AtomicU8 = AtomicU8::new(0);

// 3바이트 패킷 누적
static PKT:     [AtomicU8; 3] = [const { AtomicU8::new(0) }; 3];
static PKT_IDX: AtomicUsize   = AtomicUsize::new(0);

// 초기화 완료 플래그
static READY: AtomicBool = AtomicBool::new(false);

// ── 저수준 i8042 I/O ──────────────────────────────────────────────────────────

unsafe fn wait_write() {
    let mut i = 0u32;
    loop {
        let s: u8;
        core::arch::asm!("in al, dx", out("al") s, in("dx") CMD_PORT, options(nomem, nostack));
        if s & 0x02 == 0 { break; }
        i += 1;
        if i > 200_000 { break; }
    }
}

unsafe fn wait_read() {
    let mut i = 0u32;
    loop {
        let s: u8;
        core::arch::asm!("in al, dx", out("al") s, in("dx") CMD_PORT, options(nomem, nostack));
        if s & 0x01 != 0 { break; }
        i += 1;
        if i > 200_000 { break; }
    }
}

unsafe fn cmd(c: u8) {
    wait_write();
    core::arch::asm!("out dx, al", in("dx") CMD_PORT, in("al") c, options(nomem, nostack));
}

unsafe fn write_data(d: u8) {
    wait_write();
    core::arch::asm!("out dx, al", in("dx") DATA_PORT, in("al") d, options(nomem, nostack));
}

unsafe fn read_data() -> u8 {
    wait_read();
    let v: u8;
    core::arch::asm!("in al, dx", out("al") v, in("dx") DATA_PORT, options(nomem, nostack));
    v
}

// 마우스로 바이트 전송 (0xD4 명령 → 0x60에 데이터)
unsafe fn mouse_send(byte: u8) -> u8 {
    cmd(0xD4);
    write_data(byte);
    read_data() // ACK
}

// ── 초기화 ────────────────────────────────────────────────────────────────────

pub fn init() {
    unsafe {
        // 1. 기존 출력 버퍼 플러시 (오래된 데이터 제거)
        for _ in 0..16 {
            let s: u8;
            core::arch::asm!("in al, dx", out("al") s, in("dx") CMD_PORT, options(nomem, nostack));
            if s & 0x01 == 0 { break; }
            let _: u8;
            core::arch::asm!("in al, dx", out("al") _, in("dx") DATA_PORT, options(nomem, nostack));
        }

        // 2. 마우스 포트 활성화 (0xA8)
        cmd(0xA8);

        // 3. CCB 읽기 → IRQ12(bit1=1) + 마우스 클럭(bit5=0) 설정
        cmd(0x20);
        let ccb = read_data();
        let ccb_new = (ccb | 0x02) & !0x20;
        cmd(0x60);
        write_data(ccb_new);

        // 4. 마우스 리셋 (0xFF) — ACK 유무와 상관없이 응답 버퍼 비우기
        cmd(0xD4); write_data(0xFF);
        // 짧은 지연 후 버퍼 flush (ACK + BAT + ID 최대 3바이트)
        for _ in 0..3 {
            let s: u8;
            core::arch::asm!("in al, dx", out("al") s, in("dx") CMD_PORT, options(nomem, nostack));
            if s & 0x01 == 0 { break; }
            let _: u8;
            core::arch::asm!("in al, dx", out("al") _, in("dx") DATA_PORT, options(nomem, nostack));
            // 짧은 대기 (~100µs 수준)
            for _ in 0u32..10_000 {}
        }

        // 5. 기본 설정 (0xF6) — ACK 무시
        cmd(0xD4); write_data(0xF6);
        for _ in 0u32..50_000 {}
        // 버퍼 flush
        let s: u8;
        core::arch::asm!("in al, dx", out("al") s, in("dx") CMD_PORT, options(nomem, nostack));
        if s & 0x01 != 0 {
            let _: u8;
            core::arch::asm!("in al, dx", out("al") _, in("dx") DATA_PORT, options(nomem, nostack));
        }

        // 6. 스트리밍 활성화 (0xF4)
        cmd(0xD4); write_data(0xF4);
        for _ in 0u32..50_000 {}
        // ACK 읽기 시도 (있으면 읽고, 없으면 무시)
        let s: u8;
        core::arch::asm!("in al, dx", out("al") s, in("dx") CMD_PORT, options(nomem, nostack));
        if s & 0x01 != 0 {
            let _: u8;
            core::arch::asm!("in al, dx", out("al") _, in("dx") DATA_PORT, options(nomem, nostack));
        }
    }

    // 커서를 화면 중앙으로 초기화
    let w = crate::fb::width();
    let h = crate::fb::height();
    if w > 0 && h > 0 {
        MOUSE_X.store(w as i32 / 2, Ordering::Relaxed);
        MOUSE_Y.store(h as i32 / 2, Ordering::Relaxed);
    }

    READY.store(true, Ordering::Relaxed);
    crate::serial_println!("[mouse] PS/2 mouse ready — cursor at ({}, {})",
        MOUSE_X.load(Ordering::Relaxed), MOUSE_Y.load(Ordering::Relaxed));
}

// ── IRQ12 핸들러 ──────────────────────────────────────────────────────────────

pub fn handle_irq() {
    let byte: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") byte, in("dx") DATA_PORT, options(nomem, nostack));
    }

    let idx = PKT_IDX.load(Ordering::Relaxed);

    // 첫 번째 바이트 동기화: bit3이 반드시 1이어야 함
    if idx == 0 && (byte & 0x08) == 0 { return; }

    PKT[idx].store(byte, Ordering::Relaxed);
    let next = idx + 1;

    if next == 3 {
        PKT_IDX.store(0, Ordering::Relaxed);
        process_packet();
    } else {
        PKT_IDX.store(next, Ordering::Relaxed);
    }
}

fn process_packet() {
    if !READY.load(Ordering::Relaxed) { return; }

    let b0 = PKT[0].load(Ordering::Relaxed);
    let b1 = PKT[1].load(Ordering::Relaxed);
    let b2 = PKT[2].load(Ordering::Relaxed);

    // X/Y 오버플로 비트 — 패킷이 손상됐으므로 무시
    if b0 & 0xC0 != 0 { return; }

    let dx: i32 = if b0 & 0x10 != 0 { b1 as i32 - 256 } else { b1 as i32 };
    let dy: i32 = if b0 & 0x20 != 0 { b2 as i32 - 256 } else { b2 as i32 };

    let w = crate::fb::width() as i32;
    let h = crate::fb::height() as i32;
    if w == 0 || h == 0 { return; }

    let x = (MOUSE_X.load(Ordering::Relaxed) + dx).max(0).min(w - 1);
    let y = (MOUSE_Y.load(Ordering::Relaxed) - dy).max(0).min(h - 1); // Y 반전
    MOUSE_X.store(x, Ordering::Relaxed);
    MOUSE_Y.store(y, Ordering::Relaxed);

    let btn = b0 & 0x07; // [M|R|L]
    MOUSE_BTN.store(btn, Ordering::Relaxed);

    let prev = PREV_BTN.load(Ordering::Relaxed);

    // 왼쪽 버튼 rising edge → 클릭
    if btn & 0x01 != 0 && prev & 0x01 == 0 {
        on_click(x, y);
    }
    // 왼쪽 버튼 홀드 + 이동 → 드래그
    if btn & 0x01 != 0 && (dx != 0 || dy != 0) {
        crate::wm::on_drag(x, y);
    }
    // 왼쪽 버튼 falling edge → 드래그 종료
    if prev & 0x01 != 0 && btn & 0x01 == 0 {
        crate::wm::on_release();
    }

    PREV_BTN.store(btn, Ordering::Relaxed);

    // 커서 다시 그리기 (드래그 중엔 on_drag 내부에서 이미 redraw하므로 중복되지만 무해)
    crate::fb::draw_cursor(x, y);
}

fn on_click(x: i32, y: i32) {
    crate::serial_println!("[mouse] click @ ({}, {})", x, y);
    crate::wm::on_click(x as u32, y as u32);
}
