//! Policy Engine — 적응형 스케줄러 Phase 2 (ALPHA M3 → BETA M1)
//!
//! ## 적응 로직
//!
//! 매 리포트 주기(36틱 ≈ 2초)마다 최근 윈도우 내 최대 CPU 점유율을 계산:
//!
//! ```text
//! 최대 점유율 ≥ 70%  →  slice = 1틱  (CPU-bound, 공격적 선점)
//! 최대 점유율 ≥ 40%  →  slice = 2틱  (혼합 부하)
//! 최대 점유율 < 40%  →  slice = 3틱  (협력적, 컨텍스트 스위치 최소화)
//! ```
//!
//! ## interrupt-safe 설계
//!
//! - `TIME_SLICE`: `AtomicU64` → lock-free read/write
//! - `on_switch()`: 힙 할당 없음, 고정 배열, 잠금 없음

use core::sync::atomic::{AtomicU64, Ordering};
use crate::process::{Pid, Priority};

/// 스케줄러(timer_preempt)가 읽는 현재 타임슬라이스 (틱 단위).
///
/// Policy Engine이 적응 로직으로 주기적으로 갱신.
/// 기본값: 3틱 (≈165ms @ 18.2Hz PIT)
pub static TIME_SLICE: AtomicU64 = AtomicU64::new(3);

// ── 프로세스별 통계 ──────────────────────────────────────────────────────────

#[derive(Copy, Clone)]
pub struct CpuStats {
    pub pid: Pid,
    pub active: bool,
    pub ticks_run: u64,
    pub recent_ticks: u64,
    pub run_start_tick: u64,
    pub switch_in_count: u64,
    /// BETA-X-2 6: 우선순위 변경 후 남은 쿨다운 창 수 (0=변경 허용)
    pub pri_cooldown: u8,
    /// voluntary 비율 EMA (×10 고정소수점, 예: 605 = 60.5%)
    ///
    /// 공식: new_ema = (3 × current_vol_pct×10 + 7 × old_ema) / 10
    /// α=0.3 → 최근 관찰에 30% 가중치, 과거 추세에 70% 가중치.
    /// 초기값 500 (50%) — neutral에서 출발해 실측치로 수렴.
    pub vol_ema: u64,
}

impl CpuStats {
    const fn empty() -> Self {
        CpuStats {
            pid: 0,
            active: false,
            ticks_run: 0,
            recent_ticks: 0,
            run_start_tick: 0,
            switch_in_count: 0,
            pri_cooldown: 0,
            vol_ema: 500, // 50.0% — neutral 초기값
        }
    }
}

const MAX_PROCS: usize = 32;

// ── BETA-X 1: IPC 빈도 추적 ──────────────────────────────────────────────────

#[derive(Copy, Clone)]
struct IpcEntry {
    from: Pid,
    to: Pid,
    count: u64,
    active: bool,
    /// BETA-X 6: 마지막 리포트 창 기준 누적 카운트 (델타 계산용)
    last_count: u64,
    /// BETA-X 6: 연속 cold 창 수 (이 횟수가 DECAY_COLD_WINDOWS 이상이면 채널 회수)
    cold_windows: u8,
    // ── BETA-X-2 4: 이상 탐지 ─────────────────────────────────────────────
    /// 창당 메시지 수 EMA (×10 고정소수점, α=0.3)
    rate_ema: u64,
    /// 현재 창에서 수신된 총 페이로드 바이트 (창 시작마다 0 리셋)
    payload_bytes_window: u64,
    /// 창당 페이로드 바이트 EMA (×10 고정소수점, α=0.3)
    payload_ema: u64,
    // ── ML 1: Beta-Binomial Bayesian 핫 감지 ──────────────────────────────
    /// Bayesian posterior α — 활성 창 pseudo-count
    bayes_alpha: u32,
    /// Bayesian posterior β — 비활성 창 pseudo-count
    bayes_beta: u32,
    // ── ML 2: GBDT 인퍼런스용 추가 feature ────────────────────────────────
    /// 이전 창 delta (trend feature: 증가 추세 감지용)
    delta_prev: u64,
    // ── ML 3: Q-learning per-pair 상태 추적 ───────────────────────────────
    /// 직전 창에서 관찰한 RL state bucket (Q-table 업데이트용)
    rl_prev_state: u8,
    /// 이번 창에 적용할 action (직전 창 끝에 선택됨)
    rl_action: u8,
}

impl IpcEntry {
    const fn empty() -> Self {
        IpcEntry {
            from: 0, to: 0, count: 0, active: false,
            last_count: 0, cold_windows: 0,
            rate_ema: 0, payload_bytes_window: 0, payload_ema: 0,
            bayes_alpha:    BAYES_PRIOR_ALPHA,
            bayes_beta:     BAYES_PRIOR_BETA,
            delta_prev:     0,
            rl_prev_state:  0,
            rl_action:      RL_NOOP,
        }
    }
}

/// IPC 핫-쌍 임계값: Legacy (anomaly EMA 기준선 유지용)
pub const IPC_HOT_THRESHOLD: u64 = 100;

// ── Policy B-1: 메모리 압력 추적 ─────────────────────────────────────────────

#[derive(Copy, Clone)]
struct MemEntry {
    pid:         Pid,
    mmap_calls:  u64, // 누적 mmap 호출 횟수
    page_faults: u64, // 누적 페이지 폴트 횟수
    last_mmap:   u64, // 마지막 창 기준 mmap (델타 계산)
    last_faults: u64, // 마지막 창 기준 faults (델타 계산)
    active:      bool,
}

impl MemEntry {
    const fn empty() -> Self {
        MemEntry { pid: 0, mmap_calls: 0, page_faults: 0, last_mmap: 0, last_faults: 0, active: false }
    }
}

/// 창당 mmap 횟수가 이 이상이면 "메모리 활동 높음"
const MEM_HOT_THRESHOLD: u64 = 10;
const MAX_MEM_ENTRIES: usize = 16;

// ── Policy B-2: 전력 상태 ────────────────────────────────────────────────────

struct PowerStats {
    last_aperf:    u64,
    last_mperf:    u64,
    /// 유휴율 EMA (×10 고정소수점, 초기 500 = 50.0%)
    idle_pct_ema:  u64,
}

impl PowerStats {
    const fn new() -> Self {
        PowerStats { last_aperf: 0, last_mperf: 0, idle_pct_ema: 500 }
    }
}

/// BETA-X 6: 창당 이 이하 메시지면 "cold" (채널 회수 후보)
const DECAY_THRESHOLD: u64 = 5;

/// BETA-X 6: DECAY_THRESHOLD 이하인 창이 이 횟수 연속되면 채널 회수
const DECAY_COLD_WINDOWS: u8 = 2;

