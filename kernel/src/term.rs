//! GUI 터미널 버퍼 — MuShell 창 (window index 1) 전용
//!
//! `sys_write(fd=1/2, ...)` 호출 시 이 버퍼에 텍스트가 쌓이고,
//! `repaint()`가 호출되면 창 1의 콘텐츠 영역에 렌더링된다.
//!
//! ## 처리하는 제어 문자
//! - `\n` (0x0A): 줄 바꿈 + 스크롤
//! - `\r` (0x0D): 커서를 줄 처음으로
//! - `\x08` (0x08): 커서 왼쪽 이동 (backspace)
//! - `\x1B` (ESC): ANSI 시퀀스 skip (단순 무시)

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

// ── 그리드 크기 ─────────────────────────────────────────────────────────────
//
// 창 1: w=290, h=200
// 콘텐츠: x+5, y+21, w≈280, h≈174
// 8×8 폰트, 줄 간격 10px → 34 cols × 17 rows

const COLS: usize = 34;
const ROWS: usize = 17;
const TOTAL: usize = COLS * ROWS;

// ── 정적 버퍼 ────────────────────────────────────────────────────────────────

static TERM_BUF: [AtomicU8;  TOTAL] = [const { AtomicU8::new(b' ') }; TOTAL];
static TERM_COL: AtomicUsize = AtomicUsize::new(0);
static TERM_ROW: AtomicUsize = AtomicUsize::new(0);

// ESC 시퀀스 skip 상태
static ESC_STATE: AtomicU8 = AtomicU8::new(0); // 0=normal,1=ESC,2=CSI

// ── 버퍼 접근 ────────────────────────────────────────────────────────────────

#[inline]
fn cell(r: usize, c: usize) -> &'static AtomicU8 {
    &TERM_BUF[r * COLS + c]
}

// ── 스크롤 ───────────────────────────────────────────────────────────────────

fn scroll_up() {
    for r in 0..ROWS - 1 {
        for c in 0..COLS {
            let b = cell(r + 1, c).load(Ordering::Relaxed);
            cell(r, c).store(b, Ordering::Relaxed);
        }
    }
    for c in 0..COLS {
        cell(ROWS - 1, c).store(b' ', Ordering::Relaxed);
    }
    TERM_ROW.store(ROWS - 1, Ordering::Relaxed);
}

// ── 바이트 쓰기 ──────────────────────────────────────────────────────────────

pub fn write_byte(b: u8) {
    let esc = ESC_STATE.load(Ordering::Relaxed);
    match esc {
        1 => {
            // ESC received — next byte determines sequence type
            if b == b'[' {
                ESC_STATE.store(2, Ordering::Relaxed); // CSI
            } else {
                ESC_STATE.store(0, Ordering::Relaxed); // unknown, reset
            }
            return;
        }
        2 => {
            // CSI sequence — skip until letter (A-Z, a-z)
            if b.is_ascii_alphabetic() {
                ESC_STATE.store(0, Ordering::Relaxed);
            }
            return;
        }
        _ => {}
    }

    match b {
        0x1B => {
            ESC_STATE.store(1, Ordering::Relaxed);
        }
        b'\r' => {
            TERM_COL.store(0, Ordering::Relaxed);
        }
        b'\n' => {
            TERM_COL.store(0, Ordering::Relaxed);
            let r = TERM_ROW.load(Ordering::Relaxed);
            if r + 1 < ROWS {
                TERM_ROW.store(r + 1, Ordering::Relaxed);
            } else {
                scroll_up();
            }
        }
        0x08 => {
            // 커서 왼쪽 이동 (erase-left 시퀀스 "\x08 \x08" 에서 첫/세 번째)
            let col = TERM_COL.load(Ordering::Relaxed);
            if col > 0 {
                TERM_COL.store(col - 1, Ordering::Relaxed);
            }
        }
        0x07 => {} // BEL 무시
        b' '..=b'~' => {
            let r = TERM_ROW.load(Ordering::Relaxed);
            let c = TERM_COL.load(Ordering::Relaxed);
            if r < ROWS && c < COLS {
                cell(r, c).store(b, Ordering::Relaxed);
                let nc = c + 1;
                if nc >= COLS {
                    // 줄 넘김
                    TERM_COL.store(0, Ordering::Relaxed);
                    let nr = r + 1;
                    if nr < ROWS {
                        TERM_ROW.store(nr, Ordering::Relaxed);
                    } else {
                        scroll_up();
                    }
                } else {
                    TERM_COL.store(nc, Ordering::Relaxed);
                }
            }
        }
        _ => {} // 기타 제어 문자 무시
    }
}

/// 여러 바이트를 쓰고 한 번에 repaint
pub fn write_bytes(bytes: &[u8]) {
    for &b in bytes {
        write_byte(b);
    }
    repaint();
}

// ── 렌더링 ───────────────────────────────────────────────────────────────────

/// MuShell 창의 콘텐츠 영역에 현재 버퍼를 그린다.
/// `cx, cy`: 콘텐츠 영역 시작 좌표 (픽셀)
pub fn render_at(cx: u32, cy: u32) {
    let bg = crate::fb::rgb(18, 18, 32);
    let fg = crate::fb::rgb(220, 220, 220);
    let cur_fg = crate::fb::rgb(0, 255, 128); // 커서 색

    let cur_row = TERM_ROW.load(Ordering::Relaxed);
    let cur_col = TERM_COL.load(Ordering::Relaxed);

    // 콘텐츠 영역 배경 클리어
    crate::fb::fill_rect(cx, cy, (COLS as u32) * 8, (ROWS as u32) * 10, bg);

    for r in 0..ROWS {
        for c in 0..COLS {
            let ch = cell(r, c).load(Ordering::Relaxed);
            let px = cx + (c as u32) * 8;
            let py = cy + (r as u32) * 10;

            // 현재 커서 위치 표시 (블록)
            if r == cur_row && c == cur_col {
                crate::fb::draw_char(px, py, if ch == b' ' { b'_' } else { ch }, cur_fg, bg);
            } else if ch != b' ' {
                crate::fb::draw_char(px, py, ch, fg, bg);
            }
        }
    }
}

/// wm.rs가 shell 창 위치를 줄 때 쓰는 버전 (wm에서 호출)
pub fn repaint() {
    if let Some((cx, cy)) = crate::wm::shell_content_origin() {
        render_at(cx, cy);
    }
}
