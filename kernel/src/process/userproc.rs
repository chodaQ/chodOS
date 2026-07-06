//! BETA 9-11: 유저 프로세스/스레드 테이블 (fork / exec / wait4 / clone / futex)
//!
//! ## isr128 프레임 레이아웃 (frame_rsp 기준 오프셋)
//! ```text
//! +0x00 r11  +0x08 r10(a4)  +0x10 r9(a6)  +0x18 r8(a5)
//! +0x20 rdi(a1)  +0x28 rsi(a2)  +0x30 rdx(a3)  +0x38 rcx
//! +0x40 rax(nr→ret)
//! +0x48 RIP  +0x50 CS  +0x58 RFLAGS  +0x60 RSP  +0x68 SS
//! ```
//!
//! ## BETA 11: 스레딩
//! - `clone(CLONE_VM|CLONE_THREAD|CLONE_SETTLS|...)` → 공유 CR3, 새 kstack
//! - `iretq_to_frame` → WRMSR FS_BASE (per-thread TLS 복원)
//! - `futex_wait_impl` → 블록 + pick_next_runnable
//! - `futex_wake_impl` → Ready 전환
//! - `sched_yield_impl` → cooperative context switch
//! - `thread exit` → child_tid_ptr 클리어 + futex_wake + 자동 reap

use alloc::{string::String, vec::Vec};

const KSTACK_SIZE:    usize = 64 * 1024;
const MAX_PROCS:      usize = 16;        // BETA 11: 스레드 지원으로 슬롯 확대
const ISR_FRAME_SIZE: usize = 14 * 8;   // 112B

// ── clone(2) 플래그 ───────────────────────────────────────────────────────────
const CLONE_VM:             u64 = 0x0000_0100;
const CLONE_THREAD:         u64 = 0x0001_0000;
const CLONE_SETTLS:         u64 = 0x0008_0000;
const CLONE_PARENT_SETTID:  u64 = 0x0010_0000;
const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;
const CLONE_CHILD_SETTID:   u64 = 0x0100_0000;

// ── 상태 ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UState {
    Empty,
    Ready,           // fork/clone 직후, 아직 실행 안 됨
    Running,         // ring3에서 실행 중
    Waiting(u64),    // wait4 block 중; 값 = 대기 child pid (u64::MAX = 임의)
    Zombie(i32),     // 종료됨, 부모 reap 대기
    Blocked(u64),    // futex_wait block 중; 값 = uaddr
}

// ── 유저 프로세스/스레드 엔트리 ──────────────────────────────────────────────

pub struct UserProc {
    pub pid:              u64,
    pub parent_pid:       u64,     // 0 = 최초 프로세스
    pub state:            UState,
    pub user_cr3:         u64,
    pub kstack:           Vec<u8>,
    pub saved_frame_rsp:  u64,
    // BETA 10: 시그널
    pub sig_handlers: [crate::signal::SigAction; crate::signal::NSIG],
    pub sig_mask:     u64,
    pub pending_sigs: u64,
    // BETA 11: 스레딩
    pub fs_base:          u64,    // MSR_FS_BASE (TLS 포인터)
    pub thread_group_pid: u64,    // 0 = 독립 프로세스, else = tgid
    pub child_tid_ptr:    u64,    // CLONE_CHILD_CLEARTID 대상 주소
    // BETA 12: CWD
    pub cwd:              String, // 현재 작업 디렉토리
    // BETA 15: mmap 가상 주소 범프 포인터
    pub mmap_next:        u64,   // 다음 mmap 할당 시작 주소
    // BETA 21: brk(2) 힙 포인터 (초기값 = ELF BSS end, exec_replace에서 설정)
    pub brk_base:         u64,   // 힙 시작 주소 (불변)
    pub brk_cur:          u64,   // 현재 brk (= sbrk 현재 끝)
}

impl UserProc {
    #[inline]
    pub fn kstack_top(&self) -> u64 {
        self.kstack.as_ptr() as u64 + KSTACK_SIZE as u64
    }
}

// ── 전역 테이블 ───────────────────────────────────────────────────────────────

static mut USER_PROCS:    [Option<UserProc>; MAX_PROCS] =
    [None, None, None, None, None, None, None, None,
     None, None, None, None, None, None, None, None];
static mut CURRENT_UPROC: usize = 0;

