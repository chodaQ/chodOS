//! Framebuffer driver — limine linear framebuffer 직접 쓰기
//! 8×8 비트맵 폰트(build/font8x8.bin)로 텍스트 렌더링 지원
//! 마우스 커서 렌더링 (12×18 화살표, 배경 픽셀 저장/복원)

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};

static FB_ADDR:   AtomicU64 = AtomicU64::new(0);
static FB_WIDTH:  AtomicU32 = AtomicU32::new(0);
static FB_HEIGHT: AtomicU32 = AtomicU32::new(0);
static FB_PITCH:  AtomicU32 = AtomicU32::new(0);
static FB_R_SH:   AtomicU32 = AtomicU32::new(16);
static FB_G_SH:   AtomicU32 = AtomicU32::new(8);
static FB_B_SH:   AtomicU32 = AtomicU32::new(0);

// build/font8x8.bin: 128글자 × 8바이트 = 1024 바이트 (gen_font.py 생성)
static FONT: &[u8] = include_bytes!(
    concat!(env!("CARGO_MANIFEST_DIR"), "/../build/font8x8.bin")
);

pub fn init(
    addr: usize, width: u32, height: u32, pitch: u32,
    r_shift: u8, g_shift: u8, b_shift: u8,
) {
    FB_ADDR.store(addr as u64, Ordering::Relaxed);
    FB_WIDTH.store(width,      Ordering::Relaxed);
    FB_HEIGHT.store(height,    Ordering::Relaxed);
    FB_PITCH.store(pitch,      Ordering::Relaxed);
    FB_R_SH.store(r_shift as u32, Ordering::Relaxed);
    FB_G_SH.store(g_shift as u32, Ordering::Relaxed);
    FB_B_SH.store(b_shift as u32, Ordering::Relaxed);
}

pub fn width()  -> u32 { FB_WIDTH.load(Ordering::Relaxed) }
pub fn height() -> u32 { FB_HEIGHT.load(Ordering::Relaxed) }

/// R/G/B 값을 프레임버퍼 픽셀 형식으로 변환
pub fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << FB_R_SH.load(Ordering::Relaxed))
  | ((g as u32) << FB_G_SH.load(Ordering::Relaxed))
  | ((b as u32) << FB_B_SH.load(Ordering::Relaxed))
}

pub fn put_pixel(x: u32, y: u32, color: u32) {
    let addr  = FB_ADDR.load(Ordering::Relaxed) as usize;
    if addr == 0 { return; }
    let pitch = FB_PITCH.load(Ordering::Relaxed) as usize;
    let w     = FB_WIDTH.load(Ordering::Relaxed);
    let h     = FB_HEIGHT.load(Ordering::Relaxed);
    if x >= w || y >= h { return; }
    let off = y as usize * pitch + x as usize * 4;
    unsafe { *((addr + off) as *mut u32) = color; }
}

pub fn fill_rect(x: u32, y: u32, w: u32, h: u32, color: u32) {
    for row in y..y.saturating_add(h) {
        for col in x..x.saturating_add(w) {
            put_pixel(col, row, color);
        }
    }
}

/// 한 글자 그리기 (8×8 픽셀). LSB = 왼쪽 픽셀.
pub fn draw_char(cx: u32, cy: u32, c: u8, fg: u32, bg: u32) {
    let idx = (c.min(127) as usize) * 8;
    for row in 0u32..8 {
        let byte = FONT[idx + row as usize];
        for col in 0u32..8 {
            let color = if byte & (1 << col) != 0 { fg } else { bg };
            put_pixel(cx + col, cy + row, color);
        }
    }
}

pub fn draw_text(x: u32, y: u32, text: &str, fg: u32, bg: u32) {
    let mut cx = x;
    for c in text.bytes() {
        draw_char(cx, y, c, fg, bg);
        cx += 8;
    }
}

pub fn clear(color: u32) {
    fill_rect(0, 0, FB_WIDTH.load(Ordering::Relaxed), FB_HEIGHT.load(Ordering::Relaxed), color);
}

/// 프레임버퍼에서 픽셀 색상 읽기 (커서 배경 저장용)
pub fn read_pixel(x: u32, y: u32) -> u32 {
    let addr  = FB_ADDR.load(Ordering::Relaxed) as usize;
    if addr == 0 { return 0; }
    let pitch = FB_PITCH.load(Ordering::Relaxed) as usize;
    let w     = FB_WIDTH.load(Ordering::Relaxed);
    let h     = FB_HEIGHT.load(Ordering::Relaxed);
    if x >= w || y >= h { return 0; }
    let off = y as usize * pitch + x as usize * 4;
    unsafe { *((addr + off) as *const u32) }
}

