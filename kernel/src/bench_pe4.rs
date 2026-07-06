//! PE-4: Policy Engine A/B 벤치마크 (on vs off)
//!
//! ## 목적
//!
//! ARCHITECTURE.md PE-4: "Policy Engine 자체가 효과 있는지" 증명.
//! 같은 워크로드(CPU바운드 배경 작업 + 인터랙티브 전경 작업)를
//! Policy Engine on/off 두 조건에서 실행해 다음을 비교한다:
//!
//! 1. 키 입력 → 반영 레이턴시 (rdtsc)
//! 2. 36틱 동안 컨텍스트 스위치 횟수
//! 3. CPU바운드 작업 완료 시간 (starvation 여부)
//!
//! ## 워크로드
//!
//! - `cpu_hog_task`: 절대 자발적으로 yield하지 않는 순수 CPU바운드 루프.
//!   고정된 반복 횟수(HOG_TARGET_ITERS)를 채우면 완료 시각(tick)을 기록.
//! - `kbd_task`: 대부분 대기하다가 "키 입력" 신호를 받으면 즉시 반응하는
//!   인터랙티브 전경 작업 시뮬레이션.
//!
//! ## 키 입력 시뮬레이션
//!
//! 실제 i8042 스캔코드 대신 `process::scheduler::keyboard_boost()`를
//! 직접 호출한다 — 실제 IRQ1 핸들러가 scancode 판별 후 호출하는 것과
//! 동일한 코드 경로이므로, 포트 0x60 접근(테스트 환경에 실제 키 입력 없음)
//! 없이도 스케줄러 반응을 정확히 측정할 수 있다.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub const N_SAMPLES: usize = 40;
/// cpu_hog가 채워야 하는 반복 횟수 — 부팅시간 대비 측정 유효성 균형.
const HOG_TARGET_ITERS: u64 = 20_000_000;
/// 키 입력 시뮬레이션 간격 (틱).
const KEYPRESS_INTERVAL_TICKS: u64 = 3;

pub static KBD_TRIGGER_TS: AtomicU64 = AtomicU64::new(0);
static KBD_LATS: [AtomicU64; N_SAMPLES] = [const { AtomicU64::new(0) }; N_SAMPLES];
pub static KBD_IDX: AtomicUsize = AtomicUsize::new(0);

pub static CPU_HOG_ITERS: AtomicU64 = AtomicU64::new(0);
pub static CPU_HOG_DONE_TICK: AtomicU64 = AtomicU64::new(0);

#[inline(always)]
fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    ((hi as u64) << 32) | lo as u64
}

/// CPU바운드 배경 작업 — 절대 yield하지 않음(타이머 강제 선점에만 의존).
pub fn cpu_hog_task() -> ! {
    let mut x: u64 = 0xdeadbeef;
    loop {
        for _ in 0..1000 {
            x = x.wrapping_mul(2654435761).wrapping_add(1);
        }
        core::hint::black_box(x);
        let n = CPU_HOG_ITERS.fetch_add(1000, Ordering::Relaxed) + 1000;
        if n >= HOG_TARGET_ITERS && CPU_HOG_DONE_TICK.load(Ordering::Relaxed) == 0 {
            let tick = crate::interrupts::handlers::TICK.load(Ordering::Relaxed);
            CPU_HOG_DONE_TICK.store(tick.max(1), Ordering::Relaxed);
        }
    }
}

/// 인터랙티브 전경 작업 — 키 입력 신호를 기다렸다가 즉시 latency 기록.
pub fn kbd_task() -> ! {
    loop {
        let ts = KBD_TRIGGER_TS.swap(0, Ordering::SeqCst);
        if ts != 0 {
            let now = rdtsc();
            let idx = KBD_IDX.fetch_add(1, Ordering::SeqCst);
            if idx < N_SAMPLES {
                KBD_LATS[idx].store(now.saturating_sub(ts), Ordering::Relaxed);
            }
        }
        crate::process::scheduler::yield_now();
    }
}