#[inline]
fn table() -> &'static mut [Option<UserProc>; MAX_PROCS] {
    unsafe { &mut *core::ptr::addr_of_mut!(USER_PROCS) }
}

/// signal.rs에서 접근할 수 있도록 pub 노출.
#[inline]
pub fn table_pub() -> &'static mut [Option<UserProc>; MAX_PROCS] { table() }

/// 현재 UserProc 슬롯 인덱스.
#[inline]
pub fn current_idx() -> usize { unsafe { CURRENT_UPROC } }

// ── 공개 API ─────────────────────────────────────────────────────────────────

/// ring3 ELF 진입 직전 호출: UserProc 등록 + TSS.RSP0 갱신.
pub fn register(pid: u64, parent_pid: u64, user_cr3: u64) {
    let t = table();
    // 동일 pid 재진입 (exec-in-place)
    for (i, slot) in t.iter_mut().enumerate() {
        if slot.as_ref().map(|p| p.pid) == Some(pid) {
            if let Some(ref mut p) = slot {
                p.user_cr3 = user_cr3;
                p.state    = UState::Running;
            }
            unsafe { CURRENT_UPROC = i; }
            tss_update(t[i].as_ref().unwrap().kstack_top());
            return;
        }
    }
    // 빈 슬롯 할당
    for (i, slot) in t.iter_mut().enumerate() {
        if slot.is_none() {
            let mut ks = Vec::new();
            ks.resize(KSTACK_SIZE, 0u8);
            let top = ks.as_ptr() as u64 + KSTACK_SIZE as u64;
            *slot = Some(UserProc {
                pid, parent_pid,
                state:           UState::Running,
                user_cr3,
                kstack:          ks,
                saved_frame_rsp: 0,
                sig_handlers:    [crate::signal::SigAction::default(); crate::signal::NSIG],
                sig_mask:        0,
                pending_sigs:    0,
                fs_base:         0,
                thread_group_pid: 0,
                child_tid_ptr:   0,
                cwd:             String::from("/"),
                mmap_next:       0x4000_0000, // BETA 15: mmap 시작 주소 (1GB)
                brk_base:        0,           // exec_replace에서 ELF BSS end로 설정
                brk_cur:         0,
            });
            unsafe { CURRENT_UPROC = i; }
            tss_update(top);
            crate::serial_println!("[uproc] register pid={} parent={} slot={}", pid, parent_pid, i);
            return;
        }
    }
    crate::serial_println!("[uproc] ERROR: no slot for pid={}", pid);
}

/// 현재 프로세스 PID (getpid: tgid 반환, gettid: own pid 반환).
pub fn current_pid() -> u64 {
    table()[unsafe { CURRENT_UPROC }].as_ref().map(|p| p.pid).unwrap_or(0)
}

/// getpid() 용: tgid 반환 (스레드 그룹 없으면 자신의 pid).
pub fn current_tgid() -> u64 {
    table()[unsafe { CURRENT_UPROC }].as_ref().map(|p| {
        if p.thread_group_pid != 0 { p.thread_group_pid } else { p.pid }
    }).unwrap_or(0)
}

/// fork된 자식 또는 clone 스레드인지.
pub fn is_forked_child() -> bool {
    table()[unsafe { CURRENT_UPROC }].as_ref().map(|p| p.parent_pid != 0).unwrap_or(false)
}

/// 현재 프로세스의 CWD 반환 (BETA 12).
pub fn current_cwd() -> String {
    table()[unsafe { CURRENT_UPROC }]
        .as_ref()
        .map(|p| p.cwd.clone())
        .unwrap_or_else(|| String::from("/"))
}

/// 현재 프로세스의 CWD 설정 (BETA 12).
pub fn set_cwd(path: &str) {
    let cur = unsafe { CURRENT_UPROC };
    if let Some(ref mut p) = table()[cur] {
        p.cwd = String::from(path);
    }
}

/// BETA 21: exec 후 brk 기준점 설정 (ELF BSS end 기준).
/// load_exec 완료 후 exec_replace에서 호출.
pub fn set_brk_base(end: u64) {
    let cur = unsafe { CURRENT_UPROC };
    if let Some(ref mut p) = table()[cur] {
        // 4KB 정렬: ELF BSS end를 페이지 경계로 올림
        let aligned = (end + 0xFFF) & !0xFFF;
        p.brk_base = aligned;
        p.brk_cur  = aligned;
    }
}

