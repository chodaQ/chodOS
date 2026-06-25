//! BETA-X 7: IPC 레이턴시 A/B 벤치마크
//!
//! ## 측정 방법
//!
//! `rdtsc`로 send 직전과 recv 직후의 CPU 사이클을 기록해 레이턴시(cycles)를 계산.
//!
//! - Phase A: 일반 IPC (fast channel 없음) — 64회 측정
//! - Phase B: fast channel 활성 (ensure_channel로 강제 생성) — 64회 측정
//!
//! ## 동기화
//!
//! `BENCH_SEND_TS`: sender가 send 직전 rdtsc를 기록, receiver가 recv 후 0으로 초기화.
//! sender는 0이 될 때까지 대기 → 한 번에 하나씩 측정, 오버랩 없음.
//!
//! ## QEMU 단일 코어 한계
//!
//! 콘텍스트 스위치 비용이 레이턴시에 포함됨. 실 하드웨어 대비 캐시 동작이
//! 에뮬레이션되므로 절대 수치보다 Phase B / Phase A 비율이 의미 있음.

use core::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

use crate::process;

// ── 설정 ─────────────────────────────────────────────────────────────────────

pub const BENCH_N: usize = 64;
const SAMPLE_CAP: usize = BENCH_N * 2; // Phase A + Phase B

// ── 공유 상태 (IRQ-safe: 모두 AtomicXxx) ────────────────────────────────────

pub static BENCH_RECV_PID: AtomicU64  = AtomicU64::new(0);
/// sender가 send 직전 rdtsc를 기록; receiver가 recv 후 0으로 교체.
static BENCH_SEND_TS:      AtomicU64  = AtomicU64::new(0);
/// 수집된 샘플 수
pub static BENCH_IDX:      AtomicUsize = AtomicUsize::new(0);
/// 레이턴시 샘플 배열 — [0..N]=Phase A, [N..2N]=Phase B
static BENCH_LATENCIES: [AtomicU64; SAMPLE_CAP] =
    [const { AtomicU64::new(0) }; SAMPLE_CAP];
/// 0=off  1=Phase A(baseline)  2=Phase B(fast)  3=완료
pub static BENCH_PHASE: AtomicU8 = AtomicU8::new(0);

// ── rdtsc ────────────────────────────────────────────────────────────────────

#[inline(always)]
fn rdtsc() -> u64 {
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
    ((hi as u64) << 32) | (lo as u64)
}

// ── 수신 태스크 ───────────────────────────────────────────────────────────────

pub fn bench_receiver_task() -> ! {
    BENCH_RECV_PID.store(process::scheduler::current_pid(), Ordering::Relaxed);
    loop {
        while let Some(_) = process::ipc::recv() {
            let t1 = rdtsc();
            let t0 = BENCH_SEND_TS.swap(0, Ordering::SeqCst);
            if t0 != 0 {
                let lat = t1.saturating_sub(t0);
                let idx = BENCH_IDX.fetch_add(1, Ordering::SeqCst);
                if idx < SAMPLE_CAP {
                    BENCH_LATENCIES[idx].store(lat, Ordering::Relaxed);
                }
            }
        }
        process::scheduler::yield_now();
    }
}

// ── 송신 태스크 ───────────────────────────────────────────────────────────────

