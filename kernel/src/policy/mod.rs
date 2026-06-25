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
}

impl IpcEntry {
    const fn empty() -> Self {
        IpcEntry { from: 0, to: 0, count: 0, active: false, last_count: 0, cold_windows: 0 }
    }
}

/// IPC 핫-쌍 임계값: 이 횟수 이상이면 전용 채널 후보 (BETA-X 2에서 사용)
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

    /// BETA-X 1: IPC 이벤트 수신 — from→to 누적 카운트 갱신.
    pub fn observe_ipc(&mut self, from: Pid, to: Pid, count: u64) {
        // 기존 항목 업데이트
        for i in 0..MAX_IPC_PAIRS {
            if self.ipc_table[i].active
                && self.ipc_table[i].from == from
                && self.ipc_table[i].to == to
            {
                self.ipc_table[i].count = count;
                return;
            }
        }
        // 빈 슬롯에 신규 등록
        for i in 0..MAX_IPC_PAIRS {
            if !self.ipc_table[i].active {
                self.ipc_table[i] = IpcEntry { from, to, count, active: true, last_count: 0, cold_windows: 0 };
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
        self.ipc_table[min_i] = IpcEntry { from, to, count, active: true, last_count: 0, cold_windows: 0 };
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

    /// BETA-X 2용: 임계값 이상의 핫 IPC 쌍을 `out`에 채우고 개수 반환.
    pub fn hot_ipc_pairs(&self, threshold: u64, out: &mut [(Pid, Pid)], max: usize) -> usize {
        // 후보 수집
        let mut buf = [(0u64, 0u64, 0u64); MAX_IPC_PAIRS];
        let mut cnt = 0usize;
        for i in 0..MAX_IPC_PAIRS {
            if self.ipc_table[i].active && self.ipc_table[i].count >= threshold {
                buf[cnt] = (self.ipc_table[i].from, self.ipc_table[i].to, self.ipc_table[i].count);
                cnt += 1;
            }
        }
        // count 내림차순 선택 정렬
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

        // BETA-X 1: IPC 핫 쌍 top-3 리포트
        let mut ranked = [(0usize, 0u64); MAX_IPC_PAIRS];
        let mut valid = 0usize;
        for i in 0..MAX_IPC_PAIRS {
            if self.ipc_table[i].active && self.ipc_table[i].count > 0 {
                ranked[valid] = (i, self.ipc_table[i].count);
                valid += 1;
            }
        }
        // 선택 정렬 (count 내림차순)
        for i in 0..valid.min(3) {
            let mut best = i;
            for j in (i + 1)..valid { if ranked[j].1 > ranked[best].1 { best = j; } }
            ranked.swap(i, best);
        }
        if valid > 0 {
            crate::serial_println!("[policy-X] IPC 핫 쌍 top{}:", valid.min(3));
            for i in 0..valid.min(3) {
                let idx = ranked[i].0;
                let count = self.ipc_table[idx].count;
                let from  = self.ipc_table[idx].from;
                let to    = self.ipc_table[idx].to;
                let hot   = if count >= IPC_HOT_THRESHOLD { " [HOT]" } else { "" };
                crate::serial_println!(
                    "[policy-X]   #{} pid{}→pid{}: {}회{}",
                    i + 1, from, to, count, hot,
                );

                // BETA-X 2: 임계값 초과 쌍에 fast channel 자동 생성
                if count >= IPC_HOT_THRESHOLD {
                    let n_before = crate::process::ipc_fast::channel_count();
                    let _cap = crate::process::ipc_fast::ensure_channel(from, to);
                    let n_after = crate::process::ipc_fast::channel_count();
                    if n_after > n_before {
                        crate::serial_println!(
                            "[policy-X]   └→ fast channel 활성화 cap={} (총 {}개)",
                            _cap, n_after,
                        );
                    }

                    // BETA-X 5: hot pair를 같은 코어에 자동 고정
                    crate::smp::pin_pair(from, to);
                    // PCB preferred_cpu도 갱신
                    let target_cpu = crate::smp::get_affinity(from).unwrap_or(0);
                    crate::process::scheduler::set_preferred_cpu(from, target_cpu);
                    crate::process::scheduler::set_preferred_cpu(to, target_cpu);
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
                    crate::process::scheduler::set_priority(pid, crate::process::Priority::High);
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

            if delta < DECAY_THRESHOLD {
                self.ipc_table[i].cold_windows =
                    self.ipc_table[i].cold_windows.saturating_add(1);
                let cw = self.ipc_table[i].cold_windows;

                if cw >= DECAY_COLD_WINDOWS {
                    // 채널 회수
                    crate::process::ipc_fast::drop_channel(from, to);
                    crate::smp::unpin(from);
                    crate::smp::unpin(to);
                    crate::process::scheduler::set_preferred_cpu(from, u8::MAX);
                    crate::process::scheduler::set_preferred_cpu(to, u8::MAX);
                    self.ipc_table[i].active = false;
                    self.ipc_table[i].cold_windows = 0;
                    crate::serial_println!(
                        "[policy-X] decay: pid{}↔pid{} 채널 회수 완료 (cold={}회)",
                        from, to, DECAY_COLD_WINDOWS,
                    );
                } else {
                    crate::serial_println!(
                        "[policy-X] decay: pid{}→pid{} 통신 감소 delta={} (cold={}/{})",
                        from, to, delta, cw, DECAY_COLD_WINDOWS,
                    );
                }
            } else {
                // 활성 — cold 카운터 초기화
                self.ipc_table[i].cold_windows = 0;
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

/// BETA-X 1: IPC 이벤트 — Policy Engine에 (from→to, count) 알림.
pub fn observe_ipc(from: Pid, to: Pid, count: u64) {
    unsafe { (*core::ptr::addr_of_mut!(ENGINE)).observe_ipc(from, to, count); }
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
