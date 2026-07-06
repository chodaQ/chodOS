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
//! | ParamTuned        | param_id | 0  | 0     | old_val<<32|new_val|

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
    Anomaly         = 8,
    SafetyBound     = 9, // BETA-X-2 6: 안전 한도 차단
    ParamTuned      = 10, // ST-1: Self-Tuning Policy Engine 파라미터 변경
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
            Self::SafetyBound     => "SafetyBound    ",
            Self::ParamTuned      => "ParamTuned     ",
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

/// BETA-X-2 6: 안전 한도가 차단한 결정을 기록.
///
/// | extra bits | 의미                              |
/// |------------|-----------------------------------|
/// | 0x1x       | 우선순위 쿨다운 차단 (x=잔여 창)  |
/// | 0x20       | 창당 채널 생성 한도 초과          |
/// | 0x30       | 채널 재생성 쿨다운 중             |
pub fn safety_bound(pid_a: u32, pid_b: u32, reason: u64) {
    record(TraceEvent {
        ts: rdtsc(), kind: EventKind::SafetyBound,
        pid_a, pid_b, cap: 0, extra: reason,
    });
}

/// BETA-X-2 4: 이상 탐지 강제 회수 시 기록.
pub fn anomaly(from: u32, to: u32, cap: u32, reason: u64) {
    record(TraceEvent {
        ts: rdtsc(), kind: EventKind::Anomaly,
        pid_a: from, pid_b: to, cap, extra: reason,
    });
}

/// ST-1: Self-Tuning Policy Engine이 파라미터를 조정할 때마다 기록.
///
/// "왜 갑자기 이렇게 동작하지?"를 사후 추적하기 위함 (ARCHITECTURE.md
/// Self-Tuning 트랙 설계 원칙: 모든 조정은 Event Tracer에 기록).
///
/// param_id: 1=report_interval (추후 2=ema_alpha, 3=time_slice_range 등 확장)
pub fn param_tuned(param_id: u32, old: u64, new: u64) {
    record(TraceEvent {
        ts: rdtsc(), kind: EventKind::ParamTuned,
        pid_a: param_id, pid_b: 0, cap: 0,
        extra: (old << 32) | (new & 0xFFFF_FFFF),
    });
}

// ── 덤프 ─────────────────────────────────────────────────────────────────────

/// 스냅샷 버퍼 (dump()용 정적 배열 — 스택 오버플로 방지)
///
/// dump()가 TRACER 락을 잡은 채 serial 출력을 하면, 동시에 타이머 ISR이
/// policy_tick() → TRACER.lock()을 시도해 스핀락 데드락이 발생한다.
/// 해결: 락을 잡는 시간을 최소화(메모리 복사만)하고, 출력은 락 밖에서 수행.
static mut DUMP_BUF: [TraceEvent; RING_SIZE] = [const { TraceEvent::zero() }; RING_SIZE];

/// ring buffer 전체를 serial로 출력 (오래된 순서).
pub fn dump() {
    // 1. 락을 잡고 스냅샷만 복사 (느린 serial 출력은 락 밖에서)
    let (total, start, count) = {
        let t = TRACER.lock();
        let total = t.total;
        let count = total.min(RING_SIZE);
        let start = if total >= RING_SIZE { t.head } else { 0 };
        unsafe {
            for i in 0..count {
                let idx = (start + i) % RING_SIZE;
                DUMP_BUF[i] = t.buf[idx];
            }
        }
        (total, start, count)
    }; // ← 락 해제 (타이머 ISR이 다시 record 가능)
    let _ = start;

    // 2. 락 없이 출력
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

    for i in 0..count {
        let ev = unsafe { &DUMP_BUF[i] };
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
        EventKind::SafetyBound => crate::serial_println!(
            "[tracer]  {:>3} {:016x}  {}  pid{}↔pid{}  reason={:#x}",
            seq, ev.ts, ev.kind.label(), ev.pid_a, ev.pid_b, ev.extra,
        ),
        EventKind::ParamTuned => {
            let old = (ev.extra >> 32) as u32;
            let new = ev.extra as u32;
            crate::serial_println!(
                "[tracer]  {:>3} {:016x}  {}  param#{}  {}→{}",
                seq, ev.ts, ev.kind.label(), ev.pid_a, old, new,
            );
        },
    }
}
