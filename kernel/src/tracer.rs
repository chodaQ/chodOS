//! BETA-X-2 1: Event Tracer — 자율 결정 블랙박스
//!
//! Policy Engine이 내리는 모든 자율 결정(채널 생성/소멸/핫쌍 감지/decay/우선순위 조정)을
//! 고정 크기 ring buffer에 기록한다. "왜 그 결정을 했는지" 사후 추적 가능.
//!
//! ## 설계
//!
//! - RING_SIZE = 256 슬롯, 고정 크기 (BSS, 힙 불필요, ~12 KB)
//! - 오버플로 시 가장 오래된 슬롯을 덮어씀 (circular overwrite)
//! - 단일 Mutex: Policy Engine은 BSP 전용이므로 contention 없음
//!
//! ## 이벤트별 extra 필드 의미
//!
//! | Kind              | pid_a | pid_b | cap   | extra              |
//! |-------------------|-------|-------|-------|--------------------|
//! | ChannelCreated    | from  | to    | cap   | capacity(B)        |
//! | ChannelDropped    | from  | to    | cap   | cold_count         |
//! | HotPairDetected   | from  | to    | 0     | ipc_count          |
//! | PinPair           | from  | to    | 0     | cpu_id             |
//! | PriorityBoost     | pid   | 0     | 0     | old_pri<<32|new    |
//! | DecayWarning      | from  | to    | 0     | cold<<16|max_cold  |
//! | PolicyTick        | 0     | 0     | 0     | tick               |
//! | Anomaly           | from  | to    | cap   | reason code        |

use spin::Mutex;

// ── 상수 ─────────────────────────────────────────────────────────────────────

const RING_SIZE: usize = 256;

// ── 이벤트 타입 ───────────────────────────────────────────────────────────────

#[repr(u8)]
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum EventKind {
    ChannelCreated  = 1,
    ChannelDropped  = 2,
    HotPairDetected = 3,
    PinPair         = 4,
    PriorityBoost   = 5,
    DecayWarning    = 6,
    PolicyTick      = 7,
    Anomaly         = 8, // BETA-X-2 4 예약
}

impl EventKind {
    fn label(self) -> &'static str {
        match self {
            Self::ChannelCreated  => "ChannelCreated ",
            Self::ChannelDropped  => "ChannelDropped ",
            Self::HotPairDetected => "HotPairDetected",
            Self::PinPair         => "PinPair        ",
            Self::PriorityBoost   => "PriorityBoost  ",
            Self::DecayWarning    => "DecayWarning   ",
            Self::PolicyTick      => "PolicyTick     ",
            Self::Anomaly         => "Anomaly        ",
        }
    }
}

// ── 이벤트 구조체 ─────────────────────────────────────────────────────────────

#[derive(Copy, Clone)]
pub struct TraceEvent {
    pub ts:    u64,
    pub kind:  EventKind,
    pub pid_a: u32,
    pub pid_b: u32,
    pub cap:   u32,
    pub extra: u64,
}

impl TraceEvent {
    const fn zero() -> Self {
        Self { ts: 0, kind: EventKind::PolicyTick, pid_a: 0, pid_b: 0, cap: 0, extra: 0 }
    }
}

// ── rdtsc ─────────────────────────────────────────────────────────────────────

#[inline(always)]
pub fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack),
        );
    }
    ((hi as u64) << 32) | lo as u64
}

// ── Ring Buffer ───────────────────────────────────────────────────────────────

struct TracerState {
    buf:   [TraceEvent; RING_SIZE],
    head:  usize,  // 다음 쓸 슬롯 (mod RING_SIZE)
    total: usize,  // 누적 이벤트 수 (오버플로 감지용)
}

impl TracerState {
    const fn new() -> Self {
        Self {
            buf:   [const { TraceEvent::zero() }; RING_SIZE],
            head:  0,
            total: 0,
        }
    }

    fn push(&mut self, ev: TraceEvent) {
        self.buf[self.head] = ev;
        self.head = (self.head + 1) % RING_SIZE;
        self.total += 1;
    }
}

static TRACER: Mutex<TracerState> = Mutex::new(TracerState::new());

// ── 공개 기록 API ─────────────────────────────────────────────────────────────

pub fn record(ev: TraceEvent) {
    TRACER.lock().push(ev);
}

pub fn channel_created(from: u32, to: u32, cap: u32, capacity: usize) {
    record(TraceEvent {
        ts: rdtsc(), kind: EventKind::ChannelCreated,
        pid_a: from, pid_b: to, cap, extra: capacity as u64,
    });
}

pub fn channel_dropped(from: u32, to: u32, cap: u32, cold_count: u32) {
    record(TraceEvent {
        ts: rdtsc(), kind: EventKind::ChannelDropped,
        pid_a: from, pid_b: to, cap, extra: cold_count as u64,
    });
}

pub fn hot_pair_detected(from: u32, to: u32, ipc_count: u64) {
    record(TraceEvent {
        ts: rdtsc(), kind: EventKind::HotPairDetected,
        pid_a: from, pid_b: to, cap: 0, extra: ipc_count,
    });
}

