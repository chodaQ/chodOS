//! MuWM — 기본 창 관리자 (정적 레이아웃)
//! framebuffer 드라이버(fb.rs) 위에서 동작

use crate::fb;

pub struct Window {
    pub x:     u32,
    pub y:     u32,
    pub w:     u32,
    pub h:     u32,
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

        // 테두리
        fb::fill_rect(self.x, self.y, self.w, self.h, border_col);
        // 제목 표시줄
        fb::fill_rect(
            self.x + BORDER, self.y + BORDER,
            self.w.saturating_sub(BORDER * 2), TITLE_H,
            title_bg,
        );
        // 제목 텍스트 (왼쪽 4px 여백)
        fb::draw_text(self.x + BORDER + 4, self.y + BORDER + 4,
            self.title, title_fg, title_bg);
        // 닫기 버튼 표시 (우측 끝 12×12)
        let btn_x = self.x + self.w.saturating_sub(BORDER + 13);
        let btn_y = self.y + BORDER + 2;
        fb::fill_rect(btn_x, btn_y, 12, 12, fb::rgb(180, 40, 40));
        fb::draw_char(btn_x + 2, btn_y + 2, b'X', fb::rgb(255, 255, 255), fb::rgb(180, 40, 40));
        // 콘텐츠 영역
        let cont_top = self.y + BORDER + TITLE_H;
        let cont_h   = self.h.saturating_sub(BORDER + TITLE_H + BORDER);
        fb::fill_rect(
            self.x + BORDER, cont_top,
            self.w.saturating_sub(BORDER * 2), cont_h,
            content_bg,
        );
    }

    /// 창 콘텐츠 영역 안에 한 줄 텍스트 그리기 (row=0부터, 행 간격 10px)
    pub fn draw_line(&self, row: u32, text: &str, fg: u32) {
        let cx = self.x + BORDER + 4;
        let cy = self.y + BORDER + TITLE_H + 4 + row * 10;
        // content_bg로 bg 처리
        fb::draw_text(cx, cy, text, fg, fb::rgb(18, 18, 32));
    }
}

/// 전체 데스크탑을 프레임버퍼에 그린다.
pub fn render_desktop() {
    let sw = fb::width();
    let sh = fb::height();

    // ── 배경 ──────────────────────────────────────────────────────────────
    fb::clear(fb::rgb(12, 12, 28));

    // 수평 그라디언트 효과: 상단 줄만 밝게
    for x in 0..sw {
        let t = (x * 30 / sw.max(1)) as u8;
        fb::put_pixel(x, 0, fb::rgb(20 + t, 20, 50 + t));
    }

    // ── 창 정의 ───────────────────────────────────────────────────────────
    let wins = [
        Window { x: 20,  y: 20,  w: 290, h: 200, title: "MuKernel Info" },
        Window { x: 330, y: 20,  w: 290, h: 200, title: "MuShell" },
        Window { x: 20,  y: 240, w: 600, h: 130, title: "System Monitor" },
    ];

    for win in &wins { win.draw(); }

    // ── 창 콘텐츠 ──────────────────────────────────────────────────────────
    let white  = fb::rgb(220, 220, 220);
    let green  = fb::rgb(80,  220, 80);
    let cyan   = fb::rgb(80,  210, 210);
    let yellow = fb::rgb(220, 220, 60);
    let gray   = fb::rgb(140, 140, 160);

    // 창 0: 시스템 정보
    wins[0].draw_line(0, "OS:   MuKernel v0.1.0-alpha", white);
    wins[0].draw_line(1, "Arch: x86_64  (UEFI ALPHA)", white);
    wins[0].draw_line(2, "Boot: Limine v8.x", white);
    wins[0].draw_line(3, "GUI:  ALPHA 17 (Framebuffer)", green);
    wins[0].draw_line(4, "RAM:  256 MB", white);
    wins[0].draw_line(5, "Font: 8x8 bitmap (128 glyphs)", white);
    wins[0].draw_line(6, "WM:   MuWM v0.1  3 windows", cyan);
    wins[0].draw_line(7, "PKG:  mukg 0.1.0  3 pkgs", cyan);
    wins[0].draw_line(8, "(c) 2026 MuKernel Project", gray);

    // 창 1: 쉘 세션 미리보기
    wins[1].draw_line(0, "mukernel$ mukg list", white);
    wins[1].draw_line(1, " sysinfo  0.1.0 - Sys info", cyan);
    wins[1].draw_line(2, " muecho   0.1.0 - Echo args", cyan);
    wins[1].draw_line(3, " mucat    0.1.0 - Read file", cyan);
    wins[1].draw_line(4, "mukernel$ sysinfo", white);
    wins[1].draw_line(5, " > MuKernel 0.1.0-alpha", green);
    wins[1].draw_line(6, " > Arch: x86_64 bare metal", green);
    wins[1].draw_line(7, "mukernel$ _", white);

    // 창 2: 시스템 모니터
    wins[2].draw_line(0, "CPU [####......] 42%   MEM [######....] 58%", yellow);
    wins[2].draw_line(1, "Tasks: 4 running   Uptime: ~3s   IRQ: OK", white);
    wins[2].draw_line(2, "VirtIO: blk+net OK   IPC: zero-copy cap", cyan);
    wins[2].draw_line(3, "Syscalls: write/getpid/mmap/exit/execve...", gray);
    wins[2].draw_line(4, "ext4 mounted   tmpfs OK   mukg: 3 pkgs", white);

    // ── 태스크바 ──────────────────────────────────────────────────────────
    let tb_y = sh.saturating_sub(22);
    fb::fill_rect(0, tb_y, sw, 22, fb::rgb(28, 28, 46));
    // 구분선
    fb::fill_rect(0, tb_y, sw, 1, fb::rgb(60, 70, 140));

    let tb_bg = fb::rgb(28, 28, 46);
    // 시작 버튼
    fb::fill_rect(2, tb_y + 2, 56, 18, fb::rgb(40, 60, 130));
    fb::draw_text(6, tb_y + 7, "MuStart", white, fb::rgb(40, 60, 130));
    // 열린 창 목록
    draw_taskbar_btn(66,  tb_y, "MuKernel", tb_bg);
    draw_taskbar_btn(138, tb_y, "MuShell",  tb_bg);
    draw_taskbar_btn(210, tb_y, "Monitor",  tb_bg);
    // 시계
    fb::draw_text(sw.saturating_sub(88), tb_y + 7, "ALPHA 17", cyan, tb_bg);
}

