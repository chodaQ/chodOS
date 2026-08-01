//! BETA-X 4: 마우스/키보드 → 포그라운드 앱 입력 경로 직통화
//!
//! ## 구조
//!
//! ```text
//! IRQ1  → kbd::push_key(ascii)   → input_direct::push_key  ─┐
//! IRQ12 → mouse::process_packet  → input_direct::push_mouse ─┤
//!                                                            ↓
//!                                              InputRing (AtomicU64 × 64)
//!                                                            ↓ (task context)
//!                                              input_drv_task  pop → ipc::send
//!                                                            ↓
//!                                              FOREGROUND_PID 프로세스 (수신)
//! ```
//!
//! ## 이벤트 인코딩 (u64 하나)
//!
//! ```text
//! bits 0-1:   type  (0=key, 1=mouse_move, 2=mouse_click)
//! bits 2-9:   ascii (key 이벤트)
//! bits 2-4:   btns  (mouse 이벤트, L/R/M = bit 0/1/2)
//! bits 10-20: x     (mouse, 0..2047)
//! bits 21-31: y     (mouse, 0..2047)
//! ```
//!
//! ## Fast channel 전환 흐름
//!
//! 1. input_drv_task가 IPC 100+회 송신
//! 2. Policy Engine adapt_and_report: (input_drv, foreground) 쌍 감지
//! 3. ensure_channel 자동 호출 → SharedBuffer 512B 예약
//! 4. 이후 ipc::send() → write_fast → sentinel (fast path)
//! 5. foreground recv(): msg.fast_cap != 0 → 직통 확인

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use crate::process::{ipc, ipc_fast, scheduler};

// ── 이벤트 타입 상수 ──────────────────────────────────────────────────────────

pub const EVT_KEY:         u64 = 0;
pub const EVT_MOUSE_MOVE:  u64 = 1;
pub const EVT_MOUSE_CLICK: u64 = 2;

// ── IRQ-safe 이벤트 링 버퍼 ───────────────────────────────────────────────────

const EVT_CAP: usize = 64;

struct InputRing {
    buf:  [AtomicU64;  EVT_CAP],
    head: AtomicUsize, // consumer
    tail: AtomicUsize, // producer (IRQ context)
}

unsafe impl Sync for InputRing {}

static RING: InputRing = InputRing {
    buf:  [const { AtomicU64::new(0) }; EVT_CAP],
    head: AtomicUsize::new(0),
    tail: AtomicUsize::new(0),
};

fn push_event(val: u64) {
    let tail = RING.tail.load(Ordering::Relaxed);
    let next = (tail + 1) % EVT_CAP;
    if next == RING.head.load(Ordering::Relaxed) { return; } // 풀 — 드롭
    RING.buf[tail].store(val, Ordering::Relaxed);
    RING.tail.store(next, Ordering::Relaxed);
}

fn pop_event() -> Option<u64> {
    let head = RING.head.load(Ordering::Relaxed);
    if head == RING.tail.load(Ordering::Relaxed) { return None; }
    let val = RING.buf[head].load(Ordering::Relaxed);
    RING.head.store((head + 1) % EVT_CAP, Ordering::Relaxed);
    Some(val)
}

// ── 공개 push API (IRQ 핸들러에서 호출) ──────────────────────────────────────

/// 키보드 ASCII 이벤트 push (kbd::push_key에서 호출, IRQ1 컨텍스트).
pub fn push_key(ascii: u8) {
    push_event(EVT_KEY | ((ascii as u64) << 2));
}

/// 마우스 이동 이벤트 push (mouse::process_packet에서 호출, IRQ12 컨텍스트).
pub fn push_mouse_move(x: i32, y: i32) {
    let xc = x.max(0).min(2047) as u64;
    let yc = y.max(0).min(2047) as u64;
    push_event(EVT_MOUSE_MOVE | (xc << 10) | (yc << 21));
}

/// 마우스 클릭 이벤트 push (mouse::on_click에서 호출).
pub fn push_mouse_click(x: i32, y: i32, btns: u8) {
    let xc  = x.max(0).min(2047) as u64;
    let yc  = y.max(0).min(2047) as u64;
    let b   = (btns as u64) & 0x7;
    push_event(EVT_MOUSE_CLICK | (b << 2) | (xc << 10) | (yc << 21));
}

// ── 포그라운드 PID 관리 ───────────────────────────────────────────────────────

/// 현재 입력 수신 프로세스 PID (0 = 미설정)
pub static FOREGROUND_PID: AtomicU64 = AtomicU64::new(0);
/// 입력 드라이버 태스크 PID (fast channel "from" 쪽)
pub static INPUT_DRV_PID:  AtomicU64 = AtomicU64::new(0);

pub fn set_foreground(pid: crate::process::Pid) {
    FOREGROUND_PID.store(pid, Ordering::Relaxed);
    crate::serial_println!("[input] 포그라운드 → pid{}", pid);
}

