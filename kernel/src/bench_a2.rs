//! BETA-X 검증 A-2: QEMU -smp 4 멀티코어 레이턴시 비교
//!
//! ## 목적
//!
//! BETA-X 5 core affinity의 효과를 측정한다.
//! 핵심 가설: "자주 통신하는 쌍이 같은 코어에서 실행될 때 캐시 지역성 이득이 생긴다."
//!
//! ## 측정 방식
//!
//! ```text
//! Phase A (크로스코어):
//!   AP1(sender) ─rdtsc→ A2_PING ─SeqCst→ BSP(receiver) ─rdtsc→ latency
//!   → 서로 다른 코어 → cache coherence(MESI) 오버헤드 포함
//!
//! Phase B (같은코어):
//!   BSP sender(yield) ─rdtsc→ A2_PING → BSP receiver(yield) ─rdtsc→ latency
//!   → 같은 코어에서 scheduler yield로 교대 실행 → L1/L2 캐시 hot 기대
//! ```
//!
//! ## QEMU 한계
//!
//! TCG 에뮬레이션에서는 캐시를 시뮬레이션하지 않으므로 A/B 차이가 작거나 없을 수 있음.
//! "구조적으로는 cross-core atomic vs same-core atomic의 차이" 자체는 측정 가능.
//! 실 하드웨어(KVM 또는 베어메탈)에서 재측정 시 의미있는 캐시 효과가 나타날 것.

use core::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

// ── 설정 ─────────────────────────────────────────────────────────────────────

pub const N: usize = 64; // Phase별 샘플 수

// ── 공유 상태 ─────────────────────────────────────────────────────────────────

/// Phase A/B 공통 ping: sender가 rdtsc 기록 (0=idle, >0=timestamp)
pub static A2_PING: AtomicU64 = AtomicU64::new(0);
/// Phase A/B 공통 pong: receiver가 준비 신호 (0=처리중, 1=다음 ready)
pub static A2_PONG: AtomicU64 = AtomicU64::new(0);

/// 측정 결과: [0..N] = Phase A, [N..2N] = Phase B
static A2_LATS: [AtomicU64; N * 2] = [const { AtomicU64::new(0) }; N * 2];

/// Phase A 완료 인덱스 카운터 (receiver 쪽에서 기록)
pub static A2_IDX_A: AtomicUsize = AtomicUsize::new(0);
/// Phase B 완료 인덱스 카운터
pub static A2_IDX_B: AtomicUsize = AtomicUsize::new(0);

/// 0=미시작  1=Phase A 진행중  2=Phase A 완료  3=Phase B 완료
pub static A2_DONE: AtomicU8 = AtomicU8::new(0);

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

// ── Phase A: AP1 sender ───────────────────────────────────────────────────────

/// AP1에서 실행: BSP(receiver)로 N번 ping을 보낸 뒤 idle spin.
///
/// 반환하지 않음 (-> !). `smp::assign_ap_work(0, ap_sender_phase_a)` 로 호출.
pub unsafe fn ap_sender_phase_a() -> ! {
    // serial_println 호출 금지: AP에서 QEMU UART 접근 시 BQL 경합 → BSP hang.
    // A2_DONE=1이 될 때까지(BSP가 Phase A 초기화 완료) 대기.
    while A2_DONE.load(Ordering::Acquire) < 1 {
        core::arch::asm!("pause", options(nomem, nostack, preserves_flags));
    }

    for _ in 0..N {
        // receiver가 ready 신호(pong=1)를 올릴 때까지 대기
        while A2_PONG.load(Ordering::Acquire) == 0 {
            core::arch::asm!("pause", options(nomem, nostack, preserves_flags));
        }
        A2_PONG.store(0, Ordering::Relaxed);
        let t0 = rdtsc();
        A2_PING.store(t0, Ordering::Release);
    }

    // Phase A 완료 — BSP가 A2_DONE=2를 설정할 것임. 이후 pause 루프.
    loop {
        core::arch::asm!("pause", options(nomem, nostack, preserves_flags));
    }
}

// ── Phase A: BSP receiver (인라인, 스케줄러 프로세스 불필요) ─────────────────

/// BSP에서 직접 호출: Phase A 수신 루프 (N번 pong→ping→latency).
///
/// AP1 sender와 lock-free AtomicU64 ping-pong으로 레이턴시 측정.
pub fn bsp_receiver_phase_a() {
    crate::serial_println!("[a2] BSP receiver Phase A 시작");

    for i in 0..N {
        // ready 신호 발송
        A2_PONG.store(1, Ordering::Release);
        // AP1 ping 도착 대기
        loop {
            let t0 = A2_PING.load(Ordering::Acquire);
            if t0 != 0 {
                let t1 = rdtsc();
                A2_PING.store(0, Ordering::Relaxed);
                A2_LATS[i].store(t1.saturating_sub(t0), Ordering::Relaxed);
                A2_IDX_A.fetch_add(1, Ordering::Relaxed);
                break;
            }
            // pause spin: QEMU MTTCG에서 BSP와 AP #0이 별도 스레드로 동시 실행되므로
            // HLT 불필요. pause로 스핀하면 수 µs 내에 ping 감지 가능.
            unsafe { core::arch::asm!("pause", options(nomem, nostack, preserves_flags)); }
        }
    }

    A2_DONE.store(2, Ordering::SeqCst);
    crate::serial_println!("[a2] BSP receiver Phase A 완료");
}

// ── Phase B: BSP same-core sender 태스크 ─────────────────────────────────────

