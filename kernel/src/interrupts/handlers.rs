//! 인터럽트/예외 핸들러 + 어셈블리 스텁
//!
//! ## 인터럽트 진입 흐름
//!
//! CPU가 인터럽트를 감지하면:
//! 1. 현재 실행을 중단
//! 2. 스택에 복귀 정보 push: [RFLAGS, CS, RIP] (+ 에러 코드가 있는 예외는 error_code)
//! 3. IDT에서 핸들러 주소를 읽어 점프
//!
//! 우리 스텁(stub)이 하는 일:
//! 1. 에러 코드가 없는 예외: 더미 0 push → 벡터 번호 push
//! 2. 에러 코드가 있는 예외: 벡터 번호만 push (에러 코드는 CPU가 이미 push)
//! 3. IRQ: 모든 caller-saved 레지스터 저장 → Rust 핸들러 호출 → 복원
//!
//! ## 스택 레이아웃 (exception_common 진입 시점)
//!
//! ```
//! [낮은 주소]  ← RSP (exception_common 진입 시)
//!   vector
//!   error_code   (없으면 더미 0)
//!   RIP
//!   CS
//!   RFLAGS
//! [높은 주소]
//! ```

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use super::pic;

/// PIT 타이머 틱 카운터 (원자적 — 향후 멀티코어 대비)
pub static TICK: AtomicU64 = AtomicU64::new(0);

/// 예외 진입 시 스택 레이아웃 (어셈블리 스텁이 구성)
///
/// `exception_common`이 레지스터를 push한 뒤 RSP가 이 구조체를 가리킴.
/// `#[repr(C)]`로 필드 순서와 패딩을 보장.
#[repr(C)]
pub struct ExceptionFrame {
    // ── 스텁이 push한 레지스터 (역순: 마지막 push = 낮은 주소) ──────────
    pub r15: u64, pub r14: u64, pub r13: u64, pub r12: u64,
    pub r11: u64, pub r10: u64, pub r9:  u64, pub r8:  u64,
    pub rbp: u64, pub rdi: u64, pub rsi: u64, pub rdx: u64,
    pub rcx: u64, pub rbx: u64, pub rax: u64,
    // ── 스텁이 push한 메타정보 ───────────────────────────────────────────
    pub vector:     u64, // 인터럽트 벡터 번호
    pub error_code: u64, // CPU가 push한 에러 코드 (없으면 더미 0)
    // ── CPU가 자동으로 push한 복귀 정보 ─────────────────────────────────
    pub rip:    u64,
    pub cs:     u64,
    pub rflags: u64,
    // NOTE: ring3→ring0 전환 시에만 RSP/SS가 추가로 push됨
}