fn draw_taskbar_btn(x: u32, tb_y: u32, label: &'static str, bg: u32) {
    fb::fill_rect(x, tb_y + 2, 68, 18, fb::rgb(50, 50, 80));
    fb::draw_text(x + 4, tb_y + 7, label, fb::rgb(200, 200, 220), fb::rgb(50, 50, 80));
    let _ = bg;
}

// ── 클릭 이벤트 처리 ─────────────────────────────────────────────────────────
//
// render_desktop()과 동일한 창 레이아웃을 사용해야 한다.
// 창 정의를 여기서 직접 재사용한다.

struct Rect { x: u32, y: u32, w: u32, h: u32 }

impl Rect {
    fn contains(&self, px: u32, py: u32) -> bool {
        px >= self.x && px < self.x + self.w &&
        py >= self.y && py < self.y + self.h
    }
    // 닫기 버튼 영역
    fn close_btn(&self) -> Rect {
        Rect {
            x: self.x + self.w.saturating_sub(BORDER + 13),
            y: self.y + BORDER + 2,
            w: 12, h: 12,
        }
    }
    // 제목 표시줄 영역
    fn title_bar(&self) -> Rect {
        Rect {
            x: self.x + BORDER,
            y: self.y + BORDER,
            w: self.w.saturating_sub(BORDER * 2),
            h: TITLE_H,
        }
    }
}

pub fn on_click(cx: u32, cy: u32) {
    let sh = fb::height();
    let tb_y = sh.saturating_sub(22);

    // 창 레이아웃 (render_desktop과 동일)
    let rects = [
        Rect { x: 20,  y: 20,  w: 290, h: 200 },
        Rect { x: 330, y: 20,  w: 290, h: 200 },
        Rect { x: 20,  y: 240, w: 600, h: 130 },
    ];
    let titles = ["MuKernel Info", "MuShell", "System Monitor"];

    for (i, rect) in rects.iter().enumerate() {
        if !rect.contains(cx, cy) { continue; }

        // 닫기 버튼 클릭
        if rect.close_btn().contains(cx, cy) {
            crate::serial_println!("[wm] close '{}' clicked", titles[i]);
            // 닫기 버튼 flash: 더 밝은 빨간색 → 원래 색으로
            let btn = rect.close_btn();
            fb::fill_rect(btn.x, btn.y, btn.w, btn.h, fb::rgb(255, 80, 80));
            fb::draw_char(btn.x + 2, btn.y + 2, b'X',
                fb::rgb(255, 255, 255), fb::rgb(255, 80, 80));
            return;
        }

        // 제목 표시줄 클릭 → 창 하이라이트
        if rect.title_bar().contains(cx, cy) {
            crate::serial_println!("[wm] focus '{}' title-bar clicked", titles[i]);
            // 제목 표시줄을 밝게 표시
            let tb = rect.title_bar();
            fb::fill_rect(tb.x, tb.y, tb.w, tb.h, fb::rgb(80, 110, 210));
            fb::draw_text(tb.x + 4, tb.y + 4, titles[i],
                fb::rgb(255, 255, 255), fb::rgb(80, 110, 210));
            return;
        }

        // 창 내부 콘텐츠 클릭
        crate::serial_println!("[wm] click inside '{}' @ ({}, {})",
            titles[i], cx, cy);
        return;
    }

    // 태스크바 클릭
    if cy >= tb_y {
        crate::serial_println!("[wm] taskbar click @ ({}, {})", cx, cy);
    }
}