/// BETA 21: brk(2) 구현.
/// addr=0 → 현재 brk 반환.
/// addr>brk_base → brk 확장 (새 페이지 demand 매핑) 후 새 brk 반환.
/// addr<brk_base → 실패, 현재 brk 반환 (musl 동작과 일치).
pub fn sys_brk(addr: u64) -> i64 {
    let cur = unsafe { CURRENT_UPROC };
    if let Some(ref mut p) = table()[cur] {
        if addr == 0 || addr < p.brk_base {
            return p.brk_cur as i64;
        }
        let new_brk = (addr + 0xFFF) & !0xFFF;
        // 새 브레이크 영역을 demand-paging VMA로 등록 (RW)
        if new_brk > p.brk_cur {
            let pages = ((new_brk - p.brk_cur) as usize + 4095) / 4096;
            crate::process::vma::insert(p.brk_cur, pages, 0x3 /* RW */, true /* lazy */);
        }
        p.brk_cur = new_brk;
        new_brk as i64
    } else {
        -12  // ENOMEM
    }
}

/// BETA 15: mmap용 가상 주소 할당 (bump allocator).
/// `pages` 페이지만큼 예약하고 시작 가상 주소를 반환.
pub fn alloc_mmap_vaddr(pages: usize) -> u64 {
    let cur = unsafe { CURRENT_UPROC };
    if let Some(ref mut p) = table()[cur] {
        let addr = p.mmap_next;
        p.mmap_next = addr + (pages as u64) * 4096;
        return addr;
    }
    0x4000_0000
}

/// BETA 15: 현재 프로세스 CR3 반환.
pub fn current_cr3() -> u64 {
    let cur = unsafe { CURRENT_UPROC };
    table()[cur].as_ref().map(|p| p.user_cr3).unwrap_or(0)
}

/// set_tid_address(tidptr) → 현재 tid 반환.
pub fn sys_set_tid_address(tidptr: u64) -> i64 {
    let t = table();
    let cur = unsafe { CURRENT_UPROC };
    if let Some(ref mut p) = t[cur] {
        p.child_tid_ptr = tidptr;
        return p.pid as i64;
    }
    0
}

/// fork: 유저 주소 공간 deep-copy + 자식 kstack 구성.
pub fn fork_current(frame_rsp: u64) -> u64 {
    let t = table();
    let cur = unsafe { CURRENT_UPROC };
    let (parent_pid, parent_cr3) = match t[cur].as_ref() {
        Some(p) => (p.pid, p.user_cr3),
        None => { crate::serial_println!("[uproc] fork: no current"); return u64::MAX; }
    };

    let child_pid = crate::process::scheduler::alloc_pid();
    let child_cr3 = unsafe { crate::paging::clone_user_space(parent_cr3) };

    let mut ks: Vec<u8> = Vec::new();
    ks.resize(KSTACK_SIZE, 0u8);
    let ks_top   = ks.as_ptr() as u64 + KSTACK_SIZE as u64;
    let frame_dst = ks_top - ISR_FRAME_SIZE as u64;
    unsafe {
        core::ptr::copy_nonoverlapping(frame_rsp as *const u8, frame_dst as *mut u8, ISR_FRAME_SIZE);
        *((frame_dst + 0x40) as *mut u64) = 0; // rax=0 (child)
    }

    let (parent_handlers, parent_mask) = {
        let p = t[cur].as_ref().unwrap();
        (p.sig_handlers, p.sig_mask)
    };

    for slot in t.iter_mut() {
        if slot.is_none() {
            *slot = Some(UserProc {
                pid:             child_pid,
                parent_pid,
                state:           UState::Ready,
                user_cr3:        child_cr3,
                kstack:          ks,
                saved_frame_rsp: frame_dst,
                sig_handlers:    parent_handlers,
                sig_mask:        parent_mask,
                pending_sigs:    0,
                fs_base:         0,
                thread_group_pid: 0,
                child_tid_ptr:   0,
                cwd: {
                    let t2 = table();
                    let c2 = unsafe { CURRENT_UPROC };
                    t2[c2].as_ref().map(|p| p.cwd.clone()).unwrap_or_else(|| String::from("/"))
                },
                mmap_next: {
                    let t2 = table();
                    let c2 = unsafe { CURRENT_UPROC };
                    t2[c2].as_ref().map(|p| p.mmap_next).unwrap_or(0x4000_0000)
                },
                brk_base: {
                    let t2 = table();
                    let c2 = unsafe { CURRENT_UPROC };
                    t2[c2].as_ref().map(|p| p.brk_base).unwrap_or(0)
                },
                brk_cur: {
                    let t2 = table();
                    let c2 = unsafe { CURRENT_UPROC };
                    t2[c2].as_ref().map(|p| p.brk_cur).unwrap_or(0)
                },
            });
            break;
        }
    }
    crate::serial_println!("[uproc] fork: parent={} child={}", parent_pid, child_pid);
    child_pid
}