/// 벤치마크 1회 실행 (Policy Engine on 또는 off 상태에서 호출).
///
/// 반환: (평균 키입력 레이턴시(cycles), 36틱당 컨텍스트 스위치 수, cpu_hog 완료 틱)
pub fn run(label: &str) -> (u64, u64, u64) {
    CPU_HOG_ITERS.store(0, Ordering::Relaxed);
    CPU_HOG_DONE_TICK.store(0, Ordering::Relaxed);
    KBD_IDX.store(0, Ordering::Relaxed);
    KBD_TRIGGER_TS.store(0, Ordering::Relaxed);
    for s in KBD_LATS.iter() { s.store(0, Ordering::Relaxed); }

    let hog_pid = crate::process::scheduler::alloc_pid();
    crate::process::scheduler::spawn(
        crate::process::Process::new(hog_pid, "pe4_hog", cpu_hog_task)
    );
    let kbd_pid = crate::process::scheduler::alloc_pid();
    crate::process::scheduler::spawn(
        crate::process::Process::new(kbd_pid, "pe4_kbd", kbd_task)
    );

    crate::serial_println!("[pe4] {} 시작 (hog=pid{} kbd=pid{})", label, hog_pid, kbd_pid);

    let start_tick = crate::interrupts::handlers::TICK.load(Ordering::Relaxed);
    let switch_start = crate::process::scheduler::switch_count();

    let mut next_key_tick = start_tick + KEYPRESS_INTERVAL_TICKS;
    loop {
        let tick = crate::interrupts::handlers::TICK.load(Ordering::Relaxed);

        if tick >= next_key_tick && KBD_IDX.load(Ordering::Relaxed) < N_SAMPLES {
            KBD_TRIGGER_TS.store(rdtsc(), Ordering::SeqCst);
            crate::process::scheduler::keyboard_boost();
            next_key_tick = tick + KEYPRESS_INTERVAL_TICKS;
        }

        let hog_done = CPU_HOG_DONE_TICK.load(Ordering::Relaxed) != 0;
        let kbd_done = KBD_IDX.load(Ordering::Relaxed) >= N_SAMPLES;
        if hog_done && kbd_done { break; }

        // 타임아웃 (starvation 등으로 무한 대기 방지, ~20초분 틱)
        if tick - start_tick > 4000 { break; }

        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }

    let switches_36 = {
        let end_tick = crate::interrupts::handlers::TICK.load(Ordering::Relaxed);
        let total_switches = crate::process::scheduler::switch_count() - switch_start;
        let total_ticks = (end_tick - start_tick).max(1);
        (total_switches * 36) / total_ticks
    };

    let avg_lat = trimmed_avg(&KBD_LATS);
    let hog_done_tick = CPU_HOG_DONE_TICK.load(Ordering::Relaxed);
    let hog_ticks = if hog_done_tick != 0 { hog_done_tick - start_tick } else { 0 };

    crate::serial_println!(
        "[pe4] {} 완료: kbd_lat_avg={}cy(n={})  ctx_switch/36tick={}  hog완료={}{}",
        label, avg_lat, KBD_IDX.load(Ordering::Relaxed).min(N_SAMPLES),
        switches_36,
        if hog_done_tick != 0 { "" } else { "TIMEOUT " },
        hog_ticks,
    );

    crate::process::scheduler::kill_pid(hog_pid);
    crate::process::scheduler::kill_pid(kbd_pid);

    (avg_lat, switches_36, hog_ticks)
}

/// 상위/하위 이상치 제거 후 평균 (bench_a1.rs와 동일 패턴).
fn trimmed_avg(samples: &[AtomicU64; N_SAMPLES]) -> u64 {
    let mut buf = [0u64; N_SAMPLES];
    let mut n = 0;
    for s in samples.iter() {
        let v = s.load(Ordering::Relaxed);
        if v != 0 { buf[n] = v; n += 1; }
    }
    if n == 0 { return 0; }
    for i in 0..n {
        let mut min_j = i;
        for j in (i + 1)..n { if buf[j] < buf[min_j] { min_j = j; } }
        buf.swap(i, min_j);
    }
    let trim = (n / 8).max(1).min(n - 1).max(0);
    let valid = n - trim;
    if valid == 0 { return buf[0]; }
    let sum: u64 = buf[..valid].iter().sum();
    sum / valid as u64
}

/// A/B 비교 리포트.
pub fn report_ab(on: (u64, u64, u64), off: (u64, u64, u64)) {
    crate::serial_println!("[pe4] ══════════════════════════════════════════════");
    crate::serial_println!("[pe4]  PE-4: Policy Engine A/B 비교 결과");
    crate::serial_println!("[pe4] ──────────────────────────────────────────────");
    crate::serial_println!(
        "[pe4]  {:>18}  {:>12}  {:>12}",
        "지표", "PE=ON", "PE=OFF",
    );
    crate::serial_println!(
        "[pe4]  {:>18}  {:>10}cy  {:>10}cy",
        "키입력 레이턴시", on.0, off.0,
    );
    crate::serial_println!(
        "[pe4]  {:>18}  {:>12}  {:>12}",
        "ctx switch/36tick", on.1, off.1,
    );
    crate::serial_println!(
        "[pe4]  {:>18}  {:>10}tick  {:>10}tick",
        "hog 완료 시간", on.2, off.2,
    );
    crate::serial_println!("[pe4] ══════════════════════════════════════════════");
}
