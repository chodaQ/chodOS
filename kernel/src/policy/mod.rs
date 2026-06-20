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
            vol_ema: 500, // 50.0% — neutral 초기값
        }
    }
}

const MAX_PROCS: usize = 32;

// ── Policy Engine ────────────────────────────────────────────────────────────

pub struct PolicyEngine {
    stats: [CpuStats; MAX_PROCS],
    total_ticks: u64,
    last_report_tick: u64,
    report_interval: u64,
}

impl PolicyEngine {
    const fn new_const() -> Self {
        PolicyEngine {
            stats: [CpuStats::empty(); MAX_PROCS],
            total_ticks: 0,
            last_report_tick: 0,
            report_interval: 36,
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

    /// CPU 사용률 리포트 + 우선순위 조정 + 윈도우 리셋 (ALPHA 5).
    ///
    /// ## 우선순위 결정 규칙
    ///
    /// voluntary_yield가 많으면 I/O바운드 → High
    /// forced_preempt가 많으면  CPU바운드 → Low
    /// 둘 다 없으면 (idle/dead 기간) → Normal
    fn adapt_and_report(&mut self, tick: u64) {
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

            crate::process::scheduler::set_priority(pid, new_pri);

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

        // TIME_SLICE는 전역 기준선 유지 (선택 빈도로 차별화하므로 큰 값 불필요)
        let new_slice: u64 = 3;
        if new_slice != old_slice {
            TIME_SLICE.store(new_slice, Ordering::Relaxed);
        }
        crate::serial_println!(
            "[policy]    slice={}틱 고정  (최고 점유 {}%)",
            new_slice, max_pct,
        );
        crate::serial_println!("[policy] ─────────────────────────────────────");
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
    unsafe { ENGINE.register(pid); }
}

pub fn on_switch(from: Pid, to: Pid, tick: u64) {
    unsafe { ENGINE.on_switch(from, to, tick); }
}