/// BETA 11: clone(CLONE_THREAD|CLONE_VM|…) — 스레드 생성.
///
/// - 공유 CR3 (CLONE_VM)
/// - 새 kstack, parent의 isr128 프레임 복사 (rax=0, RSP=child_stack)
/// - CLONE_SETTLS: child.fs_base = newtls
/// - CLONE_CHILD_SETTID: *child_tid_vaddr = child_pid
/// - CLONE_PARENT_SETTID: *parent_tid_vaddr = child_pid
/// - 반환: child_pid (부모 rax에 기록), child는 Ready 상태로 테이블에 삽입
pub fn clone_thread(flags: u64, child_stack: u64, parent_tid_vaddr: u64,
                    child_tid_vaddr: u64, newtls: u64, frame_rsp: u64) -> u64 {
    let t = table();
    let cur = unsafe { CURRENT_UPROC };

    let (parent_pid, parent_cr3, tgid, parent_fs, parent_handlers, parent_mask) = match t[cur].as_ref() {
        Some(p) => {
            let tgid = if p.thread_group_pid != 0 { p.thread_group_pid } else { p.pid };
            (p.pid, p.user_cr3, tgid, p.fs_base, p.sig_handlers, p.sig_mask)
        }
        None => { crate::serial_println!("[thread] clone: no current"); return u64::MAX; }
    };

    let child_pid = crate::process::scheduler::alloc_pid();

    // 자식 kstack + 프레임 복사
    let mut ks: Vec<u8> = Vec::new();
    ks.resize(KSTACK_SIZE, 0u8);
    let ks_top    = ks.as_ptr() as u64 + KSTACK_SIZE as u64;
    let frame_dst = ks_top - ISR_FRAME_SIZE as u64;
    unsafe {
        core::ptr::copy_nonoverlapping(frame_rsp as *const u8, frame_dst as *mut u8, ISR_FRAME_SIZE);
        *((frame_dst + 0x40) as *mut u64) = 0;          // rax = 0 (child의 clone 반환값)
        if child_stack != 0 {
            *((frame_dst + 0x60) as *mut u64) = child_stack; // RSP = 자식 스택
        }
    }

    // TLS
    let child_fs = if flags & CLONE_SETTLS != 0 { newtls } else { parent_fs };

    // CLONE_PARENT_SETTID
    if flags & CLONE_PARENT_SETTID != 0 && parent_tid_vaddr != 0
        && parent_tid_vaddr < 0x0000_8000_0000_0000 {
        unsafe { *(parent_tid_vaddr as *mut u32) = child_pid as u32; }
    }

    // CLONE_CHILD_SETTID (커널이 직접 씀)
    if flags & CLONE_CHILD_SETTID != 0 && child_tid_vaddr != 0
        && child_tid_vaddr < 0x0000_8000_0000_0000 {
        unsafe { *(child_tid_vaddr as *mut u32) = child_pid as u32; }
    }

    let tid_ptr = if flags & (CLONE_CHILD_CLEARTID | CLONE_CHILD_SETTID) != 0 {
        child_tid_vaddr
    } else { 0 };

    // CR3: CLONE_VM이면 공유, 아니면 COW (미구현 시 공유로 폴백)
    let child_cr3 = if flags & CLONE_VM != 0 {
        parent_cr3
    } else {
        unsafe { crate::paging::clone_user_space(parent_cr3) }
    };

    let is_thread = flags & CLONE_THREAD != 0;

    for slot in t.iter_mut() {
        if slot.is_none() {
            *slot = Some(UserProc {
                pid:              child_pid,
                parent_pid,
                state:            UState::Ready,
                user_cr3:         child_cr3,
                kstack:           ks,
                saved_frame_rsp:  frame_dst,
                sig_handlers:     parent_handlers,
                sig_mask:         parent_mask,
                pending_sigs:     0,
                fs_base:          child_fs,
                thread_group_pid: if is_thread { tgid } else { 0 },
                child_tid_ptr:    tid_ptr,
                cwd: {
                    let t2 = table();
                    let c2 = unsafe { CURRENT_UPROC };
                    t2[c2].as_ref().map(|p| p.cwd.clone()).unwrap_or_else(|| String::from("/"))
                },
                mmap_next: {
                    let t2 = table();
                    let c2 = unsafe { CURRENT_UPROC };
                    t2[c2].as_ref().map(|p| p.mmap_next).unwrap_or(0x4000_0000)
                },
                brk_base: {
                    let t2 = table();
                    let c2 = unsafe { CURRENT_UPROC };
                    t2[c2].as_ref().map(|p| p.brk_base).unwrap_or(0)
                },
                brk_cur: {
                    let t2 = table();
                    let c2 = unsafe { CURRENT_UPROC };
                    t2[c2].as_ref().map(|p| p.brk_cur).unwrap_or(0)
                },
            });
            break;
        }
    }

    crate::serial_println!(
        "[thread] clone: parent={} child={} tgid={} fs={:#x}",
        parent_pid, child_pid, tgid, child_fs
    );
    child_pid
}

