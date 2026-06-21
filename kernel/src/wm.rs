//! MuWM — 인터랙티브 창 관리자 (BETA 2)
//!
//! ## 이벤트 흐름
//! mouse::IRQ12 → process_packet() → on_click / on_drag / on_release → wm
//!
//! ## 기능
//! - 창 드래그 (타이틀바 홀드+이동)
//! - 닫기 버튼 (X) → 창 숨기기
//! - MuStart 클릭 → 모든 창 원래 위치로 복원
//! - 포커스 강조 (타이틀바 색상 변경)

use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use crate::fb;

// ── 창 정의 (크기/타이틀은 고정, 위치/가시성은 동적) ──────────────────────────

const WIN_COUNT: usize = 3;

const WIN_W:      [u32;  WIN_COUNT] = [290, 290, 600];
const WIN_H:      [u32;  WIN_COUNT] = [200, 200, 130];
const WIN_TITLES: [&str; WIN_COUNT] = ["MuKernel Info", "MuShell", "System Monitor"];

const WIN_INIT_X: [i32; WIN_COUNT] = [20, 330, 20];
const WIN_INIT_Y: [i32; WIN_COUNT] = [20, 20,  240];

// ── 동적 상태 ────────────────────────────────────────────────────────────────

static WIN_X: [AtomicI32; WIN_COUNT] = [
    AtomicI32::new(20), AtomicI32::new(330), AtomicI32::new(20),
];
static WIN_Y: [AtomicI32; WIN_COUNT] = [
    AtomicI32::new(20), AtomicI32::new(20), AtomicI32::new(240),
];
static WIN_VIS: [AtomicBool; WIN_COUNT] = [
    AtomicBool::new(true), AtomicBool::new(true), AtomicBool::new(true),
];

// 드래그 상태: -1 = 없음, 0-2 = 드래그 중인 창 인덱스
static DRAG_WIN: AtomicI32 = AtomicI32::new(-1);
static DRAG_OX:  AtomicI32 = AtomicI32::new(0);
static DRAG_OY:  AtomicI32 = AtomicI32::new(0);

// ── Window 헬퍼 ──────────────────────────────────────────────────────────────

pub struct Window {
    pub x: u32, pub y: u32,
    pub w: u32, pub h: u32,
    pub title: &'static str,
}

const TITLE_H: u32 = 16;
const BORDER:  u32 = 1;

impl Window {
    pub fn draw(&self) {
        let border_col = fb::rgb(80,  100, 200);
        let title_bg   = fb::rgb(45,  65,  145);
        let title_fg   = fb::rgb(255, 255, 255);
        let content_bg = fb::rgb(18,  18,  32);

        fb::fill_rect(self.x, self.y, self.w, self.h, border_col);
        fb::fill_rect(
            self.x + BORDER, self.y + BORDER,
            self.w.saturating_sub(BORDER * 2), TITLE_H,
            title_bg,
        );
        fb::draw_text(self.x + BORDER + 4, self.y + BORDER + 4,
            self.title, title_fg, title_bg);
        // 닫기 버튼
        let btn_x = self.x + self.w.saturating_sub(BORDER + 13);
        let btn_y = self.y + BORDER + 2;
        fb::fill_rect(btn_x, btn_y, 12, 12, fb::rgb(180, 40, 40));
        fb::draw_char(btn_x + 2, btn_y + 2, b'X',
            fb::rgb(255, 255, 255), fb::rgb(180, 40, 40));
        // 콘텐츠 영역
        let cont_top = self.y + BORDER + TITLE_H;
        let cont_h   = self.h.saturating_sub(BORDER + TITLE_H + BORDER);
        fb::fill_rect(
            self.x + BORDER, cont_top,
            self.w.saturating_sub(BORDER * 2), cont_h,
            content_bg,
        );
    }