// ── ML 1: Beta-Binomial Bayesian 핫 감지 상수 ────────────────────────────────
/// Bayesian prior α: 초기에 약간 cold 편향 (쉽게 fast channel 생성 안 함)
const BAYES_PRIOR_ALPHA: u32 = 1;
/// Bayesian prior β
const BAYES_PRIOR_BETA:  u32 = 4;
/// 핫 창당 α 증가량 (delta / 10, 최소 2)
const BAYES_HOT_GAIN: u32 = 2;
/// 핫 창당 α 최대 증가량 — spike 과민 방지
/// delta=300이면 gain=min(30,5)=5 → 기존 gain=30과 달리 α 폭증 억제
const BAYES_GAIN_CAP: u32 = 5;
/// cold 창당 β 증가량 (느린 망각)
const BAYES_COLD_GAIN: u32 = 1;
/// cold 창당 α 감소량 (빠른 망각 — 기존 1에서 3으로)
/// spike 후 cold 6창 → α가 빠르게 PRIOR로 복귀
const BAYES_COLD_DECAY: u32 = 3;
/// 핫 판정 임계 스코어 (= α*1000/(α+β), 0~1000 범위)
/// 650 ≈ P(hot) ≥ 65%
pub const BAYES_HOT_SCORE: u32 = 650;
/// α, β 포화 방지 상한 (이 이상 증가 안 함)
const BAYES_MAX: u32 = 500;

// ── ML 2: Gradient Boosted Decision Tree ─────────────────────────────────────
/// ML 2 HOT 판정 임계값 (4개 트리 leaf score 합산, 이론 범위 약 -200..+580)
pub const ML2_HOT_THRESHOLD: i64 = 300;

// ── ML 3: Q-learning (ε-greedy) ──────────────────────────────────────────────
/// State 수: alpha(4) × rate(3) × cold(3) = 36
const RL_N_STATES:  usize = 36;
/// Action 수: NOOP / ENCOURAGE / DISCOURAGE / PIN_PUSH
const RL_N_ACTIONS: usize = 4;

const RL_NOOP:       u8 = 0; // ML2 임계값 그대로
const RL_ENCOURAGE:  u8 = 1; // ML2 임계값 -80 (채널 생성 쉽게)
const RL_DISCOURAGE: u8 = 2; // ML2 임계값 +80 (채널 생성 어렵게)
const RL_PIN_PUSH:   u8 = 3; // 임계값 무관, 강제 pin + 우선순위 부스트

/// ENCOURAGE/DISCOURAGE 가 ML2 임계값을 이만큼 조정
const RL_THRESHOLD_DELTA: i64 = 80;

/// Q-table 학습률 α = 1/10 (×100 fixed-point)
const RL_LR_DEN:    i32 = 10;
/// 할인율 γ = 9/10
const RL_GAMMA_NUM: i32 = 9;
const RL_GAMMA_DEN: i32 = 10;

/// 탐색 확률 초기값 (%, 100 중)
const RL_EPSILON_INIT: u8 = 20;
/// 탐색 확률 최솟값
const RL_EPSILON_MIN:  u8 =  5;
/// 이 틱 수마다 epsilon 1% 감소
const RL_EPSILON_DECAY_TICKS: u64 = 1000;

// ── BETA-X-2 4: 이상 탐지 임계값 ────────────────────────────────────────────
/// EMA 기준선이 이 이상 확립된 후에만 spike 판정 (초기 과민 반응 방지)
const ANOMALY_EMA_MIN_BASELINE: u64 = 10; // ×10 고정소수점 → 실제 1msg/창
/// delta가 rate_ema의 이 배수 이상이면 spike (갑작스러운 폭증)
const ANOMALY_RATE_SPIKE_MULT: u64 = 20;
/// spike 판정 최소 절대 메시지 수 (EMA 잡음 방지)
const ANOMALY_RATE_SPIKE_MIN: u64 = 200;
/// 페이로드 바이트 spike 배수
const ANOMALY_PAYLOAD_SPIKE_MULT: u64 = 20;

/// 이상 탐지 reason 코드 (tracer::anomaly extra 필드)
const ANOMALY_REASON_RATE_SPIKE:    u64 = 0x01;
const ANOMALY_REASON_PAYLOAD_SPIKE: u64 = 0x02;

// ── BETA-X-2 6: Safety Bounds ─────────────────────────────────────────────────
/// 우선순위 변경 후 최소 대기 창 수 (연속 thrashing 방지)
const PRIORITY_COOLDOWN_WINDOWS: u8 = 3;
/// TIME_SLICE 허용 범위 (틱 단위)
const TIME_SLICE_MIN: u64 = 1;
const TIME_SLICE_MAX: u64 = 4;
/// 한 리포트 창당 최대 신규 채널 생성 수
const MAX_CHANNELS_PER_WINDOW: u8 = 2;
/// 채널 회수(이상/decay) 후 재생성 금지 기간 (틱 단위, ≈2창)
const CHANNEL_RECREATE_COOLDOWN_TICKS: u64 = 72;
/// 재생성 금지 쌍 최대 추적 수
const MAX_EVICT_PAIRS: usize = 8;

const MAX_IPC_PAIRS: usize = 16;

// ── Policy Engine ────────────────────────────────────────────────────────────

pub struct PolicyEngine {
    stats: [CpuStats; MAX_PROCS],
    total_ticks: u64,
    last_report_tick: u64,
    report_interval: u64,
    /// BETA-X 1: 프로세스 쌍별 IPC 빈도 테이블
    ipc_table: [IpcEntry; MAX_IPC_PAIRS],
    /// Policy B-1: 프로세스별 메모리 압력 테이블
    mem_table: [MemEntry; MAX_MEM_ENTRIES],
    /// Policy B-2: 전력 상태
    power: PowerStats,
    // ── BETA-X-2 6: Safety Bounds 상태 ──────────────────────────────────────
    /// 이번 창에서 신규 생성된 채널 수 (창 시작마다 0 리셋)
    channels_this_window: u8,
    /// 회수된 쌍과 재생성 허용 최소 tick: (from, to, min_tick)
    evict_table: [(Pid, Pid, u64); MAX_EVICT_PAIRS],
    /// evict_table 유효 항목 수
    evict_len: usize,
    // ── ML 3: Q-learning 공유 상태 ───────────────────────────────────────────
    /// Q-table [state][action], ×100 fixed-point (초기값 NOOP=10 약한 bias)
    rl_q: [[i32; RL_N_ACTIONS]; RL_N_STATES],
    /// 현재 탐색 확률 (%, 100 중)
    rl_epsilon: u8,
    /// xorshift64 RNG 상태 (0이면 1로 초기화)
    rl_rng: u64,
}

impl PolicyEngine {
    const fn new_const() -> Self {
        PolicyEngine {
            stats: [CpuStats::empty(); MAX_PROCS],
            total_ticks: 0,
            last_report_tick: 0,
            report_interval: 36,
            ipc_table: [IpcEntry::empty(); MAX_IPC_PAIRS],
            mem_table: [MemEntry::empty(); MAX_MEM_ENTRIES],
            power: PowerStats::new(),
            channels_this_window: 0,
            evict_table: [(0, 0, 0); MAX_EVICT_PAIRS],
            evict_len: 0,
            rl_q:       [[10, 0, 0, 0]; RL_N_STATES], // NOOP 약한 bias
            rl_epsilon: RL_EPSILON_INIT,
            rl_rng:     0x123456789abcdef0,
        }
    }

    // ── BETA-X-2 6 헬퍼 ────────────────────────────────────────────────────

