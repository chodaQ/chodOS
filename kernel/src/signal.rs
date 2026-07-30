//! BETA 10: 시그널 서브시스템
//!
//! ## 전달 흐름
//!
//! 1. `raise_signal(signum)` → 현재 UserProc `pending_sigs` 비트 세팅.
//! 2. `deliver_pending_signals(frame_rsp)` → syscall_dispatch 종료 직전 호출.
//!    마스킹되지 않은 pending 시그널이 있으면:
//!    - SIG_DFL: 프로세스 종료 (`sys_exit_impl`).
//!    - SIG_IGN: 무시.
//!    - 핸들러: 유저 스택에 MuSigFrame 구성 + isr128 프레임 수정 → IRETQ 후 핸들러 실행.
//! 3. 핸들러 ret → 트램폴린 (mov rax,15; int 0x80) → `sys_rt_sigreturn`.
//! 4. `sys_rt_sigreturn(frame_rsp)` → isr128 프레임 복원 → IRETQ 후 원래 코드 재개.
//!
//! ## MuSigFrame 레이아웃 (유저 스택, 낮은 주소부터)
//! ```text
//! [+0]   trampoline[16B]: 0x48 0xC7 0xC0 0x0F 0x00 0x00 0x00  CD 80  90*7
//! [+16]  saved_rip        ← 인터럽트된 ring3 RIP
//! [+24]  saved_rflags
//! [+32]  saved_rsp
//! [+40]  saved_rax       ← syscall 반환값 (rt_sigreturn이 복원)
//! [+48]  saved_rcx
//! [+56]  saved_rdx
//! [+64]  saved_rsi
//! [+72]  saved_rdi
//! [+80]  saved_r8
//! [+88]  saved_r9
//! [+96]  saved_r10
//! [+104] saved_r11
//! [+112] saved_sig_mask  ← 핸들러 진입 전 sig_mask
//! total = 120B
//! ```
//!
//! ## isr128 프레임 레이아웃 (frame_rsp 기준)
//! ```text
//! +0x00 r11   +0x08 r10   +0x10 r9    +0x18 r8
//! +0x20 rdi   +0x28 rsi   +0x30 rdx   +0x38 rcx
//! +0x40 rax(ret)
//! +0x48 RIP   +0x50 CS    +0x58 RFLAGS  +0x60 RSP  +0x68 SS
//! ```

use core::sync::atomic::{AtomicU64, Ordering};

// ── 시그널 번호 (Linux 호환) ──────────────────────────────────────────────────

pub const SIGHUP:  u32 = 1;
pub const SIGINT:  u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGILL:  u32 = 4;
pub const SIGTRAP: u32 = 5;
pub const SIGABRT: u32 = 6;
pub const SIGBUS:  u32 = 7;
pub const SIGFPE:  u32 = 8;
pub const SIGKILL: u32 = 9;
pub const SIGSEGV: u32 = 11;
pub const SIGPIPE: u32 = 13;
pub const SIGALRM: u32 = 14;
pub const SIGTERM: u32 = 15;
pub const SIGCHLD: u32 = 17;
pub const SIGCONT: u32 = 18;
pub const SIGSTOP: u32 = 19;
pub const SIGWINCH: u32 = 28;

pub const NSIG: usize = 32;

// ── sigaction 핸들러 값 ───────────────────────────────────────────────────────

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

/// sigaction 플래그
pub const SA_NODEFER:   u64 = 0x4000_0000; // 핸들러 실행 중 해당 시그널 차단 안 함
pub const SA_RESETHAND: u64 = 0x8000_0000; // 1회 전달 후 SIG_DFL로 리셋
pub const SA_SIGINFO:   u64 = 0x0000_0004; // 3-arg handler (siginfo, ucontext 포함)

// ── 전역 pending 시그널 (하드웨어 발생 분) ───────────────────────────────────
// UserProc별 pending_sigs가 있으나 HW 예외(SIGSEGV 등)는 여기에도 기록
static GLOBAL_PENDING: AtomicU64 = AtomicU64::new(0);

// ── SigAction 구조체 ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Default)]
pub struct SigAction {
    pub handler:  u64, // SIG_DFL=0, SIG_IGN=1, 또는 ring3 핸들러 주소
    pub flags:    u64, // SA_* 플래그
    pub mask:     u64, // 핸들러 실행 중 추가로 차단할 시그널 마스크
}

// ── 공개 API ──────────────────────────────────────────────────────────────────