/// wait4 / waitpid 구현.
pub fn wait4_impl(child_pid_arg: u64, status_ptr: u64, options: u64, frame_rsp: u64) -> i64 {
    let t = table();
    let cur = unsafe { CURRENT_UPROC };
    let parent_pid = match t[cur].as_ref() { Some(p) => p.pid, None => return -10 };
    let wnohang    = options & 1 != 0;

    let child_idx = match find_child(t, parent_pid, child_pid_arg) {
        Some(i) => i,
        None    => return -10,
    };
    let (child_pid, child_state) = {
        let c = t[child_idx].as_ref().unwrap();
        (c.pid, c.state)
    };

    if let UState::Zombie(code) = child_state {
        write_status(status_ptr, code);
        let ret = child_pid as i64;
        t[child_idx] = None;
        return ret;
    }
    if wnohang { return 0; }

    {
        let p = t[cur].as_mut().unwrap();
        p.saved_frame_rsp = frame_rsp;
        p.state           = UState::Waiting(child_pid);
    }

    let (ccr3, cfrsp, ktop) = {
        let c = t[child_idx].as_mut().unwrap();
        c.state = UState::Running;
        (c.user_cr3, c.saved_frame_rsp, c.kstack_top())
    };
    unsafe { CURRENT_UPROC = child_idx; }
    unsafe { iretq_to_frame(ccr3, cfrsp, ktop) }
}

