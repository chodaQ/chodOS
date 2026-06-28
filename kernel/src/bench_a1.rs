//! BETA-X 검증 A-1: 페이로드 크기 스윕
//!
//! ## 목적
//!
//! 같은 yield_now() 구조에서, 페이로드가 클수록 fast channel이 유리해지는
//! 기준점(crossover)을 찾는다.
//! 64B → 256B → 1024B → 4096B 순으로 일반 IPC vs fast channel을 측정.
//!
//! ## 핵심 차이 (64B 초과)
//!
//! ```text
//! 일반 IPC:    data[..64] 만 복사 → Message.data (truncation, 데이터 손실)
//! fast channel: 전체 data 를 SharedBuffer에 write → recv()가 투명하게 읽음
//! ```
//!
//! send() 비용:
//!   - 일반 IPC:    항상 64B copy (크기 무관)
//!   - fast channel: N bytes copy to SharedBuffer (크기에 비례)
//!
//! 즉, 크기가 클수록 fast channel의 SharedBuffer write 비용이 증가한다.
//! 반면 일반 IPC는 항상 64B 고정이지만 데이터를 잃는다.
//!
//! ## 샘플 배열 레이아웃
//!
//! ```text
//! SWEEP_SIZES = [64, 256, 1024, 4096]  (N_SIZES = 4)
//! 크기별 N_PER 샘플 × Phase 2(A/B) = 총 N_SIZES × 2 × N_PER 개
//!
//! idx = size_i * 2 * N_PER  +  phase * N_PER  +  sample_i
//!        (크기 블록 시작)       (0=baseline, 1=fast)
//! ```

use core::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

use crate::process;

// ── 설정 ─────────────────────────────────────────────────────────────────────

pub const SWEEP_SIZES: [usize; 4] = [64, 256, 1024, 4096];
const N_SIZES: usize = 4;
pub const N_PER: usize = 32; // 크기별 샘플 수 (부팅 시간 ↔ 통계 정확도 균형)

const TOTAL: usize = N_SIZES * 2 * N_PER; // 4 × 2 × 32 = 256

// ── 공유 상태 ─────────────────────────────────────────────────────────────────

pub static A1_RECV_PID: AtomicU64  = AtomicU64::new(0);
/// sender가 rdtsc를 기록; receiver가 recv 후 0으로 교체 (bench_ipc.rs와 동일 패턴)
static A1_SEND_TS:      AtomicU64  = AtomicU64::new(0);
pub static A1_IDX:      AtomicUsize = AtomicUsize::new(0);
static A1_LATS:         [AtomicU64; TOTAL] = [const { AtomicU64::new(0) }; TOTAL];
/// 0=진행중  1=완료
pub static A1_DONE:     AtomicU8   = AtomicU8::new(0);

// ── rdtsc ─────────────────────────────────────────────────────────────────────

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
    ((hi as u64) << 32) | lo as u64
}

// ── 수신 태스크 ───────────────────────────────────────────────────────────────

pub fn a1_receiver_task() -> ! {
    A1_RECV_PID.store(process::scheduler::current_pid(), Ordering::Relaxed);
    loop {
        while let Some(_) = process::ipc::recv() {
            let t1 = rdtsc();
            let t0 = A1_SEND_TS.swap(0, Ordering::SeqCst);
            if t0 != 0 {
                let lat = t1.saturating_sub(t0);
                let idx = A1_IDX.fetch_add(1, Ordering::SeqCst);
                if idx < TOTAL {
                    A1_LATS[idx].store(lat, Ordering::Relaxed);
                }
            }
        }
        process::scheduler::yield_now();
    }
}

// ── 송신 태스크 ───────────────────────────────────────────────────────────────

pub fn a1_sender_task() -> ! {
    let recv = loop {
        let p = A1_RECV_PID.load(Ordering::Relaxed);
        if p != 0 { break p; }
        process::scheduler::yield_now();
    };
    let my_pid = process::scheduler::current_pid();

    // 최대 페이로드 크기 (4096B) 스택 버퍼 — 실제 전송은 size 바이트만 사용
    let payload = [0xAAu8; 4096];

    for (si, &size) in SWEEP_SIZES.iter().enumerate() {
        let base = si * 2 * N_PER;
        let data = &payload[..size];

        // ── Phase A: 일반 IPC (64B 초과는 truncation 발생) ───────────────────
        let target_a = base + N_PER;
        for _ in 0..N_PER {
            while A1_SEND_TS.load(Ordering::Relaxed) != 0 {
                process::scheduler::yield_now();
            }
            A1_SEND_TS.store(rdtsc(), Ordering::SeqCst);
            // fast channel 없는 상태에서 send → 일반 경로 보장
            process::ipc::send(recv, data);
            process::scheduler::yield_now();
        }
        // Phase A 샘플 모두 수집 대기
        while A1_IDX.load(Ordering::Relaxed) < target_a {
            process::scheduler::yield_now();
        }

        // ── fast channel 생성 (size 맞춤 capacity) ──────────────────────────
        // capacity = max(size, 512) — 512B 이하는 기본 예약 크기와 동일
        let cap_bytes = size.max(512);
        let cap = process::ipc_fast::ensure_channel_cap(my_pid, recv, cap_bytes);
        crate::serial_println!(
            "[a1] size={:4}B fast channel cap={} (buf={}B) 생성 완료",
            size, cap, cap_bytes,
        );

        // ── Phase B: fast channel ────────────────────────────────────────────
        let target_b = base + 2 * N_PER;
        for _ in 0..N_PER {
            while A1_SEND_TS.load(Ordering::Relaxed) != 0 {
                process::scheduler::yield_now();
            }
            A1_SEND_TS.store(rdtsc(), Ordering::SeqCst);
            process::ipc::send(recv, data);
            process::scheduler::yield_now();
        }
        while A1_IDX.load(Ordering::Relaxed) < target_b {
            process::scheduler::yield_now();
        }

        // ── fast channel 제거 — 다음 크기에서 새로 생성 ─────────────────────
        process::ipc_fast::drop_channel(my_pid, recv);
        crate::serial_println!("[a1] size={:4}B 측정 완료", size);
    }

    A1_DONE.store(1, Ordering::SeqCst);
    loop { process::scheduler::yield_now(); }
}