    /// 타이틀바를 포커스된 색상으로 다시 그리기
    pub fn draw_focused(&self) {
        let focused_bg = fb::rgb(80, 110, 210);
        let title_fg   = fb::rgb(255, 255, 255);
        fb::fill_rect(
            self.x + BORDER, self.y + BORDER,
            self.w.saturating_sub(BORDER * 2), TITLE_H,
            focused_bg,
        );
        fb::draw_text(self.x + BORDER + 4, self.y + BORDER + 4,
            self.title, title_fg, focused_bg);
        // 닫기 버튼 유지
        let btn_x = self.x + self.w.saturating_sub(BORDER + 13);
        let btn_y = self.y + BORDER + 2;
        fb::fill_rect(btn_x, btn_y, 12, 12, fb::rgb(180, 40, 40));
        fb::draw_char(btn_x + 2, btn_y + 2, b'X',
            fb::rgb(255, 255, 255), fb::rgb(180, 40, 40));
    }

    pub fn draw_line(&self, row: u32, text: &str, fg: u32) {
        let cx = self.x + BORDER + 4;
        let cy = self.y + BORDER + TITLE_H + 4 + row * 10;
        fb::draw_text(cx, cy, text, fg, fb::rgb(18, 18, 32));
    }
}

// ── Rect 히트 테스트 ─────────────────────────────────────────────────────────

struct Rect { x: u32, y: u32, w: u32, h: u32 }

impl Rect {
    fn contains(&self, px: u32, py: u32) -> bool {
        px >= self.x && px < self.x + self.w &&
        py >= self.y && py < self.y + self.h
    }
    fn close_btn(&self) -> Rect {
        Rect {
            x: self.x + self.w.saturating_sub(BORDER + 13),
            y: self.y + BORDER + 2,
            w: 12, h: 12,
        }
    }
    fn title_bar(&self) -> Rect {
        Rect {
            x: self.x + BORDER,
            y: self.y + BORDER,
            w: self.w.saturating_sub(BORDER * 2),
            h: TITLE_H,
        }
    }
}

// ── 동적 상태에서 Window 빌드 ────────────────────────────────────────────────

fn win_rect(i: usize) -> Rect {
    Rect {
        x: WIN_X[i].load(Ordering::Relaxed) as u32,
        y: WIN_Y[i].load(Ordering::Relaxed) as u32,
        w: WIN_W[i],
        h: WIN_H[i],
    }
}

fn win_window(i: usize) -> Window {
    Window {
        x: WIN_X[i].load(Ordering::Relaxed) as u32,
        y: WIN_Y[i].load(Ordering::Relaxed) as u32,
        w: WIN_W[i],
        h: WIN_H[i],
        title: WIN_TITLES[i],
    }
}

// ── 데스크탑 렌더링 ──────────────────────────────────────────────────────────