    /// 회수 후 재생성 금지 기간인지 확인.
    fn in_evict_cooldown(&self, from: Pid, to: Pid, tick: u64) -> bool {
        for i in 0..self.evict_len {
            let (ef, et, min_tick) = self.evict_table[i];
            if ef == from && et == to && tick < min_tick {
                return true;
            }
        }
        false
    }

    /// 채널 회수 시 재생성 금지 기간 등록.
    fn record_eviction(&mut self, from: Pid, to: Pid, tick: u64) {
        let min_tick = tick + CHANNEL_RECREATE_COOLDOWN_TICKS;
        // 같은 쌍이 있으면 갱신
        for i in 0..self.evict_len {
            if self.evict_table[i].0 == from && self.evict_table[i].1 == to {
                self.evict_table[i].2 = min_tick;
                return;
            }
        }
        if self.evict_len < MAX_EVICT_PAIRS {
            self.evict_table[self.evict_len] = (from, to, min_tick);
            self.evict_len += 1;
        } else {
            // 가장 이른 min_tick 슬롯 교체 (만료 우선)
            let mut min_i = 0;
            for i in 1..MAX_EVICT_PAIRS {
                if self.evict_table[i].2 < self.evict_table[min_i].2 { min_i = i; }
            }
            self.evict_table[min_i] = (from, to, min_tick);
        }
    }

    /// 만료된 evict 항목 제거.
    fn expire_evictions(&mut self, tick: u64) {
        let mut i = 0;
        while i < self.evict_len {
            if tick >= self.evict_table[i].2 {
                self.evict_len -= 1;
                self.evict_table[i] = self.evict_table[self.evict_len];
                self.evict_table[self.evict_len] = (0, 0, 0);
            } else {
                i += 1;
            }
        }
    }

    fn find_slot(&self, pid: Pid) -> Option<usize> {
        self.stats.iter().position(|s| s.active && s.pid == pid)
    }

    pub fn register(&mut self, pid: Pid) {
        if self.find_slot(pid).is_none() {
            if let Some(idx) = self.stats.iter().position(|s| !s.active) {
                self.stats[idx] = CpuStats {
                    pid,
                    active: true,
                    ticks_run: 0,
                    recent_ticks: 0,
                    run_start_tick: self.total_ticks,
                    switch_in_count: 0,
                    pri_cooldown: 0,
                    vol_ema: 500,
                };
            }
        }
    }

    /// 컨텍스트 스위치 이벤트 — ISR 컨텍스트에서 호출됨 (힙 할당 없음).
    pub fn on_switch(&mut self, from: Pid, to: Pid, tick: u64) {
        self.total_ticks = tick;

        // from 프로세스 실행 시간 누적
        if let Some(i) = self.find_slot(from) {
            let elapsed = tick.saturating_sub(self.stats[i].run_start_tick);
            self.stats[i].ticks_run    += elapsed;
            self.stats[i].recent_ticks += elapsed;
        }

        // to 프로세스 실행 시작 기록
        if let Some(i) = self.find_slot(to) {
            self.stats[i].run_start_tick  = tick;
            self.stats[i].switch_in_count += 1;
        }

        // 주기적 리포트 + 적응
        if tick >= self.last_report_tick + self.report_interval {
            self.last_report_tick = tick;
            self.adapt_and_report(tick);
        }
    }

    /// BETA-X 1: IPC 이벤트 수신 — from→to 누적 카운트 + 페이로드 바이트 갱신.
    pub fn observe_ipc(&mut self, from: Pid, to: Pid, count: u64, payload_bytes: u64) {
        // 기존 항목 업데이트
        for i in 0..MAX_IPC_PAIRS {
            if self.ipc_table[i].active
                && self.ipc_table[i].from == from
                && self.ipc_table[i].to == to
            {
                self.ipc_table[i].count = count;
                self.ipc_table[i].payload_bytes_window =
                    self.ipc_table[i].payload_bytes_window.saturating_add(payload_bytes);
                return;
            }
        }
        // 빈 슬롯에 신규 등록
        for i in 0..MAX_IPC_PAIRS {
            if !self.ipc_table[i].active {
                self.ipc_table[i] = IpcEntry {
                    from, to, count, active: true,
                    last_count: 0, cold_windows: 0,
                    rate_ema: 0, payload_bytes_window: payload_bytes, payload_ema: 0,
                    bayes_alpha: BAYES_PRIOR_ALPHA, bayes_beta: BAYES_PRIOR_BETA,
                    delta_prev: 0, rl_prev_state: 0, rl_action: RL_NOOP,
                };
                return;
            }
        }
        // 슬롯 가득 참 — 최소 카운트 슬롯 교체
        let mut min_i = 0;
        for i in 1..MAX_IPC_PAIRS {
            if self.ipc_table[i].count < self.ipc_table[min_i].count {
                min_i = i;
            }
        }
        self.ipc_table[min_i] = IpcEntry {
            from, to, count, active: true,
            last_count: 0, cold_windows: 0,
            rate_ema: 0, payload_bytes_window: payload_bytes, payload_ema: 0,
            bayes_alpha: BAYES_PRIOR_ALPHA, bayes_beta: BAYES_PRIOR_BETA,
            delta_prev: 0, rl_prev_state: 0, rl_action: RL_NOOP,
        };
    }

    /// Policy B-1: mmap 호출 1회 기록.
    pub fn observe_mmap(&mut self, pid: Pid) {
        for i in 0..MAX_MEM_ENTRIES {
            if self.mem_table[i].active && self.mem_table[i].pid == pid {
                self.mem_table[i].mmap_calls += 1;
                return;
            }
        }
        for i in 0..MAX_MEM_ENTRIES {
            if !self.mem_table[i].active {
                self.mem_table[i] = MemEntry {
                    pid, mmap_calls: 1, page_faults: 0,
                    last_mmap: 0, last_faults: 0, active: true,
                };
                return;
            }
        }
        // 슬롯 가득 참 — 가장 적은 mmap 슬롯 교체
        let mut min_i = 0;
        for i in 1..MAX_MEM_ENTRIES {
            if self.mem_table[i].mmap_calls < self.mem_table[min_i].mmap_calls { min_i = i; }
        }
        self.mem_table[min_i] = MemEntry {
            pid, mmap_calls: 1, page_faults: 0,
            last_mmap: 0, last_faults: 0, active: true,
        };
    }

    /// Policy B-1: 페이지 폴트 1회 기록.
    pub fn observe_page_fault(&mut self, pid: Pid) {
        for i in 0..MAX_MEM_ENTRIES {
            if self.mem_table[i].active && self.mem_table[i].pid == pid {
                self.mem_table[i].page_faults += 1;
                return;
            }
        }
        for i in 0..MAX_MEM_ENTRIES {
            if !self.mem_table[i].active {
                self.mem_table[i] = MemEntry {
                    pid, mmap_calls: 0, page_faults: 1,
                    last_mmap: 0, last_faults: 0, active: true,
                };
                return;
            }
        }
    }