/// CPU 예외 핸들러 (어셈블리 exception_common에서 호출)
///
/// 현재 구현: 예외 정보를 시리얼에 출력 후 무한 HLT.
/// 복구 불가 예외는 항상 커널 패닉으로 처리.
#[no_mangle]
pub extern "C" fn exception_handler(frame: &ExceptionFrame) {
    let vec = frame.vector;
    let ec  = frame.error_code;

    // Page Fault(#PF, 벡터 14)일 때 CR2 = 폴트 발생 가상 주소
    let cr2: u64 = if vec == 14 {
        let v: u64;
        unsafe {
            core::arch::asm!("mov {}, cr2",
                out(reg) v,
                options(nomem, nostack, preserves_flags));
        }
        v
    } else { 0 };

    crate::serial_println!("\n!!! CPU EXCEPTION !!!");
    crate::serial_println!("  vec={} ({})", vec, exception_name(vec));
    crate::serial_println!("  error_code = {:#x}", ec);
    crate::serial_println!("  RIP    = {:#018x}", frame.rip);
    crate::serial_println!("  CS     = {:#x}", frame.cs);
    crate::serial_println!("  RFLAGS = {:#x}", frame.rflags);
    if vec == 14 {
        // 에러 코드 비트: P(존재), W(쓰기), U(유저), I(명령어 fetch)
        crate::serial_println!("  CR2    = {:#018x}  (폴트 주소)", cr2);
        crate::serial_println!("  PF flags: P={} W={} U={} I={}",
            ec & 1, (ec >> 1) & 1, (ec >> 2) & 1, (ec >> 4) & 1);
    }

    loop {
        unsafe {
            core::arch::asm!("cli; hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

/// 타이머 인터럽트 선점형 핸들러 (ALPHA M1)
///
/// `isr32` 어셈블리 스텁이 ALL 레지스터를 스택에 저장한 뒤 이 함수를 호출.
///
/// ## 인수
/// `rsp`: ISR 스텁이 ALL 레지스터를 push한 직후의 스택 포인터.
///        = 현재 프로세스의 선점형 컨텍스트 프레임 시작 주소.
///
/// ## 반환값
/// 다음에 실행할 프로세스의 `preempt_rsp`.
/// 스위치가 없으면 `rsp` 그대로 반환.
/// 스위치가 있으면 다른 프로세스의 저장된 RSP를 반환.
///
/// ## 동작
/// 1. TICK 증가 + PIC EOI
/// 2. TIME_SLICE 틱마다 `scheduler::preempt(rsp)` 호출
/// 3. 반환값은 `isr32` 스텁의 `mov rsp, rax`로 스택을 전환
#[no_mangle]
pub extern "C" fn timer_preempt(rsp: u64) -> u64 {
    let tick = TICK.fetch_add(1, Ordering::Relaxed) + 1;
    if tick % 18 == 0 {
        crate::serial_println!("[timer] tick={} (~{}s)", tick, tick / 18);
    }
    // PIC Master에 EOI 전송 — 이 인터럽트 처리 완료 신호
    // EOI 전에 다른 인터럽트를 막기 위해 핸들러 진입 시 IF=0 (INT_GATE)
    pic::eoi_master();

    // TIME_SLICE: Policy Engine이 실시간 조정. 기본 3틱(≈165ms).
    let slice = crate::policy::TIME_SLICE.load(Ordering::Relaxed).max(1);
    if tick % slice == 0 {
        crate::process::scheduler::preempt(rsp)
    } else {
        rsp
    }
}

/// 자발적 양보 핸들러 — `int 0x40` (벡터 0x40, ALPHA M1)
///
/// `yield_now()`가 `int 0x40`으로 구현됨.
/// `isr64` 스텁이 ALL 레지스터를 스택에 저장한 뒤 이 함수를 호출.
/// 타이머 선점과 완전히 동일한 ISR 프레임 형식 → 컨텍스트 전환 방식 통일.
///
/// 반환값: 다음 프로세스의 `preempt_rsp` (항상 즉시 스위치).
#[no_mangle]
pub extern "C" fn voluntary_yield(rsp: u64) -> u64 {
    crate::process::scheduler::voluntary_preempt(rsp)
}

/// 키보드 인터럽트 핸들러 (IRQ1, 벡터 0x21)
///
/// 키보드 컨트롤러(i8042)는 IRQ1 발생 시 포트 0x60에 스캔 코드를 올림.
/// 반드시 포트 0x60을 읽어야 인터럽트가 해제됨.
///
/// 스캔 코드 Set 1 (기본값):
/// - 0x01-0x58: 키 누름 (make code)
/// - 0x81-0xD8: 키 뗌   (break code = make | 0x80)
#[no_mangle]
pub extern "C" fn irq_handler_keyboard() {
    let scancode: u8;
    unsafe {
        core::arch::asm!("in al, 0x60",
            out("al") scancode,
            options(nomem, nostack, preserves_flags));
    }
    crate::kbd::handle_scancode(scancode);
    if scancode < 0x80 {
        crate::process::scheduler::keyboard_boost();
    }
    pic::eoi_master();
}

/// 마우스 IRQ12 핸들러 (벡터 0x2C)
///
/// 3바이트 패킷 수집은 mouse::handle_irq()에서 처리.
/// PIC2를 통하므로 EOI를 슬레이브(PIC2) + 마스터(PIC1) 순서로 전송.
#[no_mangle]
pub extern "C" fn irq_handler_mouse() {
    crate::mouse::handle_irq();
    pic::eoi_slave(); // PIC2 EOI → PIC1 EOI
}

// ── Syscall 핸들러 (int 0x80) ────────────────────────────────────────────────
//
// ring3 → ring0 전환 흐름:
//   1. ring3 코드 `int 0x80` 실행
//   2. CPU: TSS.RSP0으로 스택 전환, [SS, RSP, RFLAGS, CS, RIP] push
//   3. IDT[0x80] 핸들러(isr128 스텁) 진입
//   4. isr128: caller-saved 레지스터 저장 → syscall_handler() 호출
//   5. syscall_handler(): 처리 후 반환 → isr128: 레지스터 복원 → IRETQ ring3 복귀
//
// 3회 호출 후 longjmp:
//   paging::KERNEL_MAIN_RSP로 RSP 복원 + after_user_demo() 점프.
//   longjmp 이후 isr128의 나머지 코드는 실행되지 않음.

/// ring3 syscall 호출 카운터 (디버그용)
pub static USER_SYSCALL_COUNT: AtomicU32 = AtomicU32::new(0);

/// ring3 프로세스가 `sys_exit(code)`를 호출할 때 저장되는 종료 코드
pub static USER_EXIT_CODE: AtomicI32 = AtomicI32::new(0);

/// int 0x80 syscall 디스패처 (ALPHA 13)
///
/// `isr128` 스텁이 9개 레지스터를 push한 직후 RSP를 이 함수에 인수로 전달.
/// 반환값(i64)은 isr128이 저장된 RAX 슬롯([rsp+64])에 써 넣어 user-space로 전달.
///
/// ## 스택 레이아웃 (`frame_rsp` 기준)
///
/// ```text
/// frame[0] = r11  frame[1] = r10(arg4)  frame[2] = r9(arg6)
/// frame[3] = r8(arg5)   frame[4] = rdi(arg1)  frame[5] = rsi(arg2)
/// frame[6] = rdx(arg3)  frame[7] = rcx        frame[8] = rax(syscall#)
/// ```
#[no_mangle]
pub extern "C" fn syscall_dispatch(frame_rsp: u64) -> i64 {
    USER_SYSCALL_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::syscall::dispatch(frame_rsp as *const u64)
}

/// 실행 대기 중인 패키지 인덱스 (ALPHA 16)
///
/// `-1` = 없음 (mushell 재시작), `>= 0` = `pkg::PACKAGES[idx]` 실행.
/// `sys_execve`가 설정하고 `after_user_demo` Phase 2+가 소비.
pub static PENDING_PKG_IDX: AtomicI32 = AtomicI32::new(-1);

/// ring3 프로세스 종료 후 커널이 재개하는 지점 (sys_exit longjmp 목적지)
///
/// ## Phase 설계
///
/// - **Phase 0**: ALPHA 13 raw code 종료 → ALPHA 14 test ELF 실행
/// - **Phase 1**: ALPHA 14 ELF 종료 → ALPHA 15 mushell 첫 실행
/// - **Phase 2+**: mushell/패키지 종료 → PENDING_PKG_IDX 확인
///   - `>= 0`: 해당 패키지 ELF 실행 (ALPHA 16 mukg run)
///   - `-1`:   mushell 재시작 (패키지 종료 후 셸 복귀)
///
/// longjmp로 도달 — `extern "C"` + `#[no_mangle]` 필수.
#[no_mangle]
pub extern "C" fn after_user_demo() -> ! {
    static PHASE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
    let phase = PHASE.fetch_add(1, Ordering::Relaxed);
    let calls = USER_SYSCALL_COUNT.load(Ordering::Relaxed);
    let code  = USER_EXIT_CODE.load(Ordering::Relaxed);

    USER_SYSCALL_COUNT.store(0, Ordering::Relaxed);
    USER_EXIT_CODE.store(0, Ordering::Relaxed);

    match phase {
        0 => {
            // ALPHA 13 완료 → ALPHA 14 진입
            crate::serial_println!(
                "[ring3] exited: code={} ({} syscalls total)", code, calls
            );
            crate::serial_println!("--- ALPHA 13 complete ---\n");
            crate::serial_println!("===========================================");
            crate::serial_println!("  ALPHA 14: ELF Loader");
            crate::serial_println!("  (실제 ELF64 바이너리를 ring3에서 실행)");
            crate::serial_println!("===========================================");
            crate::serial_println!("[elf] loading test ELF ({} bytes)...",
                crate::ELF_TEST.len());
            unsafe { crate::paging::enter_elf(crate::ELF_TEST); }
        }
        1 => {
            // ALPHA 14 완료 → ALPHA 15 셸 첫 진입
            crate::serial_println!(
                "[ring3] ELF test exited: code={} ({} syscalls total)", code, calls
            );
            crate::serial_println!("--- ALPHA 14 complete ---\n");
            crate::serial_println!("===========================================");
            crate::serial_println!("  ALPHA 15 / 16: Shell + Package Manager");
            crate::serial_println!("  (mukg list/install/run 으로 패키지 실행)");
            crate::serial_println!("===========================================");
            crate::serial_println!("[shell] loading mushell ({} bytes)...",
                crate::MUSHELL_ELF.len());
            unsafe { crate::paging::enter_elf(crate::MUSHELL_ELF); }
        }
        _ => {
            // Phase 2+: mushell 또는 패키지 종료
            // Phase를 2로 고정 — 이 브랜치를 계속 사용
            PHASE.store(2, Ordering::Relaxed);

            let pkg_idx = PENDING_PKG_IDX.swap(-1, Ordering::Relaxed);
            if pkg_idx >= 0 {
                // sys_execve가 요청한 패키지 실행
                let pkg = &crate::pkg::PACKAGES[pkg_idx as usize];
                crate::serial_println!("[mukg] running {} v{} ({} bytes)...",
                    pkg.name, pkg.version, pkg.elf.len());
                unsafe { crate::paging::enter_elf(pkg.elf); }
            } else {
                // 패키지 종료 후 또는 직접 종료 → mushell 재시작
                unsafe { crate::paging::enter_elf(crate::MUSHELL_ELF); }
            }
        }
    }
}

fn exception_name(v: u64) -> &'static str {
    match v {
        0  => "#DE Divide Error",
        1  => "#DB Debug",
        2  => "NMI",
        3  => "#BP Breakpoint",
        4  => "#OF Overflow",
        5  => "#BR Bound Range",
        6  => "#UD Invalid Opcode",
        7  => "#NM Device Not Available",
        8  => "#DF Double Fault",
        9  => "Coprocessor Segment Overrun",
        10 => "#TS Invalid TSS",
        11 => "#NP Segment Not Present",
        12 => "#SS Stack-Segment Fault",
        13 => "#GP General Protection",
        14 => "#PF Page Fault",
        16 => "#MF x87 FPU Error",
        17 => "#AC Alignment Check",
        18 => "#MC Machine Check",
        19 => "#XF SIMD FPU Error",
        _  => "Unknown",
    }
}

// ══ 어셈블리 인터럽트 스텁 ═══════════════════════════════════════════════════
//
// ## 레지스터 저장 전략
//
// x86_64 System V ABI에서 "caller-saved" 레지스터:
//   RAX, RCX, RDX, RSI, RDI, R8, R9, R10, R11
// "callee-saved" 레지스터 (Rust 핸들러가 직접 보존):
//   RBX, RBP, R12, R13, R14, R15
//
// 예외 핸들러(exception_common)는 모든 레지스터를 저장 (디버깅 정보용).
// IRQ 핸들러는 caller-saved만 저장 (callee-saved는 Rust가 알아서 보존).
//
// ## 스택 정렬
//
// System V ABI: CALL 직전 RSP는 16바이트 정렬이어야 함.
// CPU가 인터럽트 시 3개(rip, cs, rflags = 24바이트) push → RSP는 8 misaligned.
// IRQ 스텁: 9개 caller-saved push(72바이트) → 합계 96바이트 → 16 정렬 ✓
// exception 스텁: 추가 2개 push(vector+ec = 16바이트) + 15개 push(120바이트)
//                 합계 160바이트 → 16 정렬 ✓ (에러코드 있는 예외도 동일)

core::arch::global_asm!(
    // ── exception_common: 모든 예외의 공통 진입점 ────────────────────────
    // 진입 시 스택: [vector, error_code, rip, cs, rflags]
    "exception_common:",
    "push rax", "push rbx", "push rcx", "push rdx",
    "push rsi", "push rdi", "push rbp",
    "push r8",  "push r9",  "push r10", "push r11",
    "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",              // 첫 번째 인수: &ExceptionFrame
    "call exception_handler",
    "pop r15", "pop r14", "pop r13", "pop r12",
    "pop r11", "pop r10", "pop r9",  "pop r8",
    "pop rbp",  "pop rdi", "pop rsi", "pop rdx",
    "pop rcx",  "pop rbx", "pop rax",
    "add rsp, 16",               // vector + error_code 제거
    "iretq",

    // ── isr32: 타이머 IRQ (벡터 0x20) — 선점형 스케줄러 (ALPHA M1) ────────
    //
    // ## 선점형 컨텍스트 스위치 원리
    //
    // 1. CPU가 타이머 IRQ 감지 → ring0 인터럽트이므로 스택에 push:
    //       [RFLAGS, CS, RIP]  (3개, 24바이트)
    //    이후 IDT의 핸들러(isr32)로 점프.
    //
    // 2. isr32 스텁: ALL 범용 레지스터를 스택에 push (15개, 120바이트).
    //    push 순서: rax, rcx, rdx, rsi, rdi, r8..r11, rbx, rbp, r12..r15
    //    이후 RSP = 프레임 시작 (r15 위치).
    //
    // 3. `mov rdi, rsp` → timer_preempt(rsp) 호출.
    //    반환값(rax) = 다음 프로세스의 preempt_rsp.
    //
    // 4. `mov rsp, rax` → 스택 전환 (다른 RSP면 다른 프로세스의 스택).
    //
    // 5. pop r15..rax → 새 프로세스의 레지스터 복원.
    //
    // 6. iretq → 새 프로세스의 [RIP, CS, RFLAGS] 팝 → 새 프로세스 실행.
    //
    // ## 스택 레이아웃 (isr32 진입 후 all-push 직후)
    //
    //   [rsp + 0]   = r15
    //   [rsp + 8]   = r14
    //   [rsp + 16]  = r13
    //   [rsp + 24]  = r12
    //   [rsp + 32]  = rbp
    //   [rsp + 40]  = rbx
    //   [rsp + 48]  = r11
    //   [rsp + 56]  = r10
    //   [rsp + 64]  = r9
    //   [rsp + 72]  = r8
    //   [rsp + 80]  = rdi
    //   [rsp + 88]  = rsi
    //   [rsp + 96]  = rdx
    //   [rsp + 104] = rcx
    //   [rsp + 112] = rax
    //   [rsp + 120] = RIP    ← CPU가 push (ring0 iretq 프레임)
    //   [rsp + 128] = CS
    //   [rsp + 136] = RFLAGS
    //
    // 이 레이아웃이 Process::new()의 초기 스택 프레임과 일치해야 함.
    ".global isr32",
    "isr32:",
    // caller-saved 레지스터 저장
    "push rax", "push rcx", "push rdx",
    "push rsi",  "push rdi",
    "push r8",  "push r9",  "push r10", "push r11",
    // callee-saved 레지스터도 저장 (선점 시 다른 프로세스가 복원하므로 필수)
    "push rbx", "push rbp",
    "push r12", "push r13", "push r14", "push r15",
    // 현재 RSP(프레임 시작)를 첫 번째 인수로 전달
    "mov rdi, rsp",
    // timer_preempt(rsp) -> new_rsp
    // 스위치가 없으면 rsp 그대로, 있으면 다른 프로세스의 preempt_rsp 반환
    "call timer_preempt",
    // RSP를 반환값으로 교체 — 실제 스택 전환 발생 지점
    "mov rsp, rax",
    // callee-saved 복원
    "pop r15", "pop r14", "pop r13", "pop r12",
    "pop rbp",  "pop rbx",
    // caller-saved 복원
    "pop r11", "pop r10", "pop r9",  "pop r8",
    "pop rdi",  "pop rsi",
    "pop rdx",  "pop rcx", "pop rax",
    // iretq: 스택에서 RIP, CS, RFLAGS 팝 → 해당 프로세스로 복귀
    "iretq",

    // ── isr64: 자발적 양보 (벡터 0x40, int 0x40) — ALPHA M1 ─────────────
    //
    // yield_now()이 `int 0x40`으로 구현됨.
    // isr32와 완전히 동일한 스텁 구조 → 컨텍스트 형식 통일.
    // ring0 소프트웨어 인터럽트이므로 CPU는 3개(RFLAGS, CS, RIP)만 push.
    ".global isr64",
    "isr64:",
    "push rax", "push rcx", "push rdx",
    "push rsi",  "push rdi",
    "push r8",  "push r9",  "push r10", "push r11",
    "push rbx", "push rbp",
    "push r12", "push r13", "push r14", "push r15",
    "mov rdi, rsp",
    "call voluntary_yield",
    // rax = new_rsp. 스택 전환.
    "mov rsp, rax",
    "pop r15", "pop r14", "pop r13", "pop r12",
    "pop rbp",  "pop rbx",
    "pop r11", "pop r10", "pop r9",  "pop r8",
    "pop rdi",  "pop rsi",
    "pop rdx",  "pop rcx", "pop rax",
    "iretq",

    // ── process_start: 신규 프로세스 진입 트램폴린 ───────────────────────
    //
    // iretq가 새 프로세스의 RIP로 직접 점프하는 대신 이 트램폴린을 거침.
    // frame[14] (rax) = 실제 entry_fn 주소
    // frame[5]  (rbx) = stack_top (Process::new()이 설정)
    // frame[15] (RIP) = process_start (이 트램폴린)
    //
    // iretq가 ring-change 팝을 하면 RSP가 0이 됨(frame 밖 메모리 = 0 읽힘).
    // rbx에 미리 저장해 둔 stack_top으로 RSP를 강제 복원하여 항상 올바른 스택 사용.
    ".global process_start",
    "process_start:",
    "mov rsp, rbx",   // RSP = stack_top (frame[5]=rbx에서 복원됨, ring-change 시에도 안전)
    "jmp rax",        // rax = entry_fn (frame[14] 에서 복원됨)

    // ── isr33: 키보드 IRQ (벡터 0x21) ────────────────────────────────────
    ".global isr33",
    "isr33:",
    "push rax", "push rcx", "push rdx",
    "push rsi",  "push rdi",
    "push r8",  "push r9",  "push r10", "push r11",
    "call irq_handler_keyboard",
    "pop r11", "pop r10", "pop r9",  "pop r8",
    "pop rdi",  "pop rsi",
    "pop rdx",  "pop rcx", "pop rax",
    "iretq",

    // ── isr44: 마우스 IRQ12 (벡터 0x2C) ─────────────────────────────────
    ".global isr44",
    "isr44:",
    "push rax", "push rcx", "push rdx",
    "push rsi",  "push rdi",
    "push r8",  "push r9",  "push r10", "push r11",
    "call irq_handler_mouse",
    "pop r11", "pop r10", "pop r9",  "pop r8",
    "pop rdi",  "pop rsi",
    "pop rdx",  "pop rcx", "pop rax",
    "iretq",

    // ── 예외 스텁: 에러 코드 없음 (더미 0 push 후 벡터 번호 push) ────────
    ".global isr0",  "isr0:",  "push 0", "push 0",  "jmp exception_common",
    ".global isr1",  "isr1:",  "push 0", "push 1",  "jmp exception_common",
    ".global isr2",  "isr2:",  "push 0", "push 2",  "jmp exception_common",
    ".global isr3",  "isr3:",  "push 0", "push 3",  "jmp exception_common",
    ".global isr4",  "isr4:",  "push 0", "push 4",  "jmp exception_common",
    ".global isr5",  "isr5:",  "push 0", "push 5",  "jmp exception_common",
    ".global isr6",  "isr6:",  "push 0", "push 6",  "jmp exception_common",
    ".global isr7",  "isr7:",  "push 0", "push 7",  "jmp exception_common",
    // 8 = #DF: CPU가 에러 코드(항상 0) push → 벡터만 push
    ".global isr8",  "isr8:",  "push 8",            "jmp exception_common",
    ".global isr9",  "isr9:",  "push 0", "push 9",  "jmp exception_common",
    // 10-14: CPU가 에러 코드 push
    ".global isr10", "isr10:",           "push 10", "jmp exception_common",
    ".global isr11", "isr11:",           "push 11", "jmp exception_common",
    ".global isr12", "isr12:",           "push 12", "jmp exception_common",
    ".global isr13", "isr13:",           "push 13", "jmp exception_common",
    ".global isr14", "isr14:",           "push 14", "jmp exception_common",
    ".global isr15", "isr15:", "push 0", "push 15", "jmp exception_common",
    ".global isr16", "isr16:", "push 0", "push 16", "jmp exception_common",
    // 17 = #AC: 에러 코드 있음
    ".global isr17", "isr17:",           "push 17", "jmp exception_common",
    ".global isr18", "isr18:", "push 0", "push 18", "jmp exception_common",
    ".global isr19", "isr19:", "push 0", "push 19", "jmp exception_common",
    ".global isr20", "isr20:", "push 0", "push 20", "jmp exception_common",
    // 21 = #CP (Control Protection): 에러 코드 있음
    ".global isr21", "isr21:",           "push 21", "jmp exception_common",
    ".global isr22", "isr22:", "push 0", "push 22", "jmp exception_common",
    ".global isr23", "isr23:", "push 0", "push 23", "jmp exception_common",
    ".global isr24", "isr24:", "push 0", "push 24", "jmp exception_common",
    ".global isr25", "isr25:", "push 0", "push 25", "jmp exception_common",
    ".global isr26", "isr26:", "push 0", "push 26", "jmp exception_common",
    ".global isr27", "isr27:", "push 0", "push 27", "jmp exception_common",
    ".global isr28", "isr28:", "push 0", "push 28", "jmp exception_common",
    ".global isr29", "isr29:", "push 0", "push 29", "jmp exception_common",
    // 30 = #SX (Security Exception): 에러 코드 있음
    ".global isr30", "isr30:",           "push 30", "jmp exception_common",
    ".global isr31", "isr31:", "push 0", "push 31", "jmp exception_common",

    // ── isr128: int 0x80 syscall (벡터 0x80) — ALPHA 13 ─────────────────
    //
    // ring3 → ring0 전환: CPU가 스택에 SS,RSP,RFLAGS,CS,RIP(5개=40B) push.
    //
    // 저장 순서 (낮은 주소 먼저):
    //   push rax, rcx, rdx, rsi, rdi, r8, r9, r10, r11  (9개 = 72B)
    //
    // 스택 레이아웃 (9 push 직후, `call` 이전):
    //   [rsp+0x00] r11   [rsp+0x08] r10(arg4)  [rsp+0x10] r9(arg6)
    //   [rsp+0x18] r8(arg5)  [rsp+0x20] rdi(arg1)  [rsp+0x28] rsi(arg2)
    //   [rsp+0x30] rdx(arg3)  [rsp+0x38] rcx  [rsp+0x40] rax(syscall#)
    //
    // 1. `mov rdi, rsp` → frame_rsp = 9-push 직후 RSP (call 전이라 return addr 없음)
    // 2. `call syscall_dispatch(frame_rsp)` → RAX = syscall 반환값 (i64)
    // 3. `mov [rsp+0x40], rax` → 저장된 RAX 슬롯을 반환값으로 덮어씀
    //    → `pop rax` 시 user-space가 반환값을 읽음
    //
    // sys_exit: longjmp 발생 → IRETQ 미실행, 스텁 자연 종료.
    ".global isr128",
    "isr128:",
    "push rax", "push rcx", "push rdx",
    "push rsi",  "push rdi",
    "push r8",  "push r9",  "push r10", "push r11",
    "mov rdi, rsp",                 // frame_rsp → arg1
    "call syscall_dispatch",        // RAX = 반환값 (i64)
    "mov [rsp + 0x40], rax",        // 저장된 user RAX 슬롯 덮어쓰기
    "pop r11", "pop r10", "pop r9",  "pop r8",
    "pop rdi",  "pop rsi",
    "pop rdx",  "pop rcx", "pop rax", // rax = syscall 반환값
    "iretq",     // ring3 (사용자 코드)로 복귀
);