pub fn render_desktop() {
    let sw = fb::width();
    let sh = fb::height();

    // 배경
    fb::clear(fb::rgb(12, 12, 28));
    for x in 0..sw {
        let t = (x * 30 / sw.max(1)) as u8;
        fb::put_pixel(x, 0, fb::rgb(20 + t, 20, 50 + t));
    }

    let white  = fb::rgb(220, 220, 220);
    let green  = fb::rgb(80,  220, 80);
    let cyan   = fb::rgb(80,  210, 210);
    let yellow = fb::rgb(220, 220, 60);
    let gray   = fb::rgb(140, 140, 160);

    // 창 0: 시스템 정보
    if WIN_VIS[0].load(Ordering::Relaxed) {
        let w = win_window(0);
        w.draw();
        w.draw_line(0, "OS:   MuKernel v0.1.0-alpha", white);
        w.draw_line(1, "Arch: x86_64  (UEFI ALPHA)", white);
        w.draw_line(2, "Boot: Limine v8.x", white);
        w.draw_line(3, "GUI:  BETA 2 (Interactive WM)", green);
        w.draw_line(4, "RAM:  256 MB", white);
        w.draw_line(5, "Font: 8x8 bitmap (128 glyphs)", white);
        w.draw_line(6, "WM:   MuWM v0.2  drag+close", cyan);
        w.draw_line(7, "PKG:  mukg 0.1.0  5 pkgs", cyan);
        w.draw_line(8, "(c) 2026 MuKernel Project", gray);
    }

    // 창 1: 라이브 터미널 (term.rs 버퍼를 직접 렌더링)
    if WIN_VIS[1].load(Ordering::Relaxed) {
        let w = win_window(1);
        w.draw();
        if let Some((cx, cy)) = shell_content_origin() {
            crate::term::render_at(cx, cy);
        }
    }

    // 창 2: 시스템 모니터
    if WIN_VIS[2].load(Ordering::Relaxed) {
        let w = win_window(2);
        w.draw();
        w.draw_line(0, "CPU [####......] 42%   MEM [######....] 58%", yellow);
        w.draw_line(1, "Tasks: 4 running   Uptime: ~3s   IRQ: OK", white);
        w.draw_line(2, "VirtIO: blk+net OK   IPC: zero-copy cap", cyan);
        w.draw_line(3, "Syscalls: musl-static (syscall instr) OK", gray);
        w.draw_line(4, "ext4 mounted   tmpfs OK   mukg: 5 pkgs", white);
    }

    // 태스크바
    let tb_y = sh.saturating_sub(22);
    fb::fill_rect(0, tb_y, sw, 22, fb::rgb(28, 28, 46));
    fb::fill_rect(0, tb_y, sw, 1, fb::rgb(60, 70, 140));

    let tb_bg = fb::rgb(28, 28, 46);
    fb::fill_rect(2, tb_y + 2, 56, 18, fb::rgb(40, 60, 130));
    fb::draw_text(6, tb_y + 7, "MuStart", white, fb::rgb(40, 60, 130));

    draw_taskbar_btn(66,  tb_y, "MuKernel", WIN_VIS[0].load(Ordering::Relaxed), tb_bg);
    draw_taskbar_btn(138, tb_y, "MuShell",  WIN_VIS[1].load(Ordering::Relaxed), tb_bg);
    draw_taskbar_btn(210, tb_y, "Monitor",  WIN_VIS[2].load(Ordering::Relaxed), tb_bg);
    fb::draw_text(sw.saturating_sub(72), tb_y + 7, "BETA 2", cyan, tb_bg);
}

fn draw_taskbar_btn(x: u32, tb_y: u32, label: &'static str, active: bool, _bg: u32) {
    let color = if active { fb::rgb(50, 50, 90) } else { fb::rgb(35, 35, 55) };
    let fg    = if active { fb::rgb(200, 200, 220) } else { fb::rgb(110, 110, 130) };
    fb::fill_rect(x, tb_y + 2, 68, 18, color);
    fb::draw_text(x + 4, tb_y + 7, label, fg, color);
}

// ── 이벤트 핸들러 ────────────────────────────────────────────────────────────