/// 자식/스레드 exit 시 호출.
///
/// - 스레드 (thread_group_pid != 0): tid 클리어 + futex_wake + 자동 reap + pick_next
/// - 프로세스 (parent_pid != 0): 부모 깨우기
pub fn try_wake_parent(exit_code: i32) {
    let t = table();
    let cur = unsafe { CURRENT_UPROC };

    let (child_pid, parent_pid, tgid, tid_ptr) = match t[cur].as_ref() {
        Some(p) => (p.pid, p.parent_pid, p.thread_group_pid, p.child_tid_ptr),
        None    => return,
    };

    // ── 스레드 exit ──────────────────────────────────────────────────────────
    if tgid != 0 {
        // child_tid_ptr 클리어 + futex_wake (pthread_join 해제)
        if tid_ptr != 0 && tid_ptr < 0x0000_8000_0000_0000 {
            unsafe { *(tid_ptr as *mut u32) = 0; }
            futex_wake_impl(tid_ptr, u64::MAX);
        }
        // 스레드 reap
        t[cur] = None;
        crate::serial_println!("[thread] exit: tid={} tgid={}", child_pid, tgid);

        // 다음 실행 가능한 스레드/프로세스로 전환
        if let Some(next) = find_next_ready_from(0) {
            let (cr3, frsp, ktop) = {
                let p = t[next].as_ref().unwrap();
                (p.user_cr3, p.saved_frame_rsp, p.kstack_top())
            };
            t[next].as_mut().unwrap().state = UState::Running;
            unsafe { CURRENT_UPROC = next; }
            unsafe { iretq_to_frame(cr3, frsp, ktop) }
        }
        return; // 다른 스레드 없음 → longjmp fallback
    }

    // ── 프로세스 exit ─────────────────────────────────────────────────────────
    if parent_pid == 0 { return; }

    // zombie 표시
    if let Some(ref mut p) = t[cur] { p.state = UState::Zombie(exit_code); }

    // 부모 찾기
    let pidx = match t.iter().position(|s| {
        s.as_ref().map(|p| p.pid == parent_pid).unwrap_or(false)
    }) {
        Some(i) => i,
        None    => return,
    };

    let waited = match t[pidx].as_ref().unwrap().state {
        UState::Waiting(w) => w,
        _ => {
            crate::signal::raise_sigchld_to(parent_pid);
            return;
        }
    };
    if waited != child_pid && waited != u64::MAX { return; }

    let (pcr3, pfrsp, pktop) = {
        let p = t[pidx].as_ref().unwrap();
        (p.user_cr3, p.saved_frame_rsp, p.kstack_top())
    };
    t[pidx].as_mut().unwrap().state = UState::Running;
    unsafe { CURRENT_UPROC = pidx; }

    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) pcr3, options(nostack, preserves_flags));
        let sptr = *((pfrsp + 0x28) as *const u64);
        if sptr != 0 && sptr < 0x0000_8000_0000_0000 {
            *(sptr as *mut i32) = exit_code << 8;
        }
        *((pfrsp + 0x40) as *mut u64) = child_pid;
    }
    t[cur] = None;

    unsafe { iretq_to_frame(pcr3, pfrsp, pktop) }
}

/// exec in forked child: 자식의 주소 공간을 새 ELF로 교체.
///
/// - ET_EXEC (정적 실행 파일): 기존 load_elf_into_space 경로
/// - ET_DYN (PIE / PT_INTERP 있음): DynLinker 경로 → load_dyn_exec
pub fn exec_replace(elf_data: &[u8]) -> ! {
    let t = table();
    let cur = unsafe { CURRENT_UPROC };

    // exec 전 이전 프로세스가 남긴 VMA / mmap 추적 상태를 초기화
    crate::process::vma::reset();
    crate::syscall::mmap_table_reset();

    // VFS 라이브러리 제공자 — tmpfs 우선, 없으면 ext4에서 읽기
    let lib_provider = |name: &str| -> Option<alloc::vec::Vec<u8>> {
        let path = alloc::format!("/lib/{}", name);
        crate::vfs::read_file(&path)
            .or_else(|| crate::vfs::ext4_read_file(&path))
    };

    // PT_INTERP 존재 (동적 링킹 필요) → DynLinker 경로
    // ET_EXEC + PT_INTERP (일반 동적 실행 파일)과 ET_DYN (PIE) 모두 처리
    let needs_dynlink = crate::elf::Elf64::parse(elf_data)
        .map(|e| e.interp().is_some())
        .unwrap_or(false);

    let (new_cr3, new_entry, new_user_rsp) = if needs_dynlink {
        // ── DynLinker 단일 단계 경로 (PIE / 동적 실행 파일) ─────────────────
        // 1. 빈 PML4 생성 (커널 상위 절반 공유)
        let new_cr3 = unsafe { crate::paging::alloc_user_pml4() };

        // 2. DynLinker로 실행 파일 + 인터프리터 + 의존 라이브러리 모두 로드 + 재배치
        let mut dl = crate::dynlink::DynLinker::new(new_cr3);
        let entry = dl.load_exec(elf_data, lib_provider)
            .unwrap_or(0x0000_0000_0040_0000); // fallback: EXEC_LOAD_BASE
        dl.resolve_all();

        // 3. aux vector 포함 유저 스택 셋업
        // AT_BASE = 인터프리터 로드 기준 주소 (ld-musl이 로드된 곳)
        //   ET_DYN: exec_load_base (EXEC_LOAD_BASE)가 아니라 interp_entry에서 역산
        //   ET_EXEC with interp: INTERP_LOAD_BASE
        // AT_ENTRY = exec의 원래 진입점 (인터프리터가 최종적으로 점프할 곳)
        let at_base = if dl.interp_entry != 0 {
            crate::dynlink::INTERP_LOAD_BASE
        } else {
            dl.exec_load_base
        };
        // brk base = exec ELF 의 BSS end (가장 높은 PT_LOAD vaddr+memsz, 4KB 정렬)
        let exec_bss_end = dl.exec_vaddr_end;
        set_brk_base(exec_bss_end);
        let user_rsp = unsafe {
            crate::paging::setup_user_stack(
                new_cr3,
                dl.exec_phdr_va, dl.exec_phent, dl.exec_phnum,
                at_base, dl.exec_entry,  // AT_BASE, AT_ENTRY (exec 진입점)
            )
        };
        (new_cr3, entry, user_rsp)
    } else {
        // ── 정적 실행 파일 (ET_EXEC) ─────────────────────────────────────────
        unsafe { crate::paging::load_elf_into_space(elf_data) }
    };

    let (ffrsp, ktop) = {
        let p = t[cur].as_mut().unwrap();
        p.user_cr3 = new_cr3;
        p.state    = UState::Running;
        let ktop  = p.kstack_top();
        let ffrsp = ktop - ISR_FRAME_SIZE as u64;
        unsafe {
            core::ptr::write_bytes(ffrsp as *mut u8, 0, ISR_FRAME_SIZE);
            *((ffrsp + 0x48) as *mut u64) = new_entry;
            *((ffrsp + 0x50) as *mut u64) = crate::interrupts::gdt::USER_CS_RPL3 as u64;
            *((ffrsp + 0x58) as *mut u64) = 0x202;
            *((ffrsp + 0x60) as *mut u64) = new_user_rsp;
            *((ffrsp + 0x68) as *mut u64) = crate::interrupts::gdt::USER_SS_RPL3 as u64;
        }
        p.saved_frame_rsp = ffrsp;
        (ffrsp, ktop)
    };
    unsafe { iretq_to_frame(new_cr3, ffrsp, ktop) }
}