pub fn pin_pair_event(from: u32, to: u32, cpu: u8) {
    record(TraceEvent {
        ts: rdtsc(), kind: EventKind::PinPair,
        pid_a: from, pid_b: to, cap: 0, extra: cpu as u64,
    });
}

pub fn priority_boost(pid: u32, old_pri: u8, new_pri: u8) {
    record(TraceEvent {
        ts: rdtsc(), kind: EventKind::PriorityBoost,
        pid_a: pid, pid_b: 0, cap: 0,
        extra: ((old_pri as u64) << 32) | new_pri as u64,
    });
}

pub fn decay_warning(from: u32, to: u32, cold: u32, max_cold: u32) {
    record(TraceEvent {
        ts: rdtsc(), kind: EventKind::DecayWarning,
        pid_a: from, pid_b: to, cap: 0,
        extra: ((cold as u64) << 16) | max_cold as u64,
    });
}

pub fn policy_tick(tick: u64) {
    record(TraceEvent {
        ts: rdtsc(), kind: EventKind::PolicyTick,
        pid_a: 0, pid_b: 0, cap: 0, extra: tick,
    });
}

/// BETA-X-2 4 예약: 이상 탐지 강제 회수 시 기록.
pub fn anomaly(from: u32, to: u32, cap: u32, reason: u64) {
    record(TraceEvent {
        ts: rdtsc(), kind: EventKind::Anomaly,
        pid_a: from, pid_b: to, cap, extra: reason,
    });
}

// ── 덤프 ─────────────────────────────────────────────────────────────────────

/// ring buffer 전체를 serial로 출력 (오래된 순서).
pub fn dump() {
    let t = TRACER.lock();
    let total = t.total;
    let count = total.min(RING_SIZE);

    crate::serial_println!(
        "[tracer] ══════════════════════════════════════════════════════"
    );
    crate::serial_println!(
        "[tracer]  BETA-X-2 Event Log  (ring={}, 기록={}, 표시={})",
        RING_SIZE, total, count,
    );
    if total > RING_SIZE {
        crate::serial_println!(
            "[tracer]  ※ 오래된 {}개 이벤트는 덮어씌워짐", total - RING_SIZE,
        );
    }
    crate::serial_println!(
        "[tracer] ──────────────────────────────────────────────────────"
    );

    let start = if total >= RING_SIZE { t.head } else { 0 };

    for i in 0..count {
        let idx = (start + i) % RING_SIZE;
        let ev  = &t.buf[idx];
        print_event(i + 1, ev);
    }

    crate::serial_println!(
        "[tracer] ══════════════════════════════════════════════════════"
    );
}

fn print_event(seq: usize, ev: &TraceEvent) {
    match ev.kind {
        EventKind::ChannelCreated => crate::serial_println!(
            "[tracer]  {:>3} {:016x}  {}  pid{}→pid{}  cap={}  buf={}B",
            seq, ev.ts, ev.kind.label(), ev.pid_a, ev.pid_b, ev.cap, ev.extra,
        ),
        EventKind::ChannelDropped => crate::serial_println!(
            "[tracer]  {:>3} {:016x}  {}  pid{}→pid{}  cap={}  cold={}",
            seq, ev.ts, ev.kind.label(), ev.pid_a, ev.pid_b, ev.cap, ev.extra,
        ),
        EventKind::HotPairDetected => crate::serial_println!(
            "[tracer]  {:>3} {:016x}  {}  pid{}→pid{}  ipc_cnt={}",
            seq, ev.ts, ev.kind.label(), ev.pid_a, ev.pid_b, ev.extra,
        ),
        EventKind::PinPair => crate::serial_println!(
            "[tracer]  {:>3} {:016x}  {}  pid{}↔pid{}  → core{}",
            seq, ev.ts, ev.kind.label(), ev.pid_a, ev.pid_b, ev.extra,
        ),
        EventKind::PriorityBoost => {
            let old = (ev.extra >> 32) as u8;
            let new = ev.extra as u8;
            crate::serial_println!(
                "[tracer]  {:>3} {:016x}  {}  pid{}  {}→{}",
                seq, ev.ts, ev.kind.label(), ev.pid_a, old, new,
            );
        },
        EventKind::DecayWarning => {
            let cold     = (ev.extra >> 16) as u32;
            let max_cold = (ev.extra & 0xFFFF) as u32;
            crate::serial_println!(
                "[tracer]  {:>3} {:016x}  {}  pid{}→pid{}  cold={}/{}",
                seq, ev.ts, ev.kind.label(), ev.pid_a, ev.pid_b, cold, max_cold,
            );
        },
        EventKind::PolicyTick => crate::serial_println!(
            "[tracer]  {:>3} {:016x}  {}  tick={}",
            seq, ev.ts, ev.kind.label(), ev.extra,
        ),
        EventKind::Anomaly => crate::serial_println!(
            "[tracer]  {:>3} {:016x}  {}  pid{}→pid{}  cap={}  reason={:#x}",
            seq, ev.ts, ev.kind.label(), ev.pid_a, ev.pid_b, ev.cap, ev.extra,
        ),
    }
}