/// BSP 스케줄러 프로세스: Phase B sender.
///
/// 같은 코어에서 yield_now()를 사이에 두고 ping을 보낸다.
/// receiver 태스크와 BSP에서 번갈아 실행 → same-core cache 지역성 환경.
pub fn bsp_b_sender_task() -> ! {
    crate::serial_println!("[a2] BSP sender Phase B 시작");

    for _ in 0..N {
        // receiver ready 대기 (yield로 receiver에게 기회 줌)
        loop {
            if A2_PONG.load(Ordering::Acquire) != 0 {
                break;
            }
            crate::process::scheduler::yield_now();
        }
        A2_PONG.store(0, Ordering::Relaxed);
        let t0 = rdtsc();
        A2_PING.store(t0, Ordering::Release);
        crate::process::scheduler::yield_now();
    }

    crate::serial_println!("[a2] BSP sender Phase B 완료");
    loop { crate::process::scheduler::yield_now(); }
}

/// BSP 스케줄러 프로세스: Phase B receiver.
///
/// 같은 코어에서 yield_now()를 사이에 두고 pong 후 latency 기록.
pub fn bsp_b_receiver_task() -> ! {
    crate::serial_println!("[a2] BSP receiver Phase B 시작");

    for i in 0..N {
        A2_PONG.store(1, Ordering::Release); // ready 신호
        loop {
            let t0 = A2_PING.load(Ordering::Acquire);
            if t0 != 0 {
                let t1 = rdtsc();
                A2_PING.store(0, Ordering::Relaxed);
                A2_LATS[N + i].store(t1.saturating_sub(t0), Ordering::Relaxed);
                A2_IDX_B.fetch_add(1, Ordering::Relaxed);
                break;
            }
            crate::process::scheduler::yield_now();
        }
    }

    A2_DONE.store(3, Ordering::SeqCst);
    crate::serial_println!("[a2] BSP receiver Phase B 완료");
    loop { crate::process::scheduler::yield_now(); }
}

// ── 결과 리포트 ───────────────────────────────────────────────────────────────

pub fn report() {
    crate::serial_println!(
        "[a2] ══════════════════════════════════════════════════════════"
    );
    crate::serial_println!(
        "[a2]   BETA-X A-2: 멀티코어 IPC 레이턴시 비교 (n={})", N
    );
    crate::serial_println!(
        "[a2]   Phase A: AP1(sender) → BSP(receiver)   [cross-core]"
    );
    crate::serial_println!(
        "[a2]   Phase B: BSP(sender) → BSP(receiver)   [same-core, yield]"
    );
    crate::serial_println!(
        "[a2] ──────────────────────────────────────────────────────────"
    );

    let avg_a = trimmed_avg(&A2_LATS[..N]);
    let avg_b = trimmed_avg(&A2_LATS[N..]);

    crate::serial_println!("[a2]   Phase A avg: {:>8} cycles  (cross-core atomic)", avg_a);
    crate::serial_println!("[a2]   Phase B avg: {:>8} cycles  (same-core  yield)",  avg_b);

    let ratio_x10 = if avg_b > 0 { avg_a * 10 / avg_b } else { 10 };
    crate::serial_println!(
        "[a2]   비율 A/B = {}.{}×   (>1.0 = same-core가 더 빠름, <1.0 = cross-core가 더 빠름)",
        ratio_x10 / 10, ratio_x10 % 10,
    );

    crate::serial_println!(
        "[a2] ──────────────────────────────────────────────────────────"
    );
    if avg_a > avg_b {
        crate::serial_println!(
            "[a2]  ✓ same-core가 {}cy ({}.{}×) 더 빠름 — core affinity 이득 확인",
            avg_a.saturating_sub(avg_b),
            ratio_x10 / 10, ratio_x10 % 10,
        );
    } else if avg_b > avg_a {
        crate::serial_println!(
            "[a2]  △ cross-core가 오히려 {}cy 더 빠름",
            avg_b.saturating_sub(avg_a),
        );
        crate::serial_println!(
            "[a2]    원인: Phase B는 yield 오버헤드 포함 (스케줄러 2회 통과) vs"
        );
        crate::serial_println!(
            "[a2]           Phase A는 BSP가 spin-wait (스케줄러 없음)"
        );
    } else {
        crate::serial_println!("[a2]  = 두 Phase 동일 레이턴시");
    }
    crate::serial_println!(
        "[a2]  ※ QEMU TCG: 캐시 미시뮬레이션 — 실 하드웨어에서 재측정 필요"
    );
    crate::serial_println!(
        "[a2]  ※ Phase B에는 yield 2회 오버헤드 포함 (Phase A는 spin-wait)"
    );
    crate::serial_println!(
        "[a2] ══════════════════════════════════════════════════════════"
    );
}

/// 상위 12.5%(N/8개) 이상치 제거 후 평균 (A-1과 동일 방식).
fn trimmed_avg(samples: &[AtomicU64]) -> u64 {
    let n = samples.len().min(64);
    if n == 0 { return 0; }

    let mut buf = [0u64; 64];
    for i in 0..n {
        buf[i] = samples[i].load(Ordering::Relaxed);
    }
    for i in 0..n {
        let mut min_j = i;
        for j in (i + 1)..n {
            if buf[j] < buf[min_j] { min_j = j; }
        }
        buf.swap(i, min_j);
    }
    let trim = (n / 8).max(1);
    let valid = n - trim;
    if valid == 0 { return buf[0]; }
    let sum: u64 = buf[..valid].iter().sum();
    sum / valid as u64
}