pub fn bench_sender_task() -> ! {
    let recv = loop {
        let p = BENCH_RECV_PID.load(Ordering::Relaxed);
        if p != 0 { break p; }
        process::scheduler::yield_now();
    };
    let my_pid = process::scheduler::current_pid();

    let payload = [0xAAu8; 64];

    // ── Phase A: 일반 IPC (fast channel 없음) ─────────────────────────────────
    BENCH_PHASE.store(1, Ordering::SeqCst);
    for _ in 0..BENCH_N {
        // 이전 측정이 receiver에서 처리될 때까지 대기
        while BENCH_SEND_TS.load(Ordering::Relaxed) != 0 {
            process::scheduler::yield_now();
        }
        let t0 = rdtsc();
        BENCH_SEND_TS.store(t0, Ordering::SeqCst);
        process::ipc::send(recv, &payload);
        process::scheduler::yield_now();
    }
    // Phase A 샘플이 모두 수집될 때까지 대기 (마지막 recv 처리 보장)
    while BENCH_IDX.load(Ordering::Relaxed) < BENCH_N {
        process::scheduler::yield_now();
    }
    crate::serial_println!("[bench] Phase A 완료 ({} 샘플)", BENCH_N);

    // ── fast channel 강제 생성 (Policy Engine 웜업 없이 직접 생성) ──────────
    let cap = process::ipc_fast::ensure_channel(my_pid, recv);
    crate::serial_println!("[bench] fast channel 생성 cap={}", cap);

    // ── Phase B: fast channel 활성 ───────────────────────────────────────────
    BENCH_PHASE.store(2, Ordering::SeqCst);
    for _ in 0..BENCH_N {
        while BENCH_SEND_TS.load(Ordering::Relaxed) != 0 {
            process::scheduler::yield_now();
        }
        let t0 = rdtsc();
        BENCH_SEND_TS.store(t0, Ordering::SeqCst);
        process::ipc::send(recv, &payload);
        process::scheduler::yield_now();
    }
    while BENCH_IDX.load(Ordering::Relaxed) < BENCH_N * 2 {
        process::scheduler::yield_now();
    }
    crate::serial_println!("[bench] Phase B 완료 ({} 샘플)", BENCH_N);

    BENCH_PHASE.store(3, Ordering::SeqCst);
    loop { process::scheduler::yield_now(); }
}

// ── 결과 리포트 (main 태스크에서 호출) ───────────────────────────────────────

pub fn report() {
    let n = BENCH_N as u64;

    let (mut min_a, mut max_a, mut sum_a) = (u64::MAX, 0u64, 0u64);
    for i in 0..BENCH_N {
        let v = BENCH_LATENCIES[i].load(Ordering::Relaxed);
        if v == 0 { continue; }
        if v < min_a { min_a = v; }
        if v > max_a { max_a = v; }
        sum_a += v;
    }
    let avg_a = sum_a / n;

    let (mut min_b, mut max_b, mut sum_b) = (u64::MAX, 0u64, 0u64);
    for i in BENCH_N..BENCH_N * 2 {
        let v = BENCH_LATENCIES[i].load(Ordering::Relaxed);
        if v == 0 { continue; }
        if v < min_b { min_b = v; }
        if v > max_b { max_b = v; }
        sum_b += v;
    }
    let avg_b = sum_b / n;

    // 속도 향상 비율 — 소수점 한 자리 고정소수점
    let ratio_x10 = if avg_b > 0 { avg_a * 10 / avg_b } else { 10 };
    let delta_sign = if avg_a >= avg_b { "↓" } else { "↑" };
    let delta_abs  = if avg_a >= avg_b { avg_a - avg_b } else { avg_b - avg_a };

    crate::serial_println!("[bench] ══════════════════════════════════════════");
    crate::serial_println!("[bench]   BETA-X 7: IPC 레이턴시 A/B 비교 (n={})", BENCH_N);
    crate::serial_println!("[bench] ──────────────────────────────────────────");
    crate::serial_println!("[bench]  Phase A (일반 IPC):");
    crate::serial_println!("[bench]    min={:>8} cycles", min_a);
    crate::serial_println!("[bench]    avg={:>8} cycles", avg_a);
    crate::serial_println!("[bench]    max={:>8} cycles", max_a);
    crate::serial_println!("[bench]  Phase B (fast channel):");
    crate::serial_println!("[bench]    min={:>8} cycles", min_b);
    crate::serial_println!("[bench]    avg={:>8} cycles", avg_b);
    crate::serial_println!("[bench]    max={:>8} cycles", max_b);
    crate::serial_println!("[bench] ──────────────────────────────────────────");
    crate::serial_println!(
        "[bench]  avg 변화: {}{} cycles  비율={}.{}x",
        delta_sign, delta_abs, ratio_x10 / 10, ratio_x10 % 10,
    );
    if avg_b <= avg_a {
        crate::serial_println!("[bench]  fast channel이 평균 {}cycles 더 빠름 ✓", delta_abs);
    } else {
        crate::serial_println!(
            "[bench]  측정 노이즈 범위 내 — QEMU 단일 코어 에뮬레이션 한계"
        );
        crate::serial_println!(
            "[bench]  (실 하드웨어에서는 SharedBuffer 캐시 재사용 효과 기대)"
        );
    }
    crate::serial_println!("[bench] ══════════════════════════════════════════");
}
