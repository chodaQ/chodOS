//! Policy B-2: 전력 관리 보조 모듈
//!
//! ## 역할
//!
//! Policy Engine이 `adapt_and_report`에서 호출해 CPU 전력 상태를 수집.
//! 두 가지 신호를 제공:
//!
//! 1. **MSR 기반 주파수 활용률** (`APERF`/`MPERF` 비율)
//!    - APERF (MSR 0xE8): 실제 동작 주파수 누적 카운터
//!    - MPERF (MSR 0xE7): 최대 주파수 기준 누적 카운터
//!    - 비율 100% = 항상 최대 주파수, 낮을수록 절전/idle
//!    - QEMU/TCG 에뮬레이션에서는 두 값이 동일하거나 0으로 반환될 수 있음
//!
//! 2. **TICK 기반 유휴율** (`idle_pct_from_ticks`)
//!    - Policy Engine이 이미 추적 중인 `recent_ticks` 합산으로 계산
//!    - MSR과 독립적으로 동작 — QEMU에서도 신뢰 가능
//!
//! ## idle 전략 권고
//!
//! | 유휴율 | 권고 | 이유 |
//! |--------|------|------|
//! | > 70%  | MWAIT C2 | 깊은 절전, 복귀 지연 수십μs 허용 |
//! | 30~70% | HLT C1   | 중간 부하, 빠른 복귀 필요 |
//! | < 30%  | C0 (no idle) | 고부하, sleep 없이 즉시 실행 |

/// MSR 읽기 (`rdmsr`).
///
/// x86_64 ring0 전용. QEMU에서는 0을 반환하는 MSR가 있음.
/// 미지원 MSR 접근 시 #GP 발생 가능 — 안전한 MSR만 읽을 것.
#[inline]
pub fn read_msr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// APERF — 실제 유효 주파수 누적 카운터 (MSR 0xE8).
#[inline]
pub fn read_aperf() -> u64 { read_msr(0xE8) }

/// MPERF — 최대 주파수 기준 누적 카운터 (MSR 0xE7).
#[inline]
pub fn read_mperf() -> u64 { read_msr(0xE7) }

/// 두 창 사이의 APERF/MPERF 델타로 주파수 활용률(%) 계산.
///
/// 반환: 0~100 (%). MPERF 델타가 0이면 0 반환 (QEMU TCG 에뮬레이션).
pub fn freq_util_pct(prev_aperf: u64, prev_mperf: u64) -> u8 {
    let cur_aperf = read_aperf();
    let cur_mperf = read_mperf();
    let da = cur_aperf.saturating_sub(prev_aperf);
    let dm = cur_mperf.saturating_sub(prev_mperf);
    if dm == 0 { return 0; }
    ((da * 100) / dm).min(100) as u8
}

/// idle 전략 — 유휴율에 따라 권고 C-state 반환.
#[derive(Copy, Clone, PartialEq)]
pub enum IdleMode {
    /// C0: 고부하 → idle 없음
    Active,
    /// C1: HLT — 빠른 복귀 (~1μs), 낮은 절전
    Hlt,
    /// C2: MWAIT — 더 깊은 절전, 복귀 지연 수십μs
    Mwait,
}

impl IdleMode {
    pub fn name(self) -> &'static str {
        match self {
            IdleMode::Active => "C0(고부하)",
            IdleMode::Hlt    => "C1(HLT)",
            IdleMode::Mwait  => "C2(MWAIT)",
        }
    }
}

pub fn recommend_idle(idle_pct: u64) -> IdleMode {
    if idle_pct > 70 { IdleMode::Mwait }
    else if idle_pct >= 30 { IdleMode::Hlt }
    else { IdleMode::Active }
}