/// 현재 UserProc에 시그널 raise (비트 세팅만, 즉각 전달 아님).
pub fn raise_signal(signum: u32) {
    if signum == 0 || signum as usize > NSIG { return; }
    let bit = 1u64 << (signum - 1);
    let t = crate::process::userproc::table_pub();
    let cur = crate::process::userproc::current_idx();
    if let Some(ref mut p) = t[cur] {
        p.pending_sigs |= bit;
    } else {
        // UserProc 없음 → global pending
        GLOBAL_PENDING.fetch_or(bit, Ordering::Relaxed);
    }
    crate::serial_println!("[signal] raise sig={} pid={}", signum, crate::process::userproc::current_pid());
}

/// SIGCHLD를 특정 pid의 부모에게 raise.
///
/// try_wake_parent에서 부모가 Waiting 상태가 아닐 때 호출.
pub fn raise_sigchld_to(parent_pid: u64) {
    let t = crate::process::userproc::table_pub();
    for slot in t.iter_mut() {
        if slot.as_ref().map(|p| p.pid == parent_pid).unwrap_or(false) {
            if let Some(ref mut p) = slot {
                p.pending_sigs |= 1u64 << (SIGCHLD - 1);
            }
            return;
        }
    }
}

/// syscall_dispatch 종료 직전에 호출: 미처리 시그널 전달.
///
/// frame_rsp: isr128 프레임 RSP. frame[0x40]에는 이미 syscall 반환값이 기록됨.
pub fn deliver_pending_signals(frame_rsp: u64) {
    let t = crate::process::userproc::table_pub();
    let cur = crate::process::userproc::current_idx();
    let (pending, mask) = match t[cur].as_ref() {
        Some(p) => (p.pending_sigs, p.sig_mask),
        None    => return,
    };

    let deliverable = pending & !mask;
    if deliverable == 0 { return; }

    // 가장 낮은 번호의 시그널부터 처리
    let sig_idx = deliverable.trailing_zeros() as usize; // 0-based
    let signum  = (sig_idx + 1) as u32;

    // pending에서 제거
    t[cur].as_mut().unwrap().pending_sigs &= !(1u64 << sig_idx);

    let act = t[cur].as_ref().unwrap().sig_handlers[sig_idx];
    let flags = act.flags;
    // 여기서부터 t를 쓰지 않는다. table_pub()은 락 가드가 아니라 전역 static에
    // 대한 &'static mut을 그대로 돌려주므로 "해제"할 락은 없다. 다만 아래
    // 호출들(sys_exit_impl 등)이 같은 static에 대해 &mut을 새로 만들기 때문에,
    // t를 계속 들고 있으면 이중 &mut 별칭이 된다 — 실험 43에서 정리한
    // do_preempt/on_switch 문제와 같은 계열의 UB다. 이동시켜 재사용을 막는다.
    let _ = t;

    match act.handler {
        SIG_DFL => {
            // 기본 동작: 대부분 종료 (SIGCHLD/SIGCONT/SIGWINCH는 무시)
            if matches!(signum, SIGCHLD | SIGCONT | SIGWINCH) {
                crate::serial_println!("[signal] SIG_DFL ignore sig={}", signum);
                return;
            }
            crate::serial_println!("[signal] SIG_DFL terminate sig={}", signum);
            crate::syscall::sys_exit_impl(128 + signum as u64);
        }
        SIG_IGN => {
            crate::serial_println!("[signal] SIG_IGN sig={}", signum);
        }
        handler_addr => {
            crate::serial_println!("[signal] deliver sig={} to handler={:#x}", signum, handler_addr);
            // SA_NODEFER가 없으면 핸들러 실행 중 해당 시그널 차단
            let new_mask = {
                let t2 = crate::process::userproc::table_pub();
                let mut m = t2[cur].as_ref().map(|p| p.sig_mask).unwrap_or(0);
                if flags & SA_NODEFER == 0 { m |= 1u64 << sig_idx; }
                m |= act.mask;
                let saved_m = m;
                if let Some(ref mut p) = t2[cur].as_mut() {
                    p.sig_mask = m;
                }
                // SA_RESETHAND: 1회 전달 후 SIG_DFL로 리셋
                if flags & SA_RESETHAND != 0 {
                    if let Some(ref mut p) = t2[cur].as_mut() {
                        p.sig_handlers[sig_idx] = SigAction::default();
                    }
                }
                saved_m // 핸들러 진입 전 mask (복귀 시 복원용으로 sigframe에 저장)
            };
            // 실제 frame 수정 + sigframe 구성
            unsafe { install_sigframe(frame_rsp, signum as u64, handler_addr, new_mask); }
        }
    }
}