// ── 커널 태스크 ───────────────────────────────────────────────────────────────

/// 입력 드라이버 태스크 — 링 버퍼에서 이벤트를 꺼내 포그라운드 앱에 IPC 전송.
///
/// 태스크 컨텍스트에서 동작하므로 IPC 힙 할당 안전.
/// Policy Engine이 임계값 초과를 감지하면 자동으로 fast channel 전환.
pub fn input_drv_task() -> ! {
    let pid = scheduler::current_pid();
    INPUT_DRV_PID.store(pid, Ordering::Relaxed);
    crate::serial_println!("[input-drv] 태스크 시작 (pid={})", pid);

    let mut total: u64 = 0;
    let mut fast_logged = false;
    loop {
        let fg = FOREGROUND_PID.load(Ordering::Relaxed);
        if fg != 0 {
            while let Some(event) = pop_event() {
                let bytes = event.to_le_bytes();
                ipc::send(fg, &bytes);
                total += 1;

                if total == 1 {
                    crate::serial_println!(
                        "[input-drv] 첫 이벤트 → pid{} (일반 IPC 경로)",
                        fg,
                    );
                }
                // fast channel 확인
                if !fast_logged && ipc_fast::get_channel(pid, fg).is_some() {
                    fast_logged = true;
                    crate::serial_println!(
                        "[input-drv] ★ fast channel 전환 확인 (total={})",
                        total,
                    );
                }
            }
        }
        scheduler::yield_now();
    }
}

/// 포그라운드 앱 시뮬레이터 — 입력 이벤트를 수신해 로그 출력.
///
/// 실제 시나리오에서는 Shell / GUI 앱이 이 역할을 담당.
pub fn input_consumer_task() -> ! {
    let pid = scheduler::current_pid();
    set_foreground(pid);
    crate::serial_println!("[input-app] 포그라운드 앱 시작 (pid={})", pid);

    let mut count: u64 = 0;
    let mut fast_seen = false;
    loop {
        while let Some(msg) = ipc::recv() {
            count += 1;

            if msg.fast_cap != 0 && !fast_seen {
                fast_seen = true;
                crate::serial_println!(
                    "[input-app] ★ fast channel 수신! cap={} (count={})",
                    msg.fast_cap, count,
                );
            }

            // 일정 건수마다 이벤트 내용 로그 (quick 모드에서는 훨씬 드물게)
            if count % (20 * crate::DEMO_LOG_SCALE) == 0 {
                let val = u64::from_le_bytes([
                    msg.data[0], msg.data[1], msg.data[2], msg.data[3],
                    msg.data[4], msg.data[5], msg.data[6], msg.data[7],
                ]);
                let t     = val & 0x3;
                let ascii = ((val >> 2) & 0xFF) as u8;
                let kind  = match t {
                    EVT_KEY         => "key",
                    EVT_MOUSE_MOVE  => "move",
                    EVT_MOUSE_CLICK => "click",
                    _               => "?",
                };
                if t == EVT_KEY && ascii.is_ascii_graphic() {
                    crate::serial_println!(
                        "[input-app] count={} fast={} evt={} key='{}'",
                        count, fast_seen, kind, ascii as char,
                    );
                } else {
                    crate::serial_println!(
                        "[input-app] count={} fast={} evt={}",
                        count, fast_seen, kind,
                    );
                }
            }
        }
        scheduler::yield_now();
    }
}

/// 합성 키보드 이벤트 생성 태스크 — 데모용 (실제 키 입력 없이 fast channel 시연).
///
/// Phase 1: 130개 이벤트 burst → IPC_HOT_THRESHOLD 초과
/// Phase 2: 주기적 생성 (fast channel 활성 유지)
pub fn key_gen_task() -> ! {
    crate::serial_println!("[key-gen] 합성 이벤트 생성 시작");

    let chars = b"MuKernel BETA-X4 fast input path";
    let len = chars.len();

    // Phase 1: burst 130개 (threshold 초과)
    for i in 0u16..130 {
        push_key(chars[(i as usize) % len]);
    }
    // mouse move도 섞어서 push
    for i in 0u16..20 {
        push_mouse_move((i * 40) as i32, 400);
    }
    crate::serial_println!("[key-gen] burst 150 이벤트 push 완료");
    scheduler::yield_now();

    // Phase 2: 지속 생성 — 버스트마다 1틱 쉬어 생산 속도를 타이머에 묶는다.
    // (제한 없이 yield_now()를 돌리면 TCG가 포화되어 타이머 틱이 멈춘다.
    //  자세한 배경은 crate::DEMO_BURST 주석 참고)
    let mut i = 0u64;
    loop {
        push_key(chars[(i as usize) % len]);
        i += 1;
        if i % crate::DEMO_BURST == 0 {
            scheduler::sleep_ticks(1);
        } else {
            scheduler::yield_now();
        }
    }
}