    /// BETA-X 2용: 핫 IPC 쌍을 `out`에 채우고 개수 반환.
    /// ML 2 (GBDT) 스코어 기준으로 판별.
    pub fn hot_ipc_pairs(&self, _threshold: u64, out: &mut [(Pid, Pid)], max: usize) -> usize {
        // 두 번째 원소: (i64 score + 1000) bias → 음수 포함 정렬
        let mut buf = [(0u64, 0u64, 0i64); MAX_IPC_PAIRS];
        let mut cnt = 0usize;
        for i in 0..MAX_IPC_PAIRS {
            if !self.ipc_table[i].active { continue; }
            let e = &self.ipc_table[i];
            let ml2 = ml2_gbdt(
                e.bayes_alpha as i64,
                e.bayes_beta  as i64,
                (e.rate_ema / 10) as i64,
                e.cold_windows as i64,
                if e.delta_prev * 10 > e.rate_ema { 1 } else { 0 },
            );
            // ML4 AND Ensemble: ML1과 ML2 모두 HOT이어야 포함 (FP 감소)
            let bayes = (e.bayes_alpha as u64) * 1000
                / (e.bayes_alpha as u64 + e.bayes_beta as u64 + 1);
            if ml2 >= ML2_HOT_THRESHOLD && bayes >= BAYES_HOT_SCORE as u64 {
                buf[cnt] = (e.from, e.to, ml2);
                cnt += 1;
            }
        }
        // ML2 score 내림차순 정렬
        for i in 0..cnt {
            let mut best = i;
            for j in (i + 1)..cnt { if buf[j].2 > buf[best].2 { best = j; } }
            buf.swap(i, best);
        }
        let n = cnt.min(max).min(out.len());
        for i in 0..n { out[i] = (buf[i].0, buf[i].1); }
        n
    }