/// rt_sigreturn: 시그널 핸들러 종료 후 원래 컨텍스트 복원.
///
/// 트램폴린(mov rax,15; int 0x80)이 호출.
/// frame_rsp[0x60] = ring3 RSP = sigframe_addr (트램폴린 직후, 즉 sigframe 시작).
pub fn sys_rt_sigreturn(frame_rsp: u64) -> i64 {
    unsafe {
        // ring3 RSP = sigframe_addr (트램폴린 ret 후 RSP = sigframe 시작)
        let sf = *((frame_rsp + 0x60) as *const u64); // ring3 RSP
        // sigframe 오프셋에서 저장된 컨텍스트 읽기
        let rip      = *((sf + 16)  as *const u64);
        let rflags   = *((sf + 24)  as *const u64);
        let rsp      = *((sf + 32)  as *const u64);
        let rax      = *((sf + 40)  as *const u64);
        let rcx      = *((sf + 48)  as *const u64);
        let rdx      = *((sf + 56)  as *const u64);
        let rsi      = *((sf + 64)  as *const u64);
        let rdi      = *((sf + 72)  as *const u64);
        let r8       = *((sf + 80)  as *const u64);
        let r9       = *((sf + 88)  as *const u64);
        let r10      = *((sf + 96)  as *const u64);
        let r11      = *((sf + 104) as *const u64);
        let saved_mask = *((sf + 112) as *const u64);

        // 시그널 마스크 복원
        let t = crate::process::userproc::table_pub();
        let cur = crate::process::userproc::current_idx();
        if let Some(ref mut p) = t[cur] {
            p.sig_mask = saved_mask;
        }

        // isr128 프레임 원래 컨텍스트로 복원
        *((frame_rsp + 0x00) as *mut u64) = r11;
        *((frame_rsp + 0x08) as *mut u64) = r10;
        *((frame_rsp + 0x10) as *mut u64) = r9;
        *((frame_rsp + 0x18) as *mut u64) = r8;
        *((frame_rsp + 0x20) as *mut u64) = rdi;
        *((frame_rsp + 0x28) as *mut u64) = rsi;
        *((frame_rsp + 0x30) as *mut u64) = rdx;
        *((frame_rsp + 0x38) as *mut u64) = rcx;
        *((frame_rsp + 0x40) as *mut u64) = rax;   // syscall 반환값 복원
        *((frame_rsp + 0x48) as *mut u64) = rip;
        // CS, SS는 그대로 둠
        *((frame_rsp + 0x58) as *mut u64) = rflags;
        *((frame_rsp + 0x60) as *mut u64) = rsp;
    }
    // 반환값 자체는 iretq 후 rax로 무시됨 (frame[0x40]을 이미 복원했음)
    0
}

/// rt_sigaction(signum, new_act, old_act, sigsetsize)
///
/// Linux struct sigaction (x86-64):
///   [0]  sa_handler  (8B)
///   [8]  sa_flags    (8B)
///   [16] sa_restorer (8B, ignored)
///   [24] sa_mask     (8B)
pub fn sys_rt_sigaction(signum: u64, new_vaddr: u64, old_vaddr: u64, _sigsetsize: u64) -> i64 {
    if signum == 0 || signum as usize > NSIG { return crate::syscall::EINVAL; }
    let sig_idx = (signum - 1) as usize;

    let t = crate::process::userproc::table_pub();
    let cur = crate::process::userproc::current_idx();

    // old_act 출력
    if old_vaddr != 0 {
        let old = t[cur].as_ref().map(|p| p.sig_handlers[sig_idx]).unwrap_or_default();
        unsafe {
            *(old_vaddr as *mut u64)       = old.handler;
            *((old_vaddr + 8) as *mut u64) = old.flags;
            *((old_vaddr + 16) as *mut u64) = 0; // restorer
            *((old_vaddr + 24) as *mut u64) = old.mask;
        }
    }

    // new_act 적용
    if new_vaddr != 0 {
        let (handler, flags, sa_mask) = unsafe {
            (
                *(new_vaddr as *const u64),
                *((new_vaddr + 8) as *const u64),
                *((new_vaddr + 24) as *const u64),
            )
        };
        // SIGKILL/SIGSTOP은 오버라이드 불가
        if signum == SIGKILL as u64 || signum == SIGSTOP as u64 {
            return crate::syscall::EINVAL;
        }
        if let Some(ref mut p) = t[cur] {
            p.sig_handlers[sig_idx] = SigAction { handler, flags, mask: sa_mask };
        }
        crate::serial_println!("[signal] sigaction sig={} handler={:#x} flags={:#x}", signum, handler, flags);
    }
    0
}

