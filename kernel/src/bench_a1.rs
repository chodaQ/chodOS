//! BETA-X 검증 A-1: 페이로드 크기 스윕
//!
//! ## 목적
//!
//! BETA-X 7에서 fast channel이 0.9× 느렸던 원인이 "64B 페이로드가 너무 작아서"인지 검증.
//! 64B → 128B → 256B → 512B → 1024B 순으로 동일한 측정을 반복하고
//! 어느 크기에서 fast channel이 일반 IPC를 역전하는지 찾는다.
//!
//! ## 핵심 차이 (128B 이상)
//!
//! ```text
//! 일반 IPC:    data[..64] 만 복사 → Message.data (truncation, 데이터 손실)
//! fast channel: 전체 data 를 SharedBuffer에 write → recv()가 투명하게 읽음
//! ```
//!
//! 즉, 128B 이상에서는 레이턴시 비교뿐 아니라 **정확성(correctness)** 차이도 존재.
//!
//! ## 샘플 배열 레이아웃
//!
//! ```text
//! SWEEP_SIZES = [64, 128, 256, 512, 1024]  (N_SIZES = 5)
//! 크기별 N_PER 샘플 × Phase 2(A/B) = 총 N_SIZES × 2 × N_PER 개
//!
//! idx = size_i * 2 * N_PER  +  phase * N_PER  +  sample_i
//!        (크기 블록 시작)       (0=baseline, 1=fast)
//! ```

use core::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

use crate::process;

// ── 설정 ─────────────────────────────────────────────────────────────────────

pub const SWEEP_SIZES: [usize; 5] = [64, 128, 256, 512, 1024];
const N_SIZES: usize = 5;
pub const N_PER: usize = 32; // 크기별 샘플 수 (부팅 시간 ↔ 통계 정확도 균형)

const TOTAL: usize = N_SIZES * 2 * N_PER; // 5 × 2 × 32 = 320

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

    // 최대 페이로드 크기 (1024B) 스택 버퍼 — 실제 전송은 size 바이트만 사용
    let payload = [0xAAu8; 1024];

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
        "[a1]   BETA-X A-1: 페이로드 크기별 IPC 레이턴시 비교 (n={})",
        N_PER,
    );
    crate::serial_println!(
        "[a1]   일반 IPC: 64B 초과 truncation  │  fast channel: 전체 전달"
    );
    crate::serial_println!(
        "[a1] ──────────────────────────────────────────────────────────"
    );
    crate::serial_println!(
        "[a1]  {:>6}  {:>10}  {:>10}  {:>6}  {:>10}  {:>10}",
        "size", "avg_A(cy)", "avg_B(cy)", "ratio", "A전달", "B전달",
    );
    crate::serial_println!(
        "[a1] ──────────────────────────────────────────────────────────"
    );

    let mut first_win: Option<usize> = None;

    for (si, &size) in SWEEP_SIZES.iter().enumerate() {
        let base = si * 2 * N_PER;

        // 이상치 제거: 상위 4개(12.5%) 폐기 후 나머지 평균
        let avg_a = trimmed_avg(&A1_LATS[base..base + N_PER]);
        let avg_b = trimmed_avg(&A1_LATS[base + N_PER..base + 2 * N_PER]);

        let ratio_x10 = if avg_b > 0 { avg_a * 10 / avg_b } else { 10 };
        let delivered_a = size.min(64);
        let delivered_b = size;

        let win = ratio_x10 >= 10;
        if win && first_win.is_none() { first_win = Some(size); }
        let mark = if win { " ✓" } else { "  " };

        crate::serial_println!(
            "[a1]  {:>5}B  {:>10}  {:>10}  {}.{}×{}  {:>6}B  {:>6}B",
            size, avg_a, avg_b,
            ratio_x10 / 10, ratio_x10 % 10, mark,
            delivered_a, delivered_b,
        );
    }

    crate::serial_println!(
        "[a1] ──────────────────────────────────────────────────────────"
    );
    match first_win {
        Some(sz) => crate::serial_println!(
            "[a1]  ✓ 역전점: {}B 이상에서 fast channel이 일반 IPC보다 빠름",
            sz,
        ),
        None => crate::serial_println!(
            "[a1]  전 구간에서 fast channel이 일반 IPC보다 느림"
        ),
    }
    crate::serial_println!(
        "[a1]  ※ 128B 이상: 일반 IPC는 64B만 전달 → fast는 정확성에서도 우위"
    );
    crate::serial_println!(
        "[a1]  효율(bytes/cycle): A={} vs B={} (512B 기준)",
        if { let a = trimmed_avg(&A1_LATS[3*2*N_PER..3*2*N_PER+N_PER]); if a>0{512/a}else{0} } > 0
            { 512 / trimmed_avg(&A1_LATS[3*2*N_PER..3*2*N_PER+N_PER]).max(1) } else { 0 },
        if { let b = trimmed_avg(&A1_LATS[3*2*N_PER+N_PER..3*2*N_PER+2*N_PER]); if b>0{512/b}else{0} } > 0
            { 512 / trimmed_avg(&A1_LATS[3*2*N_PER+N_PER..3*2*N_PER+2*N_PER]).max(1) } else { 0 },
    );
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