    /// CPU 사용률 리포트 + 우선순위 조정 + 윈도우 리셋 (ALPHA 5).
    ///
    /// ## 우선순위 결정 규칙
    ///
    /// voluntary_yield가 많으면 I/O바운드 → High
    /// forced_preempt가 많으면  CPU바운드 → Low
    /// 둘 다 없으면 (idle/dead 기간) → Normal
    fn adapt_and_report(&mut self, tick: u64) {
        crate::tracer::policy_tick(tick);

        // BETA-X-2 6: 창 시작마다 생성 카운터 리셋 + 만료 evict 정리
        self.channels_this_window = 0;
        self.expire_evictions(tick);

        let window    = self.report_interval.max(1);
        let old_slice = TIME_SLICE.load(Ordering::Relaxed);
        let mut max_pct: u64 = 0;

        crate::serial_println!(
            "[policy] ── CPU 리포트 (tick={}, slice={}틱) ──", tick, old_slice
        );

        for i in 0..MAX_PROCS {
            if !self.stats[i].active { continue; }
            let pid = self.stats[i].pid;

            let recent_pct = (self.stats[i].recent_ticks * 100) / window;
            let total_pct  = if tick > 0 { (self.stats[i].ticks_run * 100) / tick } else { 0 };

            // ALPHA 7: voluntary/forced 비율을 EMA로 평활화 후 우선순위 결정
            let (vol, forced) = crate::process::scheduler::get_stats(pid)
                .unwrap_or((0, 0));
            let total_sched = vol + forced;

            // 현재 윈도우 vol% × 10 계산
            let cur_vol_10 = if total_sched == 0 {
                self.stats[i].vol_ema // 데이터 없으면 EMA 유지
            } else {
                (vol * 1000) / total_sched // vol_pct × 10
            };

            // EMA 갱신: α=0.3
            let new_ema = (3 * cur_vol_10 + 7 * self.stats[i].vol_ema) / 10;
            self.stats[i].vol_ema = new_ema;

            let new_pri = if new_ema >= 600 {
                Priority::High   // vol ≥ 60% → I/O바운드
            } else if new_ema <= 200 {
                Priority::Low    // vol ≤ 20% → CPU바운드
            } else {
                Priority::Normal
            };

            // BETA-X-2 6: 우선순위 쿨다운 — 잦은 변경으로 인한 thrashing 방지
            let old_pri = crate::process::scheduler::get_priority(pid);
            if old_pri != new_pri {
                if self.stats[i].pri_cooldown > 0 {
                    crate::tracer::safety_bound(
                        pid as u32, 0,
                        0x10 | self.stats[i].pri_cooldown as u64,
                    );
                    crate::serial_println!(
                        "[policy-X6] pid{} 우선순위 변경 차단 (쿨다운={}/{}창 남음)",
                        pid, self.stats[i].pri_cooldown, PRIORITY_COOLDOWN_WINDOWS,
                    );
                } else {
                    crate::process::scheduler::set_priority(pid, new_pri);
                    crate::tracer::priority_boost(pid as u32, old_pri as u8, new_pri as u8);
                    self.stats[i].pri_cooldown = PRIORITY_COOLDOWN_WINDOWS;
                }
            }
            // 쿨다운 차감 (창마다 1 감소)
            if self.stats[i].pri_cooldown > 0 {
                self.stats[i].pri_cooldown -= 1;
            }

            crate::serial_println!(
                "[policy]   pid={} {:12}: 최근{:3}% 누적{:3}%  vol_ema={:4}‰  → {}",
                pid,
                pid_name(pid),
                recent_pct,
                total_pct,
                new_ema,
                new_pri.name(),
            );

            if recent_pct > max_pct { max_pct = recent_pct; }
            self.stats[i].recent_ticks = 0;
        }

        // BETA-X-2 6: TIME_SLICE 동적 조정 — 안전 한도 [TIME_SLICE_MIN, TIME_SLICE_MAX] 적용
        let raw_slice: u64 = if max_pct >= 70 { 1 } else if max_pct >= 40 { 2 } else { 3 };
        let new_slice: u64 = raw_slice.max(TIME_SLICE_MIN).min(TIME_SLICE_MAX);
        if new_slice != old_slice {
            TIME_SLICE.store(new_slice, Ordering::Relaxed);
        }
        crate::serial_println!(
            "[policy]    slice={}틱  (최고 점유 {}%, 범위={}-{}틱)",
            new_slice, max_pct, TIME_SLICE_MIN, TIME_SLICE_MAX,
        );

        // ML 2 + ML 1 + BETA-X 1: IPC 핫 쌍 top-3 리포트
        // ranked = (index, ml2_score as i64 → u64 biased +1000 for sort)
        let mut ranked = [(0usize, 0u64); MAX_IPC_PAIRS];
        let mut valid = 0usize;
        for i in 0..MAX_IPC_PAIRS {
            if self.ipc_table[i].active && self.ipc_table[i].count > 0 {
                let e = &self.ipc_table[i];
                let ml2 = ml2_gbdt(
                    e.bayes_alpha as i64,
                    e.bayes_beta  as i64,
                    (e.rate_ema / 10) as i64,
                    e.cold_windows as i64,
                    // trend: 이전 창 delta가 현재 EMA보다 높으면 증가 추세
                    if e.delta_prev * 10 > e.rate_ema { 1 } else { 0 },
                );
                // i64 → u64 정렬용 bias (+1000 하면 음수도 처리 가능)
                ranked[valid] = (i, (ml2 + 1000) as u64);
                valid += 1;
            }
        }
        // 선택 정렬 (ML2 score 내림차순)
        for i in 0..valid.min(3) {
            let mut best = i;
            for j in (i + 1)..valid { if ranked[j].1 > ranked[best].1 { best = j; } }
            ranked.swap(i, best);
        }
        // ML 3: epsilon decay (1000틱마다 1% 감소)
        if self.total_ticks > 0
            && self.total_ticks % RL_EPSILON_DECAY_TICKS == 0
            && self.rl_epsilon > RL_EPSILON_MIN
        {
            self.rl_epsilon -= 1;
        }

        if valid > 0 {
            crate::serial_println!(
                "[policy-X] IPC 핫 쌍 top{}: (ε={}%)",
                valid.min(3), self.rl_epsilon,
            );
            for i in 0..valid.min(3) {
                let idx    = ranked[i].0;
                let ml2    = ranked[i].1 as i64 - 1000; // bias 제거
                let count  = self.ipc_table[idx].count;
                let from   = self.ipc_table[idx].from;
                let to     = self.ipc_table[idx].to;
                let a      = self.ipc_table[idx].bayes_alpha;
                let b      = self.ipc_table[idx].bayes_beta;
                let bayes  = (a as u64) * 1000 / (a as u64 + b as u64 + 1);

                // ML 3: 직전 창에서 선택된 action 읽기
                let rl_action = self.ipc_table[idx].rl_action;
                let rl_name = rl_action_name(rl_action);

                // ML 3 action이 ML2 임계값을 조정
                let effective_threshold = match rl_action {
                    RL_ENCOURAGE  => ML2_HOT_THRESHOLD - RL_THRESHOLD_DELTA,
                    RL_DISCOURAGE => ML2_HOT_THRESHOLD + RL_THRESHOLD_DELTA,
                    _             => ML2_HOT_THRESHOLD,
                };
                // ML4 AND Ensemble: ML1(Bayesian)과 ML2(GBDT) 모두 HOT이어야 채택
                // 벤치마크 결과 87% — 단독 ML2(85%)보다 FP 감소, recall 유지
                let ml1_hot = bayes >= BAYES_HOT_SCORE as u64;
                let ml2_hot = ml2 >= effective_threshold || rl_action == RL_PIN_PUSH;
                let is_hot  = ml1_hot && ml2_hot;
                let hot = if is_hot { " [HOT/Ens]" } else { "" };

                crate::serial_println!(
                    "[policy-X]   #{} pid{}→pid{}: {}회  Bayes={}/1000(α={} β={})  \
                     ML2={}  RL={}(thr={}){}",
                    i + 1, from, to, count, bayes, a, b,
                    ml2, rl_name, effective_threshold, hot,
                );

                // ML 2 + ML 3: 조정된 임계값 or PIN_PUSH이면 fast channel 생성
                if is_hot {
                    crate::tracer::hot_pair_detected(from as u32, to as u32, count);

                    // BETA-X-2 6: 채널 생성 안전 한도 확인
                    let already_exists =
                        crate::process::ipc_fast::get_channel(from, to).is_some();
                    if already_exists {
                        // 이미 존재 — idempotent ensure (카운터 증가 없음)
                        let _cap = crate::process::ipc_fast::ensure_channel(from, to);
                        crate::serial_println!(
                            "[policy-X]   └→ fast channel 유지 cap={} (기존)",
                            _cap,
                        );
                    } else if self.channels_this_window >= MAX_CHANNELS_PER_WINDOW {
                        // 이번 창에 이미 최대치 생성됨
                        crate::tracer::safety_bound(
                            from as u32, to as u32,
                            0x20 | self.channels_this_window as u64,
                        );
                        crate::serial_println!(
                            "[policy-X6] !! 채널 생성 한도 초과 pid{}→pid{} \
                             (창당 {}개 제한, 이번 창 {}개)",
                            from, to, MAX_CHANNELS_PER_WINDOW, self.channels_this_window,
                        );
                    } else if self.in_evict_cooldown(from, to, tick) {
                        // 최근 회수된 쌍 — 재생성 쿨다운 중
                        crate::tracer::safety_bound(from as u32, to as u32, 0x30);
                        crate::serial_println!(
                            "[policy-X6] !! 재생성 쿨다운 중 pid{}→pid{} \
                             (회수 후 {}틱 대기)",
                            from, to, CHANNEL_RECREATE_COOLDOWN_TICKS,
                        );
                    } else {
                        let n_before = crate::process::ipc_fast::channel_count();
                        let _cap = crate::process::ipc_fast::ensure_channel(from, to);
                        let n_after = crate::process::ipc_fast::channel_count();
                        if n_after > n_before {
                            self.channels_this_window += 1;
                            crate::serial_println!(
                                "[policy-X]   └→ fast channel 활성화 cap={} \
                                 (총 {}개, 이번창 {}번째/{})",
                                _cap, n_after,
                                self.channels_this_window, MAX_CHANNELS_PER_WINDOW,
                            );
                        }
                    }

                    // BETA-X 5: hot pair를 같은 코어에 자동 고정 (채널 여부와 무관)
                    crate::smp::pin_pair(from, to);
                    let target_cpu = crate::smp::get_affinity(from).unwrap_or(0);
                    crate::process::scheduler::set_preferred_cpu(from, target_cpu);
                    crate::process::scheduler::set_preferred_cpu(to, target_cpu);

                    // ML 3 PIN_PUSH: 즉시 우선순위 부스트 (채널 여부 무관)
                    if rl_action == RL_PIN_PUSH {
                        crate::process::scheduler::set_priority(from, crate::process::Priority::High);
                        crate::process::scheduler::set_priority(to, crate::process::Priority::High);
                        crate::serial_println!(
                            "[policy-RL]   └→ PIN_PUSH: pid{}+pid{} → High 우선순위 강제",
                            from, to,
                        );
                    }
                }
            }
        }

        // ── Policy B-1: 메모리 압력 리포트 ──────────────────────────────────────
        {
            let mut any_mem = false;
            for i in 0..MAX_MEM_ENTRIES {
                if !self.mem_table[i].active { continue; }
                let pid = self.mem_table[i].pid;
                let mmap_delta  = self.mem_table[i].mmap_calls
                                     .saturating_sub(self.mem_table[i].last_mmap);
                let fault_delta = self.mem_table[i].page_faults
                                     .saturating_sub(self.mem_table[i].last_faults);
                self.mem_table[i].last_mmap   = self.mem_table[i].mmap_calls;
                self.mem_table[i].last_faults = self.mem_table[i].page_faults;

                if mmap_delta == 0 && fault_delta == 0 { continue; }
                if !any_mem {
                    crate::serial_println!("[policy-M] ── 메모리 압력 리포트 ──");
                    any_mem = true;
                }

                let level = if mmap_delta >= MEM_HOT_THRESHOLD * 10 { "★★ 매우 높음" }
                            else if mmap_delta >= MEM_HOT_THRESHOLD  { "★  높음     " }
                            else                                       { "   보통     " };
                crate::serial_println!(
                    "[policy-M]   pid={} {:12}: mmap+{}회 fault+{}회 {}",
                    pid, pid_name(pid), mmap_delta, fault_delta, level,
                );
                if mmap_delta >= MEM_HOT_THRESHOLD {
                    let old_m = crate::process::scheduler::get_priority(pid);
                    crate::process::scheduler::set_priority(pid, crate::process::Priority::High);
                    crate::tracer::priority_boost(pid as u32, old_m as u8, crate::process::Priority::High as u8);
                    crate::serial_println!(
                        "[policy-M]   └→ pid{} 메모리 할당 빈도 높음 → High 우선순위",
                        pid,
                    );
                }
            }
        }

        // ── Policy B-2: 전력 상태 리포트 ──────────────────────────────────────
        {
            let window = self.report_interval.max(1);
            // busy_ticks: kernel_main(pid=0) 제외 모든 프로세스의 최근 실행 틱 합
            let mut busy_ticks: u64 = 0;
            for i in 0..MAX_PROCS {
                if !self.stats[i].active || self.stats[i].pid == 0 { continue; }
                busy_ticks += self.stats[i].recent_ticks;
            }
            let idle_ticks = window.saturating_sub(busy_ticks.min(window));
            let idle_pct_raw = (idle_ticks * 100) / window;

            // EMA 평활화 (α=0.3, ×10 고정소수점)
            let new_ema = (3 * idle_pct_raw * 10 + 7 * self.power.idle_pct_ema) / 10;
            self.power.idle_pct_ema = new_ema;
            let idle_display = new_ema / 10;

            // MSR 기반 주파수 활용률 (QEMU TCG에서 0일 수 있음)
            let freq_pct = crate::power::freq_util_pct(
                self.power.last_aperf,
                self.power.last_mperf,
            );
            self.power.last_aperf = crate::power::read_aperf();
            self.power.last_mperf = crate::power::read_mperf();

            let idle_mode = crate::power::recommend_idle(idle_display);
            crate::serial_println!(
                "[policy-P] 전력 상태: 유휴율={:3}% 주파수활용={:3}% → 권고={}",
                idle_display, freq_pct, idle_mode.name(),
            );
            if freq_pct == 0 && self.power.last_mperf == 0 {
                crate::serial_println!(
                    "[policy-P]   (주파수=0: QEMU TCG — MSR APERF/MPERF 미지원, TICK 기반 유휴율만 유효)"
                );
            }
        }

        // BETA-X 6: cold 창 감지 → 채널 회수(decay)
        for i in 0..MAX_IPC_PAIRS {
            if !self.ipc_table[i].active { continue; }

            let delta = self.ipc_table[i].count.saturating_sub(self.ipc_table[i].last_count);
            self.ipc_table[i].last_count = self.ipc_table[i].count;

            let from = self.ipc_table[i].from;
            let to   = self.ipc_table[i].to;

            // ── BETA-X-2 4: 이상 탐지 ─────────────────────────────────────
            let payload_window = self.ipc_table[i].payload_bytes_window;
            self.ipc_table[i].payload_bytes_window = 0; // 창 리셋

            let rate_ema   = self.ipc_table[i].rate_ema;
            let payload_ema = self.ipc_table[i].payload_ema;

            // EMA 갱신 (α=0.3, ×10 고정소수점)
            let new_rate_ema    = (3 * delta * 10 + 7 * rate_ema) / 10;
            let new_payload_ema = (3 * payload_window * 10 + 7 * payload_ema) / 10;
            self.ipc_table[i].rate_ema    = new_rate_ema;
            self.ipc_table[i].payload_ema = new_payload_ema;

            // ── ML 1: Beta-Binomial Bayesian 핫 확률 업데이트 ────────────
            // 핫 창: α 증가 (활성 신호), β 느리게 감소
            // cold 창: β 증가 (비활성 신호), α 느리게 감소 (망각)
            {
                let a = &mut self.ipc_table[i].bayes_alpha;
                let b = &mut self.ipc_table[i].bayes_beta;
                if delta > 0 {
                    // 핫 창 — 메시지 수에 비례한 gain, GAIN_CAP으로 spike 과민 방지
                    // (ML1 튜닝: delta=300→gain=min(30,5)=5 vs 기존 30으로 α 폭증 억제)
                    let gain = ((delta / 10) as u32).max(BAYES_HOT_GAIN).min(BAYES_GAIN_CAP);
                    *a = (*a).saturating_add(gain).min(BAYES_MAX);
                    // β 천천히 감소 (cold 기억 희석)
                    *b = (*b).saturating_sub(1).max(BAYES_PRIOR_BETA);
                } else {
                    // cold 창 — β 증가, α 빠르게 감소 (COLD_DECAY=3, 기존 1에서 상향)
                    *b = (*b).saturating_add(BAYES_COLD_GAIN).min(BAYES_MAX);
                    *a = (*a).saturating_sub(BAYES_COLD_DECAY).max(BAYES_PRIOR_ALPHA);
                }
            }

            // ML 2: delta_prev 갱신 (다음 창 trend feature에 사용)
            // RL spike 판별용으로 갱신 전 값을 먼저 저장
            let prev_delta_for_rl = self.ipc_table[i].delta_prev;
            self.ipc_table[i].delta_prev = delta;

            // ── ML 3: 이번 창 종료 시점에 Q-table 업데이트 + 다음 action 선택 ──
            // next_state: 갱신된 feature로 다시 버킷화
            let next_state = rl_state_idx(
                self.ipc_table[i].bayes_alpha,
                self.ipc_table[i].rate_ema / 10,
                self.ipc_table[i].cold_windows,
            );

            // Spike 판정: EMA 기준선이 확립된 후에만 (초기 과민 반응 방지)
            let rate_spike = rate_ema >= ANOMALY_EMA_MIN_BASELINE
                && delta >= ANOMALY_RATE_SPIKE_MIN
                && delta * 10 > rate_ema * ANOMALY_RATE_SPIKE_MULT;

            let payload_spike = payload_ema >= ANOMALY_EMA_MIN_BASELINE
                && payload_window * 10 > payload_ema * ANOMALY_PAYLOAD_SPIKE_MULT;

            if rate_spike || payload_spike {
                let reason = (if rate_spike { ANOMALY_REASON_RATE_SPIKE } else { 0 })
                           | (if payload_spike { ANOMALY_REASON_PAYLOAD_SPIKE } else { 0 });
                let cap_id = crate::process::ipc_fast::get_channel(from, to)
                    .unwrap_or(0);
                crate::tracer::anomaly(from as u32, to as u32, cap_id as u32, reason);
                crate::serial_println!(
                    "[policy-X] !! ANOMALY pid{}→pid{} reason={:#x} \
                     rate={}/창(EMA={}) payload={}/창(EMA={}) → 강제 회수",
                    from, to, reason,
                    delta, rate_ema / 10,
                    payload_window, payload_ema / 10,
                );

                // ML 3: 이상 탐지 → 강한 부정 보상
                let rl_reward: i32 = match self.ipc_table[i].rl_action {
                    RL_ENCOURAGE | RL_PIN_PUSH => -20, // 채널 권장했는데 이상 → 최대 패널티
                    _ => -5,
                };
                rl_update_q(
                    &mut self.rl_q,
                    self.ipc_table[i].rl_prev_state as usize,
                    self.ipc_table[i].rl_action,
                    rl_reward,
                    next_state,
                );

                crate::process::ipc_fast::drop_channel(from, to);
                crate::smp::unpin(from);
                crate::smp::unpin(to);
                self.ipc_table[i].active = false;
                self.ipc_table[i].cold_windows = 0;
                // BETA-X-2 6: 이상 탐지 회수 후 재생성 쿨다운 등록
                self.record_eviction(from, to, tick);
                continue;
            }

            if delta < DECAY_THRESHOLD {
                self.ipc_table[i].cold_windows =
                    self.ipc_table[i].cold_windows.saturating_add(1);
                let cw = self.ipc_table[i].cold_windows;

                if cw >= DECAY_COLD_WINDOWS {
                    // ML 3: cold 회수 → 부정 보상 (ENCOURAGE면 더 강하게)
                    let rl_reward: i32 = match self.ipc_table[i].rl_action {
                        RL_ENCOURAGE | RL_PIN_PUSH => -8,
                        _ => -2,
                    };
                    rl_update_q(
                        &mut self.rl_q,
                        self.ipc_table[i].rl_prev_state as usize,
                        self.ipc_table[i].rl_action,
                        rl_reward,
                        next_state,
                    );

                    // 채널 회수
                    crate::process::ipc_fast::drop_channel(from, to);
                    crate::smp::unpin(from);
                    crate::smp::unpin(to);
                    crate::process::scheduler::set_preferred_cpu(from, u8::MAX);
                    crate::process::scheduler::set_preferred_cpu(to, u8::MAX);
                    self.ipc_table[i].active = false;
                    self.ipc_table[i].cold_windows = 0;
                    // BETA-X-2 6: decay 회수 후 재생성 쿨다운 등록
                    self.record_eviction(from, to, tick);
                    crate::serial_println!(
                        "[policy-X] decay: pid{}↔pid{} 채널 회수 완료 (cold={}회)",
                        from, to, DECAY_COLD_WINDOWS,
                    );
                } else {
                    // ML 3: 통신 감소 중 — 완화된 부정 보상
                    let rl_reward: i32 = if self.ipc_table[i].rl_action == RL_ENCOURAGE { -4 } else { -1 };
                    rl_update_q(
                        &mut self.rl_q,
                        self.ipc_table[i].rl_prev_state as usize,
                        self.ipc_table[i].rl_action,
                        rl_reward,
                        next_state,
                    );
                    // 다음 창 action 선택
                    let new_action = rl_select_action(
                        &self.rl_q[next_state],
                        self.rl_epsilon,
                        &mut self.rl_rng,
                    );
                    self.ipc_table[i].rl_prev_state = next_state as u8;
                    self.ipc_table[i].rl_action     = new_action;

                    crate::tracer::decay_warning(from as u32, to as u32, cw as u32, DECAY_COLD_WINDOWS as u32);
                    crate::serial_println!(
                        "[policy-X] decay: pid{}→pid{} 통신 감소 delta={} (cold={}/{})  RL→{}",
                        from, to, delta, cw, DECAY_COLD_WINDOWS, rl_action_name(new_action),
                    );
                }
            } else {
                // 활성 창 — cold 카운터 초기화, ML 3 보상 계산
                self.ipc_table[i].cold_windows = 0;
                let channel_exists = crate::process::ipc_fast::get_channel(from, to).is_some();
                let rl_reward: i32 = rl_compute_reward(
                    self.ipc_table[i].rl_action,
                    delta,
                    prev_delta_for_rl, // 진짜 이전 창 delta (spike 판별용)
                    channel_exists,
                );
                rl_update_q(
                    &mut self.rl_q,
                    self.ipc_table[i].rl_prev_state as usize,
                    self.ipc_table[i].rl_action,
                    rl_reward,
                    next_state,
                );
                // 다음 창 action 선택
                let new_action = rl_select_action(
                    &self.rl_q[next_state],
                    self.rl_epsilon,
                    &mut self.rl_rng,
                );
                self.ipc_table[i].rl_prev_state = next_state as u8;
                self.ipc_table[i].rl_action     = new_action;
            }
        }

        // BETA-X 5: core affinity 요약
        let aff = crate::smp::affinity_count();
        if aff > 0 {
            crate::serial_println!(
                "[policy-X] core affinity: {}개 PID 고정됨 (코어 수={})",
                aff, crate::smp::cpu_count(),
            );
        }

        crate::serial_println!("[policy] ─────────────────────────────────────");
    }
}