/// 마우스 왼쪽 버튼 클릭 (rising edge)
pub fn on_click(cx: u32, cy: u32) {
    let sh = fb::height();
    let tb_y = sh.saturating_sub(22);

    // 태스크바
    if cy >= tb_y {
        // MuStart 버튼 (x: 2..58)
        if cx >= 2 && cx < 58 {
            for i in 0..WIN_COUNT {
                WIN_VIS[i].store(true, Ordering::Relaxed);
                WIN_X[i].store(WIN_INIT_X[i], Ordering::Relaxed);
                WIN_Y[i].store(WIN_INIT_Y[i], Ordering::Relaxed);
            }
            crate::serial_println!("[wm] MuStart: all windows restored");
            render_desktop();
            redraw_cursor();
            return;
        }
        // 태스크바 창 버튼 (토글)
        let btn_starts = [66u32, 138, 210];
        for (i, &bx) in btn_starts.iter().enumerate() {
            if cx >= bx && cx < bx + 68 {
                let vis = WIN_VIS[i].load(Ordering::Relaxed);
                WIN_VIS[i].store(!vis, Ordering::Relaxed);
                crate::serial_println!("[wm] taskbar: '{}' {}",
                    WIN_TITLES[i], if !vis { "shown" } else { "hidden" });
                render_desktop();
                redraw_cursor();
                return;
            }
        }
        crate::serial_println!("[wm] taskbar click @ ({}, {})", cx, cy);
        return;
    }

    // 창 히트 테스트 (역순: 위에 있는 창 우선)
    for i in (0..WIN_COUNT).rev() {
        if !WIN_VIS[i].load(Ordering::Relaxed) { continue; }
        let rect = win_rect(i);
        if !rect.contains(cx, cy) { continue; }

        // 닫기 버튼
        if rect.close_btn().contains(cx, cy) {
            WIN_VIS[i].store(false, Ordering::Relaxed);
            crate::serial_println!("[wm] '{}' closed", WIN_TITLES[i]);
            render_desktop();
            redraw_cursor();
            return;
        }

        // 타이틀바 → 드래그 시작 + 포커스 강조
        if rect.title_bar().contains(cx, cy) {
            DRAG_WIN.store(i as i32, Ordering::Relaxed);
            DRAG_OX.store(cx as i32 - WIN_X[i].load(Ordering::Relaxed), Ordering::Relaxed);
            DRAG_OY.store(cy as i32 - WIN_Y[i].load(Ordering::Relaxed), Ordering::Relaxed);
            win_window(i).draw_focused();
            crate::serial_println!("[wm] '{}' drag-start", WIN_TITLES[i]);
            return;
        }

        // 콘텐츠 클릭 → 포커스만
        win_window(i).draw_focused();
        crate::serial_println!("[wm] click inside '{}' @ ({}, {})",
            WIN_TITLES[i], cx, cy);
        return;
    }
}

/// 마우스 이동 중 왼쪽 버튼 홀드 (드래그)
pub fn on_drag(x: i32, y: i32) {
    let drag = DRAG_WIN.load(Ordering::Relaxed);
    if drag < 0 { return; }
    let i  = drag as usize;
    let ox = DRAG_OX.load(Ordering::Relaxed);
    let oy = DRAG_OY.load(Ordering::Relaxed);

    let sw = fb::width() as i32;
    let sh = fb::height() as i32;
    let new_x = (x - ox).max(0).min(sw - WIN_W[i] as i32);
    let new_y = (y - oy).max(0).min(sh - 22 - WIN_H[i] as i32);

    WIN_X[i].store(new_x, Ordering::Relaxed);
    WIN_Y[i].store(new_y, Ordering::Relaxed);

    render_desktop();
    redraw_cursor();
}

/// 마우스 왼쪽 버튼 뗌 (드래그 종료)
pub fn on_release() {
    let drag = DRAG_WIN.load(Ordering::Relaxed);
    if drag >= 0 {
        let i = drag as usize;
        crate::serial_println!("[wm] '{}' dropped → ({}, {})",
            WIN_TITLES[i],
            WIN_X[i].load(Ordering::Relaxed),
            WIN_Y[i].load(Ordering::Relaxed));
        DRAG_WIN.store(-1, Ordering::Relaxed);
    }
}

/// MuShell 창(index 1) 콘텐츠 영역의 왼쪽 위 좌표를 반환.
/// 창이 보이지 않으면 None.
pub fn shell_content_origin() -> Option<(u32, u32)> {
    const I: usize = 1;
    if !WIN_VIS[I].load(Ordering::Relaxed) { return None; }
    let wx = WIN_X[I].load(Ordering::Relaxed) as u32;
    let wy = WIN_Y[I].load(Ordering::Relaxed) as u32;
    Some((wx + BORDER + 4, wy + BORDER + TITLE_H + 4))
}

/// 렌더 후 커서를 현재 위치에 다시 그리기
fn redraw_cursor() {
    let mx = crate::mouse::MOUSE_X.load(Ordering::Relaxed);
    let my = crate::mouse::MOUSE_Y.load(Ordering::Relaxed);
    crate::fb::draw_cursor(mx, my);
}