// ── 결과 리포트 ───────────────────────────────────────────────────────────────

pub fn report() {
    crate::serial_println!(
        "[a1] ══════════════════════════════════════════════════════════"
    );
    crate::serial_println!(
        "[a1]   BETA-X A-1: 페이로드 크기별 IPC 레이턴시 비교 (n={})", N_PER,
    );
    crate::serial_println!(
        "[a1]   일반 IPC: 항상 64B 복사(초과 truncation) │ fast: N바이트 SharedBuffer write"
    );
    crate::serial_println!(
        "[a1]   yield_now() 구조 동일 — 순수 데이터 경로 차이만 비교"
    );
    crate::serial_println!(
        "[a1] ──────────────────────────────────────────────────────────"
    );
    crate::serial_println!(
        "[a1]  {:>6}  {:>10}  {:>10}  {:>5}  {:>8}  {:>8}",
        "size", "일반IPC(cy)", "fast(cy)", "ratio", "일반전달", "fast전달",
    );
    crate::serial_println!(
        "[a1] ──────────────────────────────────────────────────────────"
    );

    let mut first_win: Option<usize> = None;
    let mut last_base_for_eff = 0usize; // 1024B 기준 효율 계산용

    for (si, &size) in SWEEP_SIZES.iter().enumerate() {
        let base = si * 2 * N_PER;

        let avg_a = trimmed_avg(&A1_LATS[base..base + N_PER]);
        let avg_b = trimmed_avg(&A1_LATS[base + N_PER..base + 2 * N_PER]);

        let ratio_x10 = if avg_b > 0 { avg_a * 10 / avg_b } else { 10 };
        let delivered_a = size.min(64);

        let win = ratio_x10 >= 10;
        if win && first_win.is_none() { first_win = Some(size); }
        let mark = if win { " ✓" } else { "  " };

        crate::serial_println!(
            "[a1]  {:>5}B  {:>10}  {:>10}  {}.{}×{}  {:>6}B  {:>6}B",
            size, avg_a, avg_b,
            ratio_x10 / 10, ratio_x10 % 10, mark,
            delivered_a, size,
        );

        if size == 1024 { last_base_for_eff = base; }
    }

    crate::serial_println!(
        "[a1] ──────────────────────────────────────────────────────────"
    );
    match first_win {
        Some(sz) => crate::serial_println!(
            "[a1]  ✓ crossover: {}B 이상에서 fast channel이 일반 IPC보다 빠름", sz,
        ),
        None => crate::serial_println!(
            "[a1]  전 구간에서 fast channel이 일반 IPC보다 느림 (yield 오버헤드 지배)"
        ),
    }
    crate::serial_println!(
        "[a1]  ※ 일반 IPC는 64B 초과 데이터 유실 → 정확성은 fast가 항상 우위"
    );
    // 1024B 기준 처리량(bytes/cycle)
    if last_base_for_eff > 0 {
        let a1k = trimmed_avg(&A1_LATS[last_base_for_eff..last_base_for_eff + N_PER]).max(1);
        let b1k = trimmed_avg(&A1_LATS[last_base_for_eff + N_PER..last_base_for_eff + 2 * N_PER]).max(1);
        crate::serial_println!(
            "[a1]  처리량(1024B 기준): 일반={} B/cy  fast={} B/cy",
            64 / a1k, 1024 / b1k,
        );
    }
    crate::serial_println!(
        "[a1] ══════════════════════════════════════════════════════════"
    );
}

/// 상위 12.5%(4/32개) 이상치 제거 후 평균 — rdtsc 스케줄러 스파이크 완화.
fn trimmed_avg(samples: &[AtomicU64]) -> u64 {
    let n = samples.len();
    if n == 0 { return 0; }

    // 간단한 선택 정렬로 상위 4개 제거 (no_std, 힙 없음)
    let mut buf = [0u64; 32]; // N_PER 최대 32
    let take = n.min(32);
    for i in 0..take {
        buf[i] = samples[i].load(Ordering::Relaxed);
    }
    // 오름차순 선택 정렬
    for i in 0..take {
        let mut min_j = i;
        for j in (i + 1)..take {
            if buf[j] < buf[min_j] { min_j = j; }
        }
        buf.swap(i, min_j);
    }
    // 상위 12.5% 제거 (take / 8개, 최소 1개)
    let trim = (take / 8).max(1);
    let valid = take - trim;
    if valid == 0 { return buf[0]; }
    let sum: u64 = buf[..valid].iter().sum();
    sum / valid as u64
}