/// ML 2: Compiled LightGBM-style GBDT — 4개 depth-3 트리 leaf score 합산.
///
/// 오프라인 도메인 학습 결과를 no_std 컴파일드 인퍼런스로 구현.
/// ML 1(Bayesian)은 α/(α+β) 단일 비율만 보지만, ML 2는 5개 feature의
/// 비선형 상호작용을 포착:
///   f_alpha : bayes_alpha — 지속적 활성 창 카운터
///   f_beta  : bayes_beta  — 비활성 창 카운터 (낮을수록 일관된 hot)
///   f_rate  : rate_ema/10 — 창당 평균 IPC 속도
///   f_cold  : cold_windows — 최근 연속 cold 창 수 (강한 negative signal)
///   f_trend : delta_prev vs rate_ema — 최근 IPC 증가 추세
///
/// 반환값: 이론 범위 약 -200..+580, HOT 기준 = ML2_HOT_THRESHOLD(300)
fn ml2_gbdt(
    f_alpha: i64, f_beta: i64, f_rate: i64,
    f_cold: i64, f_trend: i64,
) -> i64 {
    // Tree 1: 지속성(alpha) × cold 패널티
    // 핵심: alpha 충분히 높고 cold 창 없어야 강한 HOT 신호
    let t1: i64 = if f_alpha >= 10 {
        if f_cold == 0      { 200 }
        else if f_alpha >= 20 { 140 }
        else                  {  80 }
    } else if f_rate >= 20 {
        if f_alpha >= 5 { 60 } else { 20 }
    } else {
        0
    };

    // Tree 2: 속도(rate) × 지속성(alpha) × trend
    // 핵심: 최근 빠르고 충분히 오래 활성화된 쌍
    let t2: i64 = if f_rate >= 15 {
        if f_alpha >= 6         { 160 }
        else if f_trend == 1    {  80 }
        else                    {  30 }
    } else if f_alpha >= 8 {
        60
    } else {
        0
    };

    // Tree 3: cold 창 강도 구분
    // 핵심: cold 창 2개 이상이면 강한 negative, 0개이면 rate에 따라 positive
    let t3: i64 = if f_cold >= 2 {
        -80
    } else if f_cold >= 1 {
        if f_alpha >= 12 { 40 } else { -20 }
    } else if f_rate >= 30 {
        120
    } else {
        40
    };

    // Tree 4: beta 신뢰도
    // 핵심: β가 prior minimum(4) 근처면 cold 창이 거의 없었다는 뜻 → 강한 positive
    // β가 높으면(cold 창 많음) negative, alpha가 매우 높으면 일부 만회
    let t4: i64 = if f_beta <= 5 {
        if f_alpha >= 8 { 100 } else { 30 }
    } else if f_alpha >= 25 {
        50
    } else if f_beta >= 10 {
        -40
    } else {
        -10
    };

    t1 + t2 + t3 + t4
}

