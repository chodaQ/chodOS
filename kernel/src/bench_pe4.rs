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

/// 수집할 키 입력 레이턴시 표본 수.
///
/// run()은 이 개수를 다 채워야 정상 종료한다. PE=ON 조건에서는 컨텍스트
/// 스위치가 폭증해 표본이 잘 안 쌓이고(실측 n=20/40) 아래 타임아웃까지
/// 끌려가므로, 이 값이 실질적으로 벤치 1회의 소요 시간을 좌우한다.
#[cfg(feature = "quick-demo")]
pub const N_SAMPLES: usize = 10;
#[cfg(not(feature = "quick-demo"))]
pub const N_SAMPLES: usize = 40;

/// cpu_hog가 채워야 하는 반복 횟수.
///
/// 주의: 이 값은 부팅 시간에 거의 영향을 주지 않는다. 실측하면 hog는
/// 4틱(약 0.2초) 만에 목표를 채우고(로그의 `hog완료=4tick`), 그 뒤로는
/// 아래 N_SAMPLES 수집과 타임아웃이 시간을 지배한다. 데모 시간을 줄이려고
/// 이 값을 건드려도 소용없다 — 실제로 1/10로 줄여봤지만 변화가 없었다.
const HOG_TARGET_ITERS: u64 = 20_000_000;

/// 키 입력 시뮬레이션 간격 (틱).
const KEYPRESS_INTERVAL_TICKS: u64 = 3;

/// 벤치 1회의 최대 대기 틱 (starvation 등으로 무한 대기 방지).
///
/// 타이머는 약 18틱/초이므로 4000틱은 20초가 아니라 **약 222초**다.
/// PE=ON 실행은 표본을 다 못 채워 매번 이 한도까지 가고, 데모가 이
/// 벤치를 10회 호출하므로 전체 부팅 41분의 대부분이 여기서 나왔다.
#[cfg(feature = "quick-demo")]
const BENCH_TIMEOUT_TICKS: u64 = 300;
#[cfg(not(feature = "quick-demo"))]
const BENCH_TIMEOUT_TICKS: u64 = 4000;

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
            // 실험 33(CFS-1)에서 발견한 버그 수정: keyboard_boost()는 호출
            // 시점의 self.current를 부스트하는데, 여기서는 kernel_main이
            // 직접 호출하므로 kernel_main 자신이 부스트되고 있었다(실제
            // IRQ1 핸들러라면 인터럽트당한 kbd_task가 self.current라 맞았을
            // 것). WeightedPriority에서는 라운드로빈에 묻혀 안 드러났지만
            // CFS는 weight 격차가 커서 kernel_main이 무기한 스케줄을
            // 독점하는 형태로 즉시 드러남 — boost_pid(kbd_pid)로 명시.
            crate::process::scheduler::boost_pid(kbd_pid, 8);
            next_key_tick = tick + KEYPRESS_INTERVAL_TICKS;
        }

        let hog_done = CPU_HOG_DONE_TICK.load(Ordering::Relaxed) != 0;
        let kbd_done = KBD_IDX.load(Ordering::Relaxed) >= N_SAMPLES;
        if hog_done && kbd_done { break; }

        if tick - start_tick > BENCH_TIMEOUT_TICKS { break; }

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

    // 표본이 하나도 안 쌓이면 avg는 0이 되는데, 이걸 그냥 찍으면 "레이턴시
    // 0사이클"이라는 잘못된 인상을 준다. 표본 부족을 명시적으로 구분한다.
    let n_collected = KBD_IDX.load(Ordering::Relaxed).min(N_SAMPLES);
    if n_collected == 0 {
        crate::serial_println!(
            "[pe4] {} 완료: 표본 없음 (레이턴시 측정 불가)  ctx_switch/36tick={}  hog완료={}{}",
            label, switches_36,
            if hog_done_tick != 0 { "" } else { "TIMEOUT " },
            hog_ticks,
        );
    } else {
        crate::serial_println!(
            "[pe4] {} 완료: kbd_lat_avg={}cy(n={}{})  ctx_switch/36tick={}  hog완료={}{}",
            label, avg_lat, n_collected,
            if n_collected < N_SAMPLES { ", 표본부족" } else { "" },
            switches_36,
            if hog_done_tick != 0 { "" } else { "TIMEOUT " },
            hog_ticks,
        );
    }

    crate::process::scheduler::kill_pid(hog_pid);
    crate::process::scheduler::kill_pid(kbd_pid);
    // 실험 35: 이 함수가 반복 호출될 때(CFS-2 등) kernel_stack이 누적되며
    // OOM으로 이어졌던 문제 수정 — 일반 컨텍스트(이 함수는 인터럽트 밖에서
    // 호출됨)에서 바로 회수한다.
    crate::process::scheduler::reap_dead();

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

/// A/B 비교 리포트 (PE-4: on/off, CFS-1: WP/CFS 등 재사용).
pub fn report_ab_labeled(title: &str, label_a: &str, a: (u64, u64, u64), label_b: &str, b: (u64, u64, u64)) {
    crate::serial_println!("[pe4] ══════════════════════════════════════════════");
    crate::serial_println!("[pe4]  {}", title);
    crate::serial_println!("[pe4] ──────────────────────────────────────────────");
    crate::serial_println!(
        "[pe4]  {:>18}  {:>12}  {:>12}",
        "지표", label_a, label_b,
    );
    crate::serial_println!(
        "[pe4]  {:>18}  {:>10}cy  {:>10}cy",
        "키입력 레이턴시", a.0, b.0,
    );
    crate::serial_println!(
        "[pe4]  {:>18}  {:>12}  {:>12}",
        "ctx switch/36tick", a.1, b.1,
    );
    crate::serial_println!(
        "[pe4]  {:>18}  {:>10}tick  {:>10}tick",
        "hog 완료 시간", a.2, b.2,
    );
    crate::serial_println!("[pe4] ══════════════════════════════════════════════");
}

/// PE-4 전용 A/B 비교 리포트 (하위 호환 — 기존 호출부 유지).
pub fn report_ab(on: (u64, u64, u64), off: (u64, u64, u64)) {
    report_ab_labeled("PE-4: Policy Engine A/B 비교 결과", "PE=ON", on, "PE=OFF", off);
}