// ── 마우스 커서 (12×18 화살표) ───────────────────────────────────────────────
//
// 0 = 투명(배경 보존), 1 = 검정(아웃라인), 2 = 흰색(내부)
//
pub const CW: usize = 12;
pub const CH: usize = 18;

const CURSOR_MAP: [[u8; CW]; CH] = [
    [1,0,0,0,0,0,0,0,0,0,0,0],
    [1,2,0,0,0,0,0,0,0,0,0,0],
    [1,2,2,0,0,0,0,0,0,0,0,0],
    [1,2,2,2,0,0,0,0,0,0,0,0],
    [1,2,2,2,2,0,0,0,0,0,0,0],
    [1,2,2,2,2,2,0,0,0,0,0,0],
    [1,2,2,2,2,2,2,0,0,0,0,0],
    [1,2,2,2,2,2,2,2,0,0,0,0],
    [1,2,2,2,2,2,2,2,2,0,0,0],
    [1,2,2,2,2,2,2,2,2,2,0,0],
    [1,2,2,2,2,2,2,2,2,2,2,0],
    [1,2,2,2,2,2,2,0,0,0,0,0],
    [1,2,2,2,1,2,2,2,0,0,0,0],
    [1,2,2,1,0,0,1,2,2,0,0,0],
    [1,2,1,0,0,0,0,1,2,2,0,0],
    [1,1,0,0,0,0,0,0,1,2,2,0],
    [0,0,0,0,0,0,0,0,0,1,2,1],
    [0,0,0,0,0,0,0,0,0,0,1,0],
];

// 커서 아래 배경 픽셀 저장 버퍼
// SAFETY: 단일 코어, INT_GATE(IF=0)에서만 접근 — 재진입 없음
static mut CURSOR_BG: [u32; CW * CH] = [0u32; CW * CH];
static CURSOR_PREV_X: AtomicI32 = AtomicI32::new(-1);
static CURSOR_PREV_Y: AtomicI32 = AtomicI32::new(-1);

/// 마우스 커서 이동: 이전 위치 복원 → 새 위치에 커서 그리기
pub fn draw_cursor(x: i32, y: i32) {
    if FB_ADDR.load(Ordering::Relaxed) == 0 { return; }

    let black = rgb(0, 0, 0);
    let white = rgb(255, 255, 255);

    // ── 이전 위치 배경 복원 ───────────────────────────────────────────────
    let px = CURSOR_PREV_X.load(Ordering::Relaxed);
    let py = CURSOR_PREV_Y.load(Ordering::Relaxed);
    if px >= 0 && py >= 0 {
        for row in 0..CH {
            for col in 0..CW {
                if CURSOR_MAP[row][col] == 0 { continue; }
                let sx = px + col as i32;
                let sy = py + row as i32;
                if sx >= 0 && sy >= 0 {
                    let saved = unsafe { CURSOR_BG[row * CW + col] };
                    put_pixel(sx as u32, sy as u32, saved);
                }
            }
        }
    }

    // ── 새 위치 배경 저장 ─────────────────────────────────────────────────
    for row in 0..CH {
        for col in 0..CW {
            if CURSOR_MAP[row][col] == 0 { continue; }
            let sx = x + col as i32;
            let sy = y + row as i32;
            let pixel = if sx >= 0 && sy >= 0 {
                read_pixel(sx as u32, sy as u32)
            } else { 0 };
            unsafe { CURSOR_BG[row * CW + col] = pixel; }
        }
    }

    // ── 커서 픽셀 그리기 ──────────────────────────────────────────────────
    for row in 0..CH {
        for col in 0..CW {
            let color = match CURSOR_MAP[row][col] {
                1 => black,
                2 => white,
                _ => continue,
            };
            let sx = x + col as i32;
            let sy = y + row as i32;
            if sx >= 0 && sy >= 0 {
                put_pixel(sx as u32, sy as u32, color);
            }
        }
    }

    CURSOR_PREV_X.store(x, Ordering::Relaxed);
    CURSOR_PREV_Y.store(y, Ordering::Relaxed);
}