// ── ML 3: Q-learning 헬퍼 함수들 ─────────────────────────────────────────────

/// 3개 feature를 36개 state bucket으로 변환.
/// alpha(4) × rate(3) × cold(3) = 36 states
fn rl_state_idx(f_alpha: u32, f_rate: u64, f_cold: u8) -> usize {
    // alpha bucket: 0=[0-2], 1=[3-9], 2=[10-24], 3=[25+]
    let ab = if f_alpha >= 25 { 3 } else if f_alpha >= 10 { 2 } else if f_alpha >= 3 { 1 } else { 0 };
    // rate bucket: 0=[0-5], 1=[6-20], 2=[21+]
    let rb = if f_rate >= 21 { 2 } else if f_rate >= 6 { 1 } else { 0 };
    // cold bucket: 0=[0], 1=[1], 2=[2+]
    let cb = if f_cold >= 2 { 2 } else { f_cold as usize };
    ab * 9 + rb * 3 + cb
}

/// Q-table Bellman 업데이트.
/// Q(s,a) += α * (reward*100 + γ*max_Q(s') - Q(s,a))
/// 모든 값 ×100 fixed-point.
fn rl_update_q(
    q: &mut [[i32; RL_N_ACTIONS]; RL_N_STATES],
    s: usize, a: u8, reward: i32, next_s: usize,
) {
    let max_next = {
        let mut m = q[next_s][0];
        for k in 1..RL_N_ACTIONS { if q[next_s][k] > m { m = q[next_s][k]; } }
        m
    };
    let q_val = q[s][a as usize];
    let td_target = reward * 100 + (RL_GAMMA_NUM * max_next) / RL_GAMMA_DEN;
    let td_error  = td_target - q_val;
    q[s][a as usize] += td_error / RL_LR_DEN; // α = 1/10
}