/// rt_sigprocmask(how, set, oldset, sigsetsize)
///
/// how: SIG_BLOCK=0, SIG_UNBLOCK=1, SIG_SETMASK=2
pub fn sys_rt_sigprocmask(how: u64, set_vaddr: u64, old_vaddr: u64, _sigsetsize: u64) -> i64 {
    let t = crate::process::userproc::table_pub();
    let cur = crate::process::userproc::current_idx();

    let old_mask = t[cur].as_ref().map(|p| p.sig_mask).unwrap_or(0);

    if old_vaddr != 0 {
        unsafe { *(old_vaddr as *mut u64) = old_mask; }
    }

    if set_vaddr != 0 {
        let new_set = unsafe { *(set_vaddr as *const u64) };
        // SIGKILL(9)과 SIGSTOP(19)은 차단 불가
        let cant_block = (1u64 << (SIGKILL - 1)) | (1u64 << (SIGSTOP - 1));
        let new_mask = match how {
            0 => old_mask | (new_set & !cant_block),   // SIG_BLOCK
            1 => old_mask & !(new_set),                 // SIG_UNBLOCK
            2 => new_set & !cant_block,                 // SIG_SETMASK
            _ => return crate::syscall::EINVAL,
        };
        if let Some(ref mut p) = t[cur] {
            p.sig_mask = new_mask;
        }
    }
    0
}

// ── 내부 헬퍼 ─────────────────────────────────────────────────────────────────

/// isr128 프레임을 수정해 시그널 핸들러로 리디렉션 + sigframe 구성.
///
/// 호출 전 frame[0x40] = syscall 반환값 (deliver_pending_signals 시점에 이미 기록됨).
unsafe fn install_sigframe(frame_rsp: u64, signum: u64, handler: u64, new_mask: u64) {
    // 현재 ring3 컨텍스트
    let ring3_rip    = *((frame_rsp + 0x48) as *const u64);
    let ring3_rflags = *((frame_rsp + 0x58) as *const u64);
    let ring3_rsp    = *((frame_rsp + 0x60) as *const u64);
    let saved_r11    = *((frame_rsp + 0x00) as *const u64);
    let saved_r10    = *((frame_rsp + 0x08) as *const u64);
    let saved_r9     = *((frame_rsp + 0x10) as *const u64);
    let saved_r8     = *((frame_rsp + 0x18) as *const u64);
    let saved_rdi    = *((frame_rsp + 0x20) as *const u64);
    let saved_rsi    = *((frame_rsp + 0x28) as *const u64);
    let saved_rdx    = *((frame_rsp + 0x30) as *const u64);
    let saved_rcx    = *((frame_rsp + 0x38) as *const u64);
    let saved_rax    = *((frame_rsp + 0x40) as *const u64); // syscall 반환값

    // sigframe 주소: ring3 스택 최상단 - 128(redzone) - 120(frame), 16-byte 정렬
    const SIGFRAME_SIZE: u64 = 120;
    let sf = (ring3_rsp.saturating_sub(128 + SIGFRAME_SIZE)) & !0xF_u64;

    // 트램폴린 코드 (16B): mov rax,15 + int 0x80 + nop*7
    let trampoline: [u8; 16] = [
        0x48, 0xC7, 0xC0, 0x0F, 0x00, 0x00, 0x00, // mov rax, 15
        0xCD, 0x80,                                  // int 0x80
        0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,  // nop*7
    ];
    core::ptr::copy_nonoverlapping(trampoline.as_ptr(), sf as *mut u8, 16);

    // 저장된 컨텍스트
    *((sf + 16)  as *mut u64) = ring3_rip;
    *((sf + 24)  as *mut u64) = ring3_rflags;
    *((sf + 32)  as *mut u64) = ring3_rsp;
    *((sf + 40)  as *mut u64) = saved_rax;
    *((sf + 48)  as *mut u64) = saved_rcx;
    *((sf + 56)  as *mut u64) = saved_rdx;
    *((sf + 64)  as *mut u64) = saved_rsi;
    *((sf + 72)  as *mut u64) = saved_rdi;
    *((sf + 80)  as *mut u64) = saved_r8;
    *((sf + 88)  as *mut u64) = saved_r9;
    *((sf + 96)  as *mut u64) = saved_r10;
    *((sf + 104) as *mut u64) = saved_r11;
    *((sf + 112) as *mut u64) = new_mask; // 현재 sig_mask (rt_sigreturn이 복원)

    // 핸들러의 리턴 주소 (= 트램폴린): new_rsp - 8 위치에 삽입
    let new_rsp = sf - 8;
    *(new_rsp as *mut u64) = sf; // ret addr = 트램폴린 시작

    // isr128 프레임 수정: 핸들러로 리디렉션
    *((frame_rsp + 0x20) as *mut u64) = signum;  // rdi = signum (arg1)
    *((frame_rsp + 0x28) as *mut u64) = 0;        // rsi = NULL (siginfo*)
    *((frame_rsp + 0x30) as *mut u64) = 0;        // rdx = NULL (ucontext*)
    *((frame_rsp + 0x48) as *mut u64) = handler;  // RIP = 핸들러
    *((frame_rsp + 0x60) as *mut u64) = new_rsp;  // RSP = 트램폴린 ret addr 위
}