// ── BETA 11: 스케줄러 헬퍼 ────────────────────────────────────────────────────

/// sched_yield: 다음 Ready 스레드로 양보.
pub fn sched_yield_impl(frame_rsp: u64) -> i64 {
    let cur = unsafe { CURRENT_UPROC };
    // 반환값 사전 기록 (switch 후 dispatch가 반환 안 할 수 있음)
    unsafe { *((frame_rsp + 0x40) as *mut i64) = 0; }
    {
        let t = table();
        if let Some(ref mut p) = t[cur] {
            p.saved_frame_rsp = frame_rsp;
            p.state           = UState::Ready;
        }
    }
    if let Some(next) = find_next_ready_from(cur + 1) {
        let (cr3, frsp, ktop) = {
            let t = table();
            let p = t[next].as_ref().unwrap();
            (p.user_cr3, p.saved_frame_rsp, p.kstack_top())
        };
        table()[next].as_mut().unwrap().state = UState::Running;
        unsafe { CURRENT_UPROC = next; }
        unsafe { iretq_to_frame(cr3, frsp, ktop) }
    }
    // 다른 Ready 스레드 없음 — 자신 재개
    table()[cur].as_mut().unwrap().state = UState::Running;
    0
}

/// futex 디스패처.
pub fn futex_dispatch(uaddr: u64, op: u64, val: u64,
                      _timeout: u64, _uaddr2: u64, _val3: u64,
                      frame_rsp: u64) -> i64 {
    const FUTEX_WAIT:         u64 = 0;
    const FUTEX_WAKE:         u64 = 1;
    const FUTEX_PRIVATE_FLAG: u64 = 128;
    const FUTEX_CLOCK_RT:     u64 = 256;
    let op_kind = op & !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_RT);

    match op_kind {
        FUTEX_WAIT => futex_wait_impl(uaddr, val, frame_rsp),
        FUTEX_WAKE => futex_wake_impl(uaddr, val),
        _          => 0,
    }
}

