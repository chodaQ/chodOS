//! BETA-X 3: WM ↔ GFX 드라이버 IPC 채널 시연
//!
//! ## 흐름
//!
//! ```text
//! [GFX 태스크] gfx_task()    ← render 커맨드 수신 → fb::fill_rect 실행
//!      ↑ IPC (recv)
//! [WM 태스크]  wm_task()     → render 커맨드 송신
//!
//! Phase 1 (0 ~ 120회):  일반 IPC 큐 경로 (64B 인라인 복사)
//! Phase 2 (120회 이후): Policy Engine이 fast channel 자동 생성
//!                       → SharedBuffer 재사용, 힙 할당 없음
//! ```
//!
//! ## Render Command 포맷 (13바이트)
//!
//! ```text
//! [0]      cmd:   0=fill_rect
//! [1..2]   x:     u16 LE
//! [3..4]   y:     u16 LE
//! [5..6]   w:     u16 LE
//! [7..8]   h:     u16 LE
//! [9..12]  color: u32 LE (0x00RRGGBB)
//! ```

use core::sync::atomic::{AtomicU64, Ordering};
use crate::process::{ipc, ipc_fast, scheduler};
use crate::fb;

/// GFX 태스크 PID (wm_task 가 읽어 수신자 식별)
pub static GFX_PID: AtomicU64 = AtomicU64::new(0);

const CMD_FILL: u8 = 0;

/// fill_rect 커맨드를 13바이트 버퍼에 직렬화.
#[inline]
fn pack_fill(x: u16, y: u16, w: u16, h: u16, color: u32) -> [u8; 13] {
    let mut b = [0u8; 13];
    b[0] = CMD_FILL;
    b[1..3].copy_from_slice(&x.to_le_bytes());
    b[3..5].copy_from_slice(&y.to_le_bytes());
    b[5..7].copy_from_slice(&w.to_le_bytes());
    b[7..9].copy_from_slice(&h.to_le_bytes());
    b[9..13].copy_from_slice(&color.to_le_bytes());
    b
}

/// render 커맨드 실행 (GFX 측).
fn exec(data: &[u8]) {
    if data.len() < 13 || data[0] != CMD_FILL { return; }
    let x     = u16::from_le_bytes([data[1], data[2]]) as u32;
    let y     = u16::from_le_bytes([data[3], data[4]]) as u32;
    let w     = u16::from_le_bytes([data[5], data[6]]) as u32;
    let h     = u16::from_le_bytes([data[7], data[8]]) as u32;
    let color = u32::from_le_bytes([data[9], data[10], data[11], data[12]]);
    fb::fill_rect(x, y, w, h, color);
}

// ── GFX 드라이버 커널 태스크 ─────────────────────────────────────────────────

/// GFX 드라이버 태스크.
///
/// WM으로부터 render 커맨드를 수신해 framebuffer에 실행한다.
/// `ipc::recv()`가 fast_cap을 투명하게 처리하므로 fast channel 여부를 알 필요 없음.
pub fn gfx_task() -> ! {
    let pid = scheduler::current_pid();
    GFX_PID.store(pid, Ordering::Relaxed);
    crate::serial_println!("[gfx] 드라이버 태스크 시작 (pid={})", pid);

    let mut frames: u64 = 0;
    let mut fast_seen = false;

    loop {
        let mut processed = 0u32;
        while let Some(msg) = ipc::recv() {
            exec(&msg.data[..msg.len]);
            processed += 1;

            // fast channel 첫 수신 로그
            if msg.fast_cap != 0 && !fast_seen {
                fast_seen = true;
                crate::serial_println!(
                    "[gfx] ★ fast channel 첫 수신! cap={} frame={}",
                    msg.fast_cap, frames,
                );
            }
        }

        if processed > 0 {
            frames += 1;
            if frames % (15 * crate::DEMO_LOG_SCALE) == 0 {
                crate::serial_println!(
                    "[gfx] frames={} fast_channels={}",
                    frames, ipc_fast::channel_count(),
                );
            }
        }

        scheduler::yield_now();
    }
}

// ── WM 커널 태스크 ───────────────────────────────────────────────────────────

/// WM 태스크.
///
/// GFX에 render 커맨드를 전송한다.
/// - Phase 1: 120회 연속 전송(일반 IPC) → Policy Engine 임계값 초과
/// - Phase 2: Policy Engine이 fast channel 활성화 → 이후 자동으로 fast path 사용
pub fn wm_task() -> ! {
    // GFX 태스크가 PID를 등록할 때까지 스핀 대기
    let gfx_pid = loop {
        let p = GFX_PID.load(Ordering::Relaxed);
        if p != 0 { break p; }
        scheduler::yield_now();
    };
    let wm_pid = scheduler::current_pid();
    crate::serial_println!(
        "[wm-ipc] WM 태스크 시작 (pid={}, gfx_pid={})",
        wm_pid, gfx_pid,
    );

    // ── Phase 1: 일반 IPC 경로로 120회 전송 ─────────────────────────────────
    // 한 번에 120개 전송 → observe_ipc(wm, gfx, count) 120회 호출
    // → Policy Engine ipc_table count=120
    // 다음 adapt_and_report(36틱 ≈ 2초)에서 자동으로 fast channel 생성
    crate::serial_println!("[wm-ipc] Phase 1: 일반 IPC 120회 전송 시작...");
    for i in 0u16..120 {
        let color = fb::rgb((i * 2 % 255) as u8, 100, 180);
        let b = pack_fill(0, 760, 120, 8, color);
        ipc::send(gfx_pid, &b);
    }
    crate::serial_println!(
        "[wm-ipc] Phase 1 완료 — Policy Engine 리포트 대기 (36틱≈2초)",
    );
    scheduler::yield_now();

    // ── Phase 2: fast channel 활성화 후 지속 전송 ────────────────────────────
    // Policy Engine이 adapt_and_report에서 ensure_channel(wm, gfx) 호출 완료 시
    // 이후 ipc::send()가 fast path (SharedBuffer 재사용) 자동 사용
    let mut frame: u64 = 0;
    loop {
        let x     = (frame * 4 % 1200) as u16;
        let r     = (frame * 7 % 255) as u8;
        let color = fb::rgb(r, 150, 200);
        let b     = pack_fill(x, 768, 60, 16, color);

        let has_fast = ipc_fast::get_channel(wm_pid, gfx_pid).is_some();
        ipc::send(gfx_pid, &b);

        if frame == 0 {
            crate::serial_println!(
                "[wm-ipc] Phase 2 첫 프레임: fast_channel={}",
                has_fast,
            );
        }
        if frame > 0 && frame % (30 * crate::DEMO_LOG_SCALE) == 0 {
            crate::serial_println!(
                "[wm-ipc] frame={} fast_channel={}",
                frame,
                ipc_fast::get_channel(wm_pid, gfx_pid).is_some(),
            );
        }
        frame += 1;
        // 버스트마다 1틱 쉬어 생산 속도를 타이머에 묶는다 — 제한 없이
        // yield_now()를 돌리면 TCG가 포화되어 타이머 틱이 멈춘다.
        // (자세한 배경은 crate::DEMO_BURST 주석 참고)
        if frame % crate::DEMO_BURST == 0 {
            scheduler::sleep_ticks(1);
        } else {
            scheduler::yield_now();
        }
    }
}