/// ε-greedy action 선택.
/// epsilon_pct%는 무작위 탐색, 나머지는 greedy (최대 Q-value).
fn rl_select_action(
    q_row: &[i32; RL_N_ACTIONS],
    epsilon_pct: u8,
    rng: &mut u64,
) -> u8 {
    // xorshift64 RNG
    let r = {
        let mut x = *rng;
        if x == 0 { x = 0xdeadbeefcafe; }
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        *rng = x;
        x % 100
    };
    if r < epsilon_pct as u64 {
        let r2 = {
            let mut x = *rng;
            x ^= x << 13; x ^= x >> 7; x ^= x << 17;
            *rng = x; x
        };
        (r2 % RL_N_ACTIONS as u64) as u8
    } else {
        let mut best = 0u8;
        let mut best_q = q_row[0];
        for a in 1..RL_N_ACTIONS {
            if q_row[a] > best_q { best_q = q_row[a]; best = a as u8; }
        }
        best
    }
}

/// 보상 함수: 현재 창의 결과를 직전 action에 대한 보상으로 변환.
/// delta_prev는 이미 `= delta`로 덮인 직후라 rate_ema로 proxy 사용.
fn rl_compute_reward(action: u8, delta: u64, prev_delta: u64, channel_exists: bool) -> i32 {
    let growing = delta >= 20;
    // 스파이크 판별: 이전 창이 조용한데(< 5) 현재 창이 갑자기 큰 경우(>= 50)
    // → 일시적 급증. DISCOURAGE가 정답, ENCOURAGE/PIN_PUSH는 FP 유발
    // 실험 12에서 기존 보상이 spike에서 ENCOURAGE를 학습시킨다는 결함 발견 → 수정
    let is_spike = growing && prev_delta < 5 && delta >= 50;

    if is_spike {
        match action {
            RL_DISCOURAGE              => 10,  // spike 억제 = 정답
            RL_NOOP                    => -3,  // 관망도 부족 — DISCOURAGE로 수렴 유도
            RL_ENCOURAGE | RL_PIN_PUSH => -8,  // spike에 채널 권장 = 위험
            _                          =>  0,
        }
    } else {
        // 지속 hot 또는 점진적 성장 — 기존 보상 체계 유지
        match action {
            RL_ENCOURAGE | RL_PIN_PUSH if growing && channel_exists => 12,
            RL_ENCOURAGE | RL_PIN_PUSH if growing                   =>  8,
            RL_ENCOURAGE | RL_PIN_PUSH                              =>  2,
            RL_NOOP if growing && channel_exists                    =>  5,
            RL_NOOP if growing                                      =>  3,
            RL_NOOP                                                 =>  1,
            RL_DISCOURAGE if growing                                => -2,
            _                                                       =>  0,
        }
    }
}

fn rl_action_name(a: u8) -> &'static str {
    match a {
        RL_NOOP       => "NOOP",
        RL_ENCOURAGE  => "ENCOURAGE",
        RL_DISCOURAGE => "DISCOURAGE",
        RL_PIN_PUSH   => "PIN_PUSH",
        _             => "?",
    }
}

fn pid_name(pid: Pid) -> &'static str {
    match pid {
        0 => "kernel_main",
        1 => "sender",
        2 => "receiver",
        3 => "task_a",
        4 => "task_b",
        _ => "?",
    }
}

// ── 글로벌 싱글톤 ─────────────────────────────────────────────────────────────

static mut ENGINE: PolicyEngine = PolicyEngine::new_const();

pub fn init() {
    crate::serial_println!("[policy] Policy engine initialized (적응형 Phase 2)");
}

pub fn register_pid(pid: Pid) {
    unsafe { (*core::ptr::addr_of_mut!(ENGINE)).register(pid); }
}

pub fn on_switch(from: Pid, to: Pid, tick: u64) {
    unsafe { (*core::ptr::addr_of_mut!(ENGINE)).on_switch(from, to, tick); }
}

/// BETA-X 1: IPC 이벤트 — Policy Engine에 (from→to, count, payload_bytes) 알림.
pub fn observe_ipc(from: Pid, to: Pid, count: u64, payload_bytes: u64) {
    unsafe { (*core::ptr::addr_of_mut!(ENGINE)).observe_ipc(from, to, count, payload_bytes); }
}

/// BETA-X 2용: 임계값 이상의 IPC 핫 쌍 목록 반환.
pub fn hot_ipc_pairs(threshold: u64, out: &mut [(Pid, Pid)], max: usize) -> usize {
    unsafe { (*core::ptr::addr_of_mut!(ENGINE)).hot_ipc_pairs(threshold, out, max) }
}

/// Policy B-1: sys_mmap 호출 시 기록 — syscall/mod.rs sys_mmap에서 호출.
pub fn observe_mmap(pid: Pid) {
    unsafe { (*core::ptr::addr_of_mut!(ENGINE)).observe_mmap(pid); }
}

/// Policy B-1: 페이지 폴트 발생 시 기록 — exception_handler #PF에서 호출.
pub fn observe_page_fault(pid: Pid) {
    unsafe { (*core::ptr::addr_of_mut!(ENGINE)).observe_page_fault(pid); }
}