/// futex_wait: *uaddr == val이면 블록 → pick_next_runnable.
fn futex_wait_impl(uaddr: u64, expected: u64, frame_rsp: u64) -> i64 {
    if uaddr == 0 { return crate::syscall::EFAULT; }
    let cur_val = unsafe { *(uaddr as *const u32) };
    if cur_val != expected as u32 { return crate::syscall::EAGAIN; }

    let cur = unsafe { CURRENT_UPROC };
    // 반환값 사전 기록 (wake 후 frame 재사용)
    unsafe { *((frame_rsp + 0x40) as *mut i64) = 0; }
    {
        let t = table();
        if let Some(ref mut p) = t[cur] {
            p.saved_frame_rsp = frame_rsp;
            p.state           = UState::Blocked(uaddr);
        }
    }
    crate::serial_println!("[futex] wait uaddr={:#x} cur={}", uaddr, cur);

    if let Some(next) = find_next_ready_from(0) {
        let (cr3, frsp, ktop) = {
            let t = table();
            let p = t[next].as_ref().unwrap();
            (p.user_cr3, p.saved_frame_rsp, p.kstack_top())
        };
        table()[next].as_mut().unwrap().state = UState::Running;
        unsafe { CURRENT_UPROC = next; }
        unsafe { iretq_to_frame(cr3, frsp, ktop) }
    }

    // 다른 스레드 없음 — 블록 해제 후 즉시 반환
    table()[cur].as_mut().unwrap().state = UState::Running;
    crate::syscall::EAGAIN
}

/// futex_wake: uaddr에서 대기 중인 스레드를 최대 max_count개 Ready로 전환.
pub fn futex_wake_impl(uaddr: u64, max_count: u64) -> i64 {
    let t = table();
    let mut woken = 0i64;
    for slot in t.iter_mut() {
        if woken >= max_count as i64 { break; }
        if let Some(ref mut p) = slot {
            if let UState::Blocked(addr) = p.state {
                if addr == uaddr {
                    p.state = UState::Ready;
                    woken += 1;
                    crate::serial_println!("[futex] wake pid={} uaddr={:#x}", p.pid, uaddr);
                }
            }
        }
    }
    woken
}

// ── 내부 헬퍼 ─────────────────────────────────────────────────────────────────

fn find_child(t: &[Option<UserProc>; MAX_PROCS], parent_pid: u64, target: u64) -> Option<usize> {
    t.iter().position(|s| {
        s.as_ref().map(|p| {
            p.parent_pid == parent_pid
                && (target == u64::MAX || p.pid == target)
                && p.thread_group_pid == 0 // 스레드는 wait4 대상 아님
        }).unwrap_or(false)
    })
}

/// start 이후 (wrap-around 포함) 첫 번째 Ready 슬롯 인덱스.
fn find_next_ready_from(start: usize) -> Option<usize> {
    let t = table();
    for i in 0..MAX_PROCS {
        let idx = (start + i) % MAX_PROCS;
        if t[idx].as_ref().map(|p| matches!(p.state, UState::Ready)).unwrap_or(false) {
            return Some(idx);
        }
    }
    None
}

fn write_status(ptr: u64, code: i32) {
    if ptr != 0 && ptr < 0x0000_8000_0000_0000 {
        unsafe { *(ptr as *mut i32) = code << 8; }
    }
}

fn tss_update(kstack_top: u64) {
    crate::interrupts::gdt::set_tss_rsp0(kstack_top);
    unsafe { crate::interrupts::handlers::syscall_kern_rsp = kstack_top; }
}

// ── 저수준 IRETQ ──────────────────────────────────────────────────────────────

extern "C" { fn up_iretq_from_frame() -> !; }

/// CR3 전환 + FS_BASE 복원 + isr128 프레임에서 ring3으로 IRETQ.
///
/// CURRENT_UPROC가 대상 스레드를 가리키도록 미리 설정해야 함.
pub unsafe fn iretq_to_frame(cr3: u64, frame_rsp: u64, kstack_top: u64) -> ! {
    tss_update(kstack_top);

    // 대상 스레드의 FS_BASE 복원 (TLS 포인터)
    let fs_base = {
        let t = table();
        let cur = CURRENT_UPROC;
        t[cur].as_ref().map(|p| p.fs_base).unwrap_or(0)
    };
    if fs_base != 0 {
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xC000_0100u32,
            in("eax") (fs_base & 0xFFFF_FFFF) as u32,
            in("edx") (fs_base >> 32) as u32,
            options(nomem, nostack),
        );
    }

    core::arch::asm!(
        "mov cr3, {cr3}",
        "mov rsp, {rsp}",
        "jmp up_iretq_from_frame",
        cr3 = in(reg) cr3,
        rsp = in(reg) frame_rsp,
        options(noreturn, nostack),
    );
}
