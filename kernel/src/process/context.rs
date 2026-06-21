//! CPU 컨텍스트 저장/복원 (Context Save & Restore)
//!
//! ## 컨텍스트 스위치란?
//! CPU는 한 번에 하나의 명령어 흐름(thread/process)만 실행할 수 있음.
//! "컨텍스트 스위치"는 현재 실행 중인 흐름의 레지스터 상태를 저장하고,
//! 다른 흐름의 저장된 상태를 복원해서 그 흐름을 이어서 실행하는 것.
//!
//! ## x86_64 System V ABI의 Callee-Saved 레지스터
//!
//! x86_64 ABI는 레지스터를 두 종류로 나눔:
//!
//! Caller-saved (호출자가 저장): RAX, RCX, RDX, RSI, RDI, R8-R11
//!   → 함수 호출 후 값이 바뀔 수 있음
//!   → 함수 호출 전에 필요하면 호출자가 스택에 저장해야 함
//!
//! Callee-saved (피호출자가 저장): RBX, RBP, R12-R15
//!   → 함수가 수정하면 반드시 원래 값으로 복원해야 함
//!   → switch_context는 이것들만 저장하면 됨
//!
//! ## 왜 Callee-saved만 저장하면 되는가?
//! switch_context는 일반 C 함수 호출로 이루어짐.
//! 호출 시점에 컴파일러가 이미 caller-saved 레지스터를 처리했음.
//! 따라서 switch_context 안에서는 callee-saved만 챙기면 됨.
//!
//! ## RSP와 RIP
//! RSP(스택 포인터)는 스택을 전환하는 핵심 레지스터 → 명시적으로 저장.
//! RIP(명령어 포인터)는 `retq` 명령어가 스택에서 자동으로 복원 → 별도 저장 불필요.

/// CPU 레지스터 스냅샷 (컨텍스트 스위치 시 저장/복원할 상태)
///
/// `#[repr(C)]`: Rust가 필드 순서나 패딩을 마음대로 바꾸지 못하게 함.
/// 어셈블리 코드가 정확한 오프셋으로 각 필드에 접근해야 하므로 필수.
///
/// 필드 → 어셈블리 오프셋 (8바이트씩):
///   rbx: 0x00
///   rbp: 0x08
///   r12: 0x10
///   r13: 0x18
///   r14: 0x20
///   r15: 0x28
///   rsp: 0x30
#[repr(C)]
pub struct CpuContext {
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    /// 스택 포인터: 이 값이 복원될 때 실제 스택이 전환됨
    /// `retq`가 이 스택의 최상단에서 복귀 주소를 팝 → RIP 복원
    pub rsp: u64,
}

impl CpuContext {
    pub const fn zeroed() -> Self {
        Self {
            rbx: 0,
            rbp: 0, // 0 = "최상위 스택 프레임" (스택 언와인딩 종료 신호)
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rsp: 0, // 반드시 spawn() 시 올바른 값으로 초기화해야 함
        }
    }
}

// ==================== 어셈블리 컨텍스트 스위치 ====================

/// 현재 CPU 컨텍스트를 `from`에 저장하고 `to`의 컨텍스트로 전환
///
/// # 동작 원리 (어셈블리 추적)
///
/// 예: A가 switch_context(A.ctx, B.ctx)를 호출할 때:
///
/// 1. x86 CALL 명령어 실행 → 복귀 주소가 스택에 push됨
///    (이 복귀 주소 = switch_context 호출 다음 명령어 = A가 재개될 위치)
///
/// 2. switch_context 진입:
///    - RBX~R15, RSP를 A.ctx에 저장
///    - RSP 저장 시점: 복귀 주소가 이미 스택에 있음
///
/// 3. B.ctx에서 RBX~R15, RSP를 복원:
///    - RSP가 B의 스택으로 전환됨
///
/// 4. `retq` 실행:
///    - B의 스택 최상단에서 복귀 주소를 팝 → RIP에 로드
///    - B가 이전에 switch_context를 호출했다면, 그 직후로 복귀
///    - B가 새로 생성됐다면, 스택에 미리 넣어둔 entry_fn 주소로 점프
///
/// # Safety
/// - `from`과 `to`가 서로 다른 CpuContext를 가리켜야 함 (aliasing 금지)
/// - `to.rsp`가 유효한 스택을 가리켜야 함
/// - 단일 코어에서만 호출해야 함 (멀티코어 미지원)
pub unsafe extern "C" fn switch_context(
    from: *mut CpuContext,
    to: *const CpuContext,
) {
    // 실제 구현은 아래 global_asm!에 있음
    // 이 Rust 함수 선언은 타입 검사 및 링킹을 위한 래퍼
    core::arch::asm!(
        "call switch_context_asm",
        in("rdi") from,
        in("rsi") to,
        clobber_abi("C"),
    );
}

// 실제 컨텍스트 스위치 어셈블리 구현
//
// AT&T 문법: `movq src, dst` (Intel 문법과 반대)
// 예: `movq %rbx, 0x00(%rdi)` = rbx 값을 [rdi+0] 에 저장
//
// 레지스터 역할 (System V ABI 함수 인수):
// - %rdi = from (첫 번째 인수 = CpuContext 저장 위치)
// - %rsi = to   (두 번째 인수 = CpuContext 복원 위치)
// Intel 문법 (Rust global_asm! 기본값):
//   mov [rdi + offset], reg    (src → dst 순서가 AT&T와 반대)
//   mov reg, [rsi + offset]
core::arch::global_asm!(
    ".global switch_context_asm",
    ".align 16",        // 16바이트 정렬 (함수 진입점 성능 최적화)
    "switch_context_asm:",

    // ── 'from' 컨텍스트 저장 (rdi = from: *mut CpuContext) ───────────
    "mov [rdi + 0x00], rbx",   // [from + 0x00] = rbx
    "mov [rdi + 0x08], rbp",   // [from + 0x08] = rbp
    "mov [rdi + 0x10], r12",   // [from + 0x10] = r12
    "mov [rdi + 0x18], r13",   // [from + 0x18] = r13
    "mov [rdi + 0x20], r14",   // [from + 0x20] = r14
    "mov [rdi + 0x28], r15",   // [from + 0x28] = r15
    "mov [rdi + 0x30], rsp",   // [from + 0x30] = rsp (스택 포인터 저장)
    // 이 시점 rsp는 call switch_context_asm의 복귀 주소를 가리킴

    // ── 'to' 컨텍스트 복원 (rsi = to: *const CpuContext) ─────────────
    "mov rbx, [rsi + 0x00]",   // rbx = [to + 0x00]
    "mov rbp, [rsi + 0x08]",   // rbp = [to + 0x08]
    "mov r12, [rsi + 0x10]",   // r12 = [to + 0x10]
    "mov r13, [rsi + 0x18]",   // r13 = [to + 0x18]
    "mov r14, [rsi + 0x20]",   // r14 = [to + 0x20]
    "mov r15, [rsi + 0x28]",   // r15 = [to + 0x28]
    "mov rsp, [rsi + 0x30]",   // rsp = [to + 0x30] ← 스택 전환! 이 순간부터 새 스택

    // ── 새 프로세스로 점프 ────────────────────────────────────────────
    // 새 스택의 최상단에서 복귀 주소를 팝해서 RIP에 로드.
    // 기존 프로세스라면: 이전에 switch_context_asm을 호출한 직후로 복귀.
    // 새 프로세스라면: spawn() 시 스택에 미리 넣어둔 entry_fn 주소로 점프.
    "ret",
);
