/// Linux Compat — syscall 디스패치 (BETA 2-1)
///
/// ## ABI  (`int 0x80`)
/// RAX=nr  RDI=a1  RSI=a2  RDX=a3  R10=a4  R8=a5  R9=a6
///
/// ## isr128 스택 레이아웃
/// ```text
/// [+0x00] r11   [+0x08] r10(a4)  [+0x10] r9(a6)  [+0x18] r8(a5)
/// [+0x20] rdi(a1) [+0x28] rsi(a2) [+0x30] rdx(a3) [+0x38] rcx
/// [+0x40] rax(nr)  [+0x48] RIP  [+0x50] CS  [+0x58] RFLAGS  [+0x60] RSP  [+0x68] SS
/// ```

pub mod dev;
pub mod fd;
pub mod fs;
pub mod mem;
pub mod pipe;
pub mod proc;
pub mod sock;
pub mod sysinfo;

// ── Linux x86-64 syscall 번호 ────────────────────────────────────────────────

pub const SYS_READ:       u64 = 0;
pub const SYS_WRITE:      u64 = 1;
pub const SYS_OPEN:       u64 = 2;
pub const SYS_CLOSE:      u64 = 3;
// BETA 3 추가
pub const SYS_PIPE:       u64 = 22;
pub const SYS_SELECT:     u64 = 23;
pub const SYS_SOCKET:     u64 = 41;
pub const SYS_CONNECT:    u64 = 42;
pub const SYS_ACCEPT:     u64 = 43;
pub const SYS_SENDTO:     u64 = 44;
pub const SYS_RECVFROM:   u64 = 45;
pub const SYS_SENDMSG:    u64 = 46;
pub const SYS_RECVMSG:    u64 = 47;
pub const SYS_SHUTDOWN:   u64 = 48;
pub const SYS_BIND:       u64 = 49;
pub const SYS_LISTEN:     u64 = 50;
pub const SYS_GETSOCKNAME:u64 = 51;
pub const SYS_GETPEERNAME:u64 = 52;
pub const SYS_SETSOCKOPT: u64 = 54;
pub const SYS_GETSOCKOPT: u64 = 55;
pub const SYS_CLONE:      u64 = 56;    // BETA 11: 스레드 생성
pub const SYS_EPOLL_WAIT:     u64 = 232;
pub const SYS_EPOLL_CTL:      u64 = 233;
pub const SYS_EPOLL_CREATE:   u64 = 281;
pub const SYS_EPOLL_CREATE1:  u64 = 291;
pub const SYS_PIPE2:          u64 = 293;
pub const SYS_PSELECT6:       u64 = 270;
pub const SYS_TRUNCATE:   u64 = 76;   // BETA 12
pub const SYS_FTRUNCATE:  u64 = 77;   // BETA 12
pub const SYS_RENAME:     u64 = 82;   // BETA 12
pub const SYS_STAT:       u64 = 4;
pub const SYS_FSTAT:      u64 = 5;
pub const SYS_LSTAT:      u64 = 6;
pub const SYS_POLL:       u64 = 7;
pub const SYS_LSEEK:      u64 = 8;
pub const SYS_MMAP:       u64 = 9;
pub const SYS_MPROTECT:   u64 = 10;
pub const SYS_MUNMAP:     u64 = 11;
pub const SYS_BRK:        u64 = 12;
pub const SYS_RT_SIGACTION:   u64 = 13;
pub const SYS_RT_SIGPROCMASK: u64 = 14;
pub const SYS_RT_SIGRETURN:   u64 = 15;
pub const SYS_IOCTL:      u64 = 16;
pub const SYS_PREAD64:    u64 = 17;
pub const SYS_PWRITE64:   u64 = 18;
pub const SYS_READV:      u64 = 19;
pub const SYS_WRITEV:     u64 = 20;
pub const SYS_ACCESS:     u64 = 21;
pub const SYS_SCHED_YIELD: u64 = 24;
pub const SYS_MREMAP:     u64 = 25;
pub const SYS_MSYNC:      u64 = 26;
pub const SYS_MINCORE:    u64 = 27;
pub const SYS_MADVISE:    u64 = 28;
pub const SYS_DUP:        u64 = 32;
pub const SYS_DUP2:       u64 = 33;
pub const SYS_NANOSLEEP:  u64 = 35;
pub const SYS_GETPID:     u64 = 39;
pub const SYS_FORK:       u64 = 57;
pub const SYS_EXECVE:     u64 = 59;
pub const SYS_EXIT:       u64 = 60;
pub const SYS_WAITPID:    u64 = 61;
pub const SYS_KILL:       u64 = 62;
pub const SYS_UNAME:      u64 = 63;
pub const SYS_FCNTL:      u64 = 72;
pub const SYS_GETCWD:     u64 = 79;
pub const SYS_CHDIR:      u64 = 80;
pub const SYS_FCHDIR:     u64 = 81;
pub const SYS_MKDIR:      u64 = 83;
pub const SYS_RMDIR:      u64 = 84;
pub const SYS_CREAT:      u64 = 85;
pub const SYS_LINK:       u64 = 86;
pub const SYS_UNLINK:     u64 = 87;
pub const SYS_SYMLINK:    u64 = 88;
pub const SYS_READLINK:   u64 = 89;
pub const SYS_CHMOD:      u64 = 90;
pub const SYS_FCHMOD:     u64 = 91;
pub const SYS_CHOWN:      u64 = 92;
pub const SYS_UMASK:      u64 = 95;
pub const SYS_GETTIMEOFDAY:  u64 = 96;
pub const SYS_GETRLIMIT:  u64 = 97;
pub const SYS_SYSINFO:    u64 = 99;
pub const SYS_GETUID:     u64 = 102;
pub const SYS_GETGID:     u64 = 104;
pub const SYS_GETEUID:    u64 = 107;
pub const SYS_GETEGID:    u64 = 108;
pub const SYS_GETPPID:    u64 = 110;
pub const SYS_GETPGRP:    u64 = 111;
pub const SYS_SETSID:     u64 = 112;
pub const SYS_STATFS:     u64 = 137;
pub const SYS_FSTATFS:    u64 = 138;
pub const SYS_PERSONALITY: u64 = 135;
pub const SYS_ARCH_PRCTL: u64 = 158;
pub const SYS_SETRLIMIT:  u64 = 160;
pub const SYS_SYNC:       u64 = 162;
pub const SYS_MLOCK:      u64 = 149;
pub const SYS_MUNLOCK:    u64 = 150;
pub const SYS_MLOCKALL:   u64 = 151;
pub const SYS_MUNLOCKALL: u64 = 152;
pub const SYS_GETTID:     u64 = 186;
pub const SYS_FUTEX:      u64 = 202;  // Linux 202 = futex (MuKernel 전용은 400번대로)
pub const SYS_SET_TID_ADDR: u64 = 218;
pub const SYS_GETDENTS64: u64 = 217;
pub const SYS_CLOCK_GETTIME: u64 = 228;
pub const SYS_CLOCK_GETRES:  u64 = 229;
pub const SYS_EXIT_GROUP: u64 = 231;
pub const SYS_TGKILL:     u64 = 234;
pub const SYS_OPENAT:     u64 = 257;
pub const SYS_MKDIRAT:    u64 = 258;   // BETA 12
pub const SYS_NEWFSTATAT: u64 = 262;
pub const SYS_UNLINKAT:   u64 = 263;   // BETA 12
pub const SYS_RENAMEAT:   u64 = 264;   // BETA 12
pub const SYS_LINKAT:     u64 = 265;   // BETA 12
pub const SYS_SYMLINKAT:  u64 = 266;   // BETA 12
pub const SYS_SET_ROBUST_LIST: u64 = 273;
pub const SYS_RENAMEAT2:  u64 = 316;   // BETA 12
pub const SYS_GET_ROBUST_LIST: u64 = 274;
pub const SYS_PRLIMIT64:  u64 = 302;
pub const SYS_CLONE3:     u64 = 435;

// MuKernel 전용 — 400번대 (Linux 번호와 충돌 없음)
pub const SYS_PKG_LIST:      u64 = 400;
pub const SYS_PKG_GETARGS:   u64 = 401;
pub const SYS_EXT4_READ:     u64 = 402;
// BETA 8: 패키지 관리자 고도화
pub const SYS_PKG_INFO:      u64 = 403; // (name_ptr, buf, max) → bytes
pub const SYS_PKG_INSTALL:   u64 = 404; // (name_ptr) → 0 | -ENOENT
pub const SYS_PKG_REMOVE:    u64 = 405; // (name_ptr) → 0 | -ENOENT
pub const SYS_PKG_INSTALLED: u64 = 406; // (buf, max) → bytes

// ── errno ────────────────────────────────────────────────────────────────────

pub(crate) const EPERM:   i64 = -1;
pub(crate) const ENOENT:  i64 = -2;
pub(crate) const EIO:     i64 = -5;
pub(crate) const EBADF:   i64 = -9;
pub(crate) const EAGAIN:  i64 = -11;
pub(crate) const ENOMEM:  i64 = -12;
pub(crate) const EFAULT:  i64 = -14;
pub(crate) const EEXIST:  i64 = -17;
pub(crate) const EINVAL:  i64 = -22;
pub(crate) const ENOTTY:  i64 = -25;
pub(crate) const ENOSYS:  i64 = -38;

// /dev/null 전용 stub fd
pub(crate) const FD_DEV_NULL: u64 = 99;

// ── 유틸 ─────────────────────────────────────────────────────────────────────

pub(crate) unsafe fn read_cstr(vaddr: u64) -> Option<&'static str> {
    if vaddr == 0 { return None; }
    let ptr = vaddr as *const u8;
    let mut len = 0usize;
    while *ptr.add(len) != 0 && len < 4096 { len += 1; }
    core::str::from_utf8(core::slice::from_raw_parts(ptr, len)).ok()
}

pub(crate) fn sys_exit_impl(code: u64) -> ! {
    use core::sync::atomic::Ordering;
    crate::interrupts::handlers::USER_EXIT_CODE.store(code as i32, Ordering::Relaxed);
    crate::serial_println!("[syscall] exit({}) code={}", code, code);

    // BETA 9: forked child → 부모가 wait4 중이면 부모에게 IRETQ (noreturn)
    // not-forked child → 여기서 반환 후 longjmp
    crate::process::userproc::try_wake_parent(code as i32);

    // old mechanism: longjmp to kernel_main → after_user_demo
    unsafe {
        core::arch::asm!(
            "mov cr3, {kcr3}",
            "mov rsp, {krsp}",
            "jmp {resume}",
            kcr3   = in(reg) crate::paging::KERNEL_CR3,
            krsp   = in(reg) crate::paging::KERNEL_MAIN_RSP,
            resume = sym crate::interrupts::handlers::after_user_demo,
            options(noreturn, nostack),
        );
    }
}

// ── 디스패처 ─────────────────────────────────────────────────────────────────

pub fn dispatch(frame: *const u64) -> i64 {
    let frame_rsp = frame as u64; // isr128 프레임 RSP (BETA 9 fork/wait4 전달용)
    let (nr, a1, a2, a3, a4, a5, a6) = unsafe {
        let nr = *frame.add(8); // rax  [+0x40]
        let a1 = *frame.add(4); // rdi  [+0x20]
        let a2 = *frame.add(5); // rsi  [+0x28]
        let a3 = *frame.add(6); // rdx  [+0x30]
        let a4 = *frame.add(1); // r10  [+0x08]
        let a5 = *frame.add(3); // r8   [+0x18]
        let a6 = *frame.add(2); // r9   [+0x10]
        (nr, a1, a2, a3, a4, a5, a6)
    };

    crate::serial_println!(
        "[syscall] nr={:<3} ({})  a1={:#x} a2={:#x} a3={:#x}",
        nr, name(nr), a1, a2, a3
    );

    let ret: i64 = match nr {
        // ── 기본 I/O ───────────────────────────────────────────────────────
        SYS_READ    => sys_read(a1, a2, a3),
        SYS_WRITE   => sys_write(a1, a2, a3),
        SYS_OPEN    => sys_open(a1, a2),
        SYS_CLOSE   => sys_close(a1),
        // BETA 3: pipe
        SYS_PIPE    => pipe::sys_pipe(a1),
        SYS_PIPE2   => pipe::sys_pipe2(a1, a2),
        SYS_SELECT  => pipe::sys_select(a1, a2, a3, a4, a5),
        SYS_PSELECT6 => pipe::sys_pselect6(a1, a2, a3, a4, a5, a6),
        // BETA 3: epoll
        SYS_EPOLL_CREATE  => pipe::sys_epoll_create(a1),
        SYS_EPOLL_CREATE1 => pipe::sys_epoll_create(a1),
        SYS_EPOLL_CTL     => pipe::sys_epoll_ctl(a1, a2, a3, a4),
        SYS_EPOLL_WAIT    => pipe::sys_epoll_wait(a1, a2, a3, a4),
        // BETA 3: socket
        SYS_SOCKET      => sock::sys_socket(a1, a2, a3),
        SYS_CONNECT     => sock::sys_connect(a1, a2, a3),
        SYS_ACCEPT      => sock::sys_accept(a1, a2, a3),
        SYS_BIND        => sock::sys_bind(a1, a2, a3),
        SYS_LISTEN      => sock::sys_listen(a1, a2),
        SYS_GETSOCKNAME => sock::sys_getsockname(a1, a2, a3),
        SYS_GETPEERNAME => sock::sys_getpeername(a1, a2, a3),
        SYS_SENDTO      => sock::sys_sendto(a1, a2, a3, a4, a5, a6),
        SYS_RECVFROM    => sock::sys_recvfrom(a1, a2, a3, a4, a5, a6),
        SYS_SENDMSG     => sock::sys_sendmsg(a1, a2, a3),
        SYS_RECVMSG     => sock::sys_recvmsg(a1, a2, a3),
        SYS_SHUTDOWN    => sock::sys_shutdown(a1, a2),
        SYS_SETSOCKOPT  => sock::sys_setsockopt(a1, a2, a3, a4, a5),
        SYS_GETSOCKOPT  => sock::sys_getsockopt(a1, a2, a3, a4, a5),
        SYS_MMAP    => sys_mmap(a1, a2, a3, a4, a5 as i64, a6),
        SYS_MUNMAP  => sys_munmap(a1, a2),
        SYS_BRK     => ENOMEM,
        // BETA 14: ioctl — 터미널/dev 지원
        SYS_IOCTL   => dev::sys_ioctl(a1, a2, a3),
        // BETA 12: truncate / rename
        SYS_TRUNCATE  => fs::sys_truncate(a1, a2),
        SYS_FTRUNCATE => fs::sys_ftruncate(a1, a2),
        SYS_RENAME    => fs::sys_rename(a1, a2),
        // BETA 11: getpid → tgid, gettid → own tid
        SYS_GETPID  => crate::process::userproc::current_tgid() as i64,
        // BETA 9: 실제 fork — 유저 주소 공간 복사 + 자식 kstack 구성
        SYS_FORK    => crate::process::userproc::fork_current(frame_rsp) as i64,
        // BETA 11: clone(CLONE_THREAD|CLONE_VM|…) — 스레드 생성
        SYS_CLONE   => crate::process::userproc::clone_thread(a1, a2, a3, a4, a5, frame_rsp) as i64,
        SYS_EXECVE  => sys_execve(a1, a2),
        SYS_EXIT    => sys_exit_impl(a1),
        // BETA 9: 실제 wait4 — 자식 zombie면 즉시 반환, 아니면 block → child IRETQ
        SYS_WAITPID => crate::process::userproc::wait4_impl(a1, a2, a3, frame_rsp),
        SYS_PKG_LIST      => sys_pkg_list(a1, a2),
        SYS_PKG_GETARGS   => sys_pkg_getargs(a1, a2),
        SYS_EXT4_READ     => sys_ext4_read(a1, a2, a3),
        SYS_PKG_INFO      => sys_pkg_info(a1, a2, a3),
        SYS_PKG_INSTALL   => sys_pkg_install(a1),
        SYS_PKG_REMOVE    => sys_pkg_remove(a1),
        SYS_PKG_INSTALLED => sys_pkg_installed(a1, a2),

        // ── BETA 2-1: fs.rs ────────────────────────────────────────────────
        SYS_STAT       => fs::sys_stat(a1, a2),
        // BETA 4: fstat은 HandleTable에서 실제 크기 조회
        SYS_FSTAT      => sys_fstat_v4(a1, a2),
        SYS_LSTAT      => fs::sys_lstat(a1, a2),
        // BETA 4: lseek는 FileResource.seek() 사용
        SYS_LSEEK      => sys_lseek_v4(a1, a2, a3),
        // BETA 4: pread64는 FileResource.pread() 사용 (위치 불변)
        SYS_PREAD64    => sys_pread64_v4(a1, a2, a3, a4),
        SYS_PWRITE64   => fs::sys_pwrite64(a1, a2, a3, a4),
        SYS_READV      => fs::sys_readv(a1, a2, a3),
        SYS_WRITEV     => fs::sys_writev(a1, a2, a3),
        SYS_ACCESS     => fs::sys_access(a1, a2),
        SYS_DUP        => fs::sys_dup(a1),
        SYS_DUP2       => fs::sys_dup2(a1, a2),
        SYS_FCNTL      => fs::sys_fcntl(a1, a2, a3),
        SYS_GETCWD     => fs::sys_getcwd(a1, a2),
        SYS_CHDIR      => fs::sys_chdir(a1),
        SYS_FCHDIR     => fs::sys_fchdir(a1),
        SYS_MKDIR      => fs::sys_mkdir(a1, a2),
        SYS_RMDIR      => fs::sys_rmdir(a1),
        SYS_CREAT      => fs::sys_creat(a1, a2),
        SYS_LINK       => fs::sys_link(a1, a2),
        SYS_UNLINK     => fs::sys_unlink(a1),
        SYS_SYMLINK    => fs::sys_symlink(a1, a2),
        SYS_READLINK   => fs::sys_readlink(a1, a2, a3),
        SYS_CHMOD      => fs::sys_chmod(a1, a2),
        SYS_FCHMOD     => fs::sys_fchmod(a1, a2),
        SYS_CHOWN      => fs::sys_chown(a1, a2, a3),
        SYS_UMASK      => fs::sys_umask(a1),
        SYS_OPENAT     => fs::sys_openat(a1 as i64, a2, a3, a4),
        SYS_NEWFSTATAT => fs::sys_newfstatat(a1 as i64, a2, a3, a4),
        // BETA 12: *at variants
        SYS_MKDIRAT    => fs::sys_mkdirat(a1 as i64, a2, a3),
        SYS_UNLINKAT   => fs::sys_unlinkat(a1 as i64, a2, a3),
        SYS_RENAMEAT   => fs::sys_renameat(a1 as i64, a2, a3 as i64, a4),
        SYS_RENAMEAT2  => fs::sys_renameat(a1 as i64, a2, a3 as i64, a4),
        SYS_LINKAT     => fs::sys_linkat(a1 as i64, a2, a3 as i64, a4, a5),
        SYS_SYMLINKAT  => fs::sys_symlinkat(a1, a2 as i64, a3),
        SYS_PRLIMIT64  => fs::sys_prlimit64(a1, a2, a3, a4),
        SYS_GETDENTS64 => fs::sys_getdents64(a1, a2, a3),
        SYS_STATFS     => fs::sys_statfs(a1, a2),
        SYS_FSTATFS    => fs::sys_fstatfs(a1, a2),

        // ── BETA 2-1: proc.rs ──────────────────────────────────────────────
        SYS_POLL         => proc::sys_poll(a1, a2, a3),
        // BETA 10: 실제 시그널 서브시스템
        SYS_RT_SIGACTION   => crate::signal::sys_rt_sigaction(a1, a2, a3, a4),
        SYS_RT_SIGPROCMASK => crate::signal::sys_rt_sigprocmask(a1, a2, a3, a4),
        SYS_RT_SIGRETURN   => crate::signal::sys_rt_sigreturn(frame_rsp),
        // BETA 11: sched_yield → cooperative thread switch
        SYS_SCHED_YIELD  => crate::process::userproc::sched_yield_impl(frame_rsp),
        SYS_NANOSLEEP    => proc::sys_nanosleep(a1, a2),
        SYS_UNAME        => proc::sys_uname(a1),
        SYS_KILL         => proc::sys_kill(a1, a2),
        SYS_GETUID       => 0,
        SYS_GETGID       => 0,
        SYS_GETEUID      => 0,
        SYS_GETEGID      => 0,
        SYS_GETPPID      => 0,
        SYS_GETPGRP      => crate::process::scheduler::current_pid() as i64,
        SYS_SETSID       => crate::process::scheduler::current_pid() as i64,
        SYS_PERSONALITY  => 0,
        SYS_ARCH_PRCTL   => proc::sys_arch_prctl(a1, a2),
        SYS_SETRLIMIT    => 0,
        SYS_SYNC         => 0,
        // BETA 11: gettid → own tid (스레드별 고유)
        SYS_GETTID       => crate::process::userproc::current_pid() as i64,
        // BETA 11: futex — 실제 블로킹 FUTEX_WAIT / FUTEX_WAKE
        SYS_FUTEX        => crate::process::userproc::futex_dispatch(a1, a2, a3, a4, a5, a6, frame_rsp),
        // BETA 11: set_tid_address — child_tid_ptr 저장
        SYS_SET_TID_ADDR => crate::process::userproc::sys_set_tid_address(a1),
        SYS_EXIT_GROUP   => proc::sys_exit_group(a1),
        SYS_TGKILL       => 0,
        SYS_CLONE3       => ENOSYS,
        SYS_SET_ROBUST_LIST => 0,
        SYS_GET_ROBUST_LIST => 0,

        // ── BETA 2-1: mem.rs ───────────────────────────────────────────────
        SYS_MPROTECT   => 0,
        SYS_MREMAP     => ENOMEM,
        SYS_MSYNC      => 0,
        SYS_MINCORE    => ENOSYS,
        SYS_MADVISE    => 0,
        SYS_MLOCK      => 0,
        SYS_MUNLOCK    => 0,
        SYS_MLOCKALL   => 0,
        SYS_MUNLOCKALL => 0,

        // ── BETA 2-1: sysinfo.rs ───────────────────────────────────────────
        SYS_GETTIMEOFDAY  => sysinfo::sys_gettimeofday(a1, a2),
        SYS_GETRLIMIT     => sysinfo::sys_getrlimit(a1, a2),
        SYS_SYSINFO       => sysinfo::sys_sysinfo(a1),
        SYS_CLOCK_GETTIME => sysinfo::sys_clock_gettime(a1, a2),
        SYS_CLOCK_GETRES  => sysinfo::sys_clock_getres(a1, a2),

        _ => { crate::serial_println!("[syscall] ENOSYS nr={}", nr); ENOSYS }
    };

    crate::serial_println!("[syscall] → {}", ret);
    ret
}

// ── BETA 15: mmap 추적 테이블 ────────────────────────────────────────────────

use spin::Mutex as SpinMutex;
use alloc::collections::BTreeMap;

/// vaddr → pages (munmap에서 해제할 페이지 수)
static MMAP_TABLE: SpinMutex<BTreeMap<u64, usize>> = SpinMutex::new(BTreeMap::new());

// ── 기본 구현 함수 ────────────────────────────────────────────────────────────

fn sys_write(fd: u64, buf: u64, count: u64) -> i64 {
    if count == 0 { return 0; }
    // pipe write fd
    if pipe::is_write_fd(fd as i32) {
        return pipe::pipe_write(buf as *const u8, count as usize);
    }
    match fd {
        1 | 2 => {
            let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, count as usize) };
            for &b in bytes { crate::serial::write_byte(b); }
            crate::term::write_bytes(bytes);
            count as i64
        }
        FD_DEV_NULL => count as i64,
        f if f >= 3 => {
            // BETA 14: dev fd 우선
            if dev::is_dev_fd(f as u32) {
                dev::write(f as u32, buf as *const u8, count as usize)
            } else {
                fd::write_fd(f as u32, buf as *const u8, count as usize)
            }
        }
        _ => EBADF,
    }
}

fn sys_read(fd: u64, buf: u64, count: u64) -> i64 {
    if count == 0 { return 0; }
    // pipe read fd (블로킹)
    if pipe::is_read_fd(fd as i32) {
        loop {
            let n = pipe::pipe_read(buf as *mut u8, count as usize);
            if n > 0 { return n; }
            unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
        }
    }
    match fd {
        0 => {
            let byte = crate::kbd::read_key_blocking();
            if byte == 3 { proc::deliver_sigint(); }
            unsafe { *(buf as *mut u8) = byte; }
            1
        }
        FD_DEV_NULL => 0,
        f if f >= 3 => {
            // BETA 14: dev fd 우선
            if dev::is_dev_fd(f as u32) {
                dev::read(f as u32, buf as *mut u8, count as usize)
            } else {
                fd::read(f as u32, buf as *mut u8, count as usize)
            }
        }
        _ => EAGAIN,
    }
}

fn sys_open(path_vaddr: u64, flags: u64) -> i64 {
    // BETA 13: 통합 오픈 로직으로 위임
    fs::sys_open_impl(path_vaddr, flags)
}

fn sys_close(fd: u64) -> i64 {
    match fd {
        0 | 1 | 2 => 0,      // stdio는 닫지 않음
        FD_DEV_NULL => 0,
        f if pipe::is_read_fd(f as i32)  => 0, // pipe 닫기는 간단 무시
        f if pipe::is_write_fd(f as i32) => 0,
        f => { fd::close(f as u32); 0 }
    }
}

// ── BETA 4: HandleTable 연동 I/O 함수 ────────────────────────────────────────

/// fstat v4 — BETA 14: dev fd 포함
fn sys_fstat_v4(raw_fd: u64, stat_vaddr: u64) -> i64 {
    match raw_fd {
        0 | 1 | 2 | FD_DEV_NULL => fs::sys_fstat(raw_fd, stat_vaddr),
        f => {
            // dev fd
            if dev::is_dev_fd(f as u32) {
                return dev::fstat(f as u32, stat_vaddr);
            }
            // 일반 파일 fd
            if let Some((size, is_dir)) = fd::fstat(f as u32) {
                fs::fill_stat_pub(stat_vaddr, size, is_dir);
                0
            } else {
                EBADF
            }
        }
    }
}

/// lseek v4 — fd≥3은 FileResource.seek() 사용
fn sys_lseek_v4(raw_fd: u64, offset: u64, whence: u64) -> i64 {
    if raw_fd >= 3 {
        fd::seek(raw_fd as u32, offset as i64, whence)
    } else {
        fs::sys_lseek(raw_fd, offset, whence)
    }
}

/// pread64 v4 — fd≥3은 FileResource.pread() 사용 (파일 위치 불변)
fn sys_pread64_v4(raw_fd: u64, buf_vaddr: u64, count: u64, offset: u64) -> i64 {
    if raw_fd >= 3 {
        fd::pread(raw_fd as u32, buf_vaddr as *mut u8, count as usize, offset as usize)
    } else {
        fs::sys_pread64(raw_fd, buf_vaddr, count, offset)
    }
}

fn sys_mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: i64, offset: u64) -> i64 {
    const MAP_ANONYMOUS: u64 = 0x20;
    const MAP_FIXED:     u64 = 0x10;

    if len == 0 { return EINVAL; }

    let pages = (len as usize + 4095) / 4096;
    let cr3   = crate::process::userproc::current_cr3();
    if cr3 == 0 { return ENOMEM; }

    let is_anon  = flags & MAP_ANONYMOUS != 0 || fd == -1;
    let is_fixed = flags & MAP_FIXED != 0;

    // MAP_FIXED: addr가 기존 mmap 영역 안이면 해당 구간 재매핑
    if is_fixed && addr != 0 {
        let in_existing = {
            let t = MMAP_TABLE.lock();
            t.range(..=addr).next_back()
                .map(|(&base, &cnt)| addr >= base && addr + len <= base + (cnt as u64) * 4096)
                .unwrap_or(false)
        };
        if in_existing {
            if !is_anon {
                fill_mmap_from_fd(addr as *mut u8, pages * 4096, fd as u32, offset as usize);
            } else {
                unsafe { core::ptr::write_bytes(addr as *mut u8, 0, pages * 4096); }
            }
            // VMA prot 갱신 (BETA 16)
            crate::process::vma::update_prot(addr, len, prot as u32);
            return addr as i64;
        }
        // MAP_FIXED인데 기존 영역 밖: 새로 할당
        let vaddr = addr;
        if !is_anon {
            let data = read_for_mmap(fd as u32, offset as usize, pages * 4096);
            unsafe { crate::paging::mmap_map(cr3, vaddr, &data, true); }
        } else {
            // BETA 17: MAP_ANONYMOUS eager (MAP_FIXED는 즉시 매핑 — 주소 고정 필요)
            unsafe { crate::paging::mmap_anon(cr3, vaddr, pages, true); }
        }
        MMAP_TABLE.lock().insert(vaddr, pages);
        crate::process::vma::insert(vaddr, pages, prot as u32, false);
        crate::serial_println!("[mmap] fixed addr={:#x} pages={} fd={}", vaddr, pages, fd);
        return vaddr as i64;
    }

    // 일반 mmap: 가상 주소 범프 할당
    let vaddr = crate::process::userproc::alloc_mmap_vaddr(pages);

    if is_anon {
        // BETA 17: MAP_ANONYMOUS → demand paging (lazy)
        // 물리 페이지를 지금 할당하지 않음 — 첫 접근 시 #PF → demand_alloc_page
        crate::process::vma::insert(vaddr, pages, prot as u32, true);
        crate::serial_println!(
            "[mmap] lazy vaddr={:#x} pages={} prot={:#x} (demand)",
            vaddr, pages, prot,
        );
    } else {
        // 파일 백킹: 즉시 매핑 (파일 데이터 로드 필요)
        let data = read_for_mmap(fd as u32, offset as usize, pages * 4096);
        unsafe { crate::paging::mmap_map(cr3, vaddr, &data, true); }
        crate::process::vma::insert(vaddr, pages, prot as u32, false);
        crate::serial_println!(
            "[mmap] eager vaddr={:#x} pages={} fd={} off={:#x}",
            vaddr, pages, fd, offset,
        );
    }

    MMAP_TABLE.lock().insert(vaddr, pages);
    crate::policy::observe_mmap(crate::process::scheduler::current_pid());
    vaddr as i64
}

/// fd에서 offset 위치부터 최대 len 바이트 읽기 (mmap 파일 백킹용)
fn read_for_mmap(fd: u32, offset: usize, len: usize) -> alloc::vec::Vec<u8> {
    let mut buf = alloc::vec![0u8; len];
    // FileResource pread
    let n = fd::pread(fd, buf.as_mut_ptr(), len, offset);
    if n > 0 { buf.truncate(n as usize); } else { buf.clear(); }
    buf
}

/// 이미 매핑된 메모리 위치에 fd 파일 데이터를 덮어씀 (MAP_FIXED 재매핑)
fn fill_mmap_from_fd(dst: *mut u8, dst_len: usize, fd: u32, offset: usize) {
    let data = read_for_mmap(fd, offset, dst_len);
    let n = data.len().min(dst_len);
    if n > 0 {
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dst, n); }
    }
}

fn sys_munmap(addr: u64, len: u64) -> i64 {
    let pages = (len as usize + 4095) / 4096;
    let cr3 = crate::process::userproc::current_cr3();
    if cr3 == 0 { return 0; }
    let actual_pages = MMAP_TABLE.lock().remove(&addr).unwrap_or(pages);
    if actual_pages > 0 {
        unsafe { crate::paging::munmap_pages(cr3, addr, actual_pages); }
        crate::serial_println!("[munmap] addr={:#x} pages={}", addr, actual_pages);
    }
    0
}

fn sys_execve(path_vaddr: u64, argv_vaddr: u64) -> i64 {
    use core::sync::atomic::Ordering;
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    let name = path.rsplit('/').next().unwrap_or(path);
    let idx = match crate::pkg::find(name) {
        Some(i) => i,
        None => { crate::serial_println!("[mukg] exec not found: {:?}", name); return ENOENT; }
    };
    let args = if argv_vaddr != 0 {
        unsafe { read_cstr(argv_vaddr) }.unwrap_or("").as_bytes()
    } else { b"" };
    crate::pkg::set_args(args);

    // BETA 9: fork된 자식이 exec를 호출한 경우 → 주소 공간 교체 후 IRETQ (noreturn)
    if crate::process::userproc::is_forked_child() {
        let elf_data = crate::pkg::PACKAGES[idx].elf;
        crate::serial_println!("[mukg] exec_replace in child: {:?}", name);
        crate::process::userproc::exec_replace(elf_data);
    }

    // 기존 경로: mushell에서 직접 exec → PENDING_PKG_IDX 설정 → longjmp
    crate::interrupts::handlers::PENDING_PKG_IDX.store(idx as i32, Ordering::Relaxed);
    crate::serial_println!("[mukg] exec {:?}", name);
    sys_exit_impl(0)
}

fn sys_pkg_list(buf_vaddr: u64, max_len: u64) -> i64 {
    let max = max_len as usize;
    let out = buf_vaddr as *mut u8;
    let mut pos = 0usize;
    for (i, pkg) in crate::pkg::PACKAGES.iter().enumerate() {
        let marker: &[u8] = if crate::pkg::is_installed(i) { b"[*] " } else { b"[ ] " };
        for s in &[marker, pkg.name.as_bytes(), b" ", pkg.version.as_bytes(),
                   b" - ", pkg.desc.as_bytes(), b"\n"] {
            let copy = s.len().min(max.saturating_sub(pos));
            if copy == 0 { break; }
            unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), out.add(pos), copy); }
            pos += copy;
        }
        if pos >= max { break; }
    }
    pos as i64
}

fn sys_pkg_getargs(buf_vaddr: u64, max_len: u64) -> i64 {
    let args = crate::pkg::get_args();
    let copy = args.len().min(max_len as usize);
    unsafe { core::ptr::copy_nonoverlapping(args.as_ptr(), buf_vaddr as *mut u8, copy); }
    copy as i64
}

fn sys_ext4_read(path_vaddr: u64, buf_vaddr: u64, max_len: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    match crate::vfs::ext4_read_file(path) {
        Some(data) => {
            let copy = data.len().min(max_len as usize);
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), buf_vaddr as *mut u8, copy); }
            copy as i64
        }
        None => ENOENT,
    }
}

// ── BETA 8: 패키지 관리자 고도화 ─────────────────────────────────────────────

/// 패키지 정보를 buf에 기록. 반환: 기록 바이트 수, -ENOENT if not found.
fn sys_pkg_info(name_vaddr: u64, buf_vaddr: u64, max_len: u64) -> i64 {
    let name = match unsafe { read_cstr(name_vaddr) } { Some(s) => s, None => return EINVAL };
    let idx = match crate::pkg::find(name) { Some(i) => i, None => return ENOENT };
    let pkg = &crate::pkg::PACKAGES[idx];
    let out = buf_vaddr as *mut u8;
    let max = max_len as usize;
    let mut pos = 0usize;

    fn write_chunks(out: *mut u8, pos: &mut usize, max: usize, chunks: &[&[u8]]) {
        for s in chunks {
            let copy = s.len().min(max.saturating_sub(*pos));
            if copy == 0 { return; }
            unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), out.add(*pos), copy); }
            *pos += copy;
        }
    }

    write_chunks(out, &mut pos, max, &[b"Name:    ", pkg.name.as_bytes(), b"\n"]);
    write_chunks(out, &mut pos, max, &[b"Version: ", pkg.version.as_bytes(), b"\n"]);
    write_chunks(out, &mut pos, max, &[b"Desc:    ", pkg.desc.as_bytes(), b"\n"]);

    let size = pkg.elf.len();
    let mut nbuf = [0u8; 20];
    let slen = fmt_u64(&mut nbuf, size as u64);
    write_chunks(out, &mut pos, max, &[b"Size:    ", &nbuf[..slen], b" bytes\n"]);

    if pkg.deps.is_empty() {
        write_chunks(out, &mut pos, max, &[b"Deps:    (none)\n"]);
    } else {
        for (i, dep) in pkg.deps.iter().enumerate() {
            let label: &[u8] = if i == 0 { b"Deps:    " } else { b"         " };
            write_chunks(out, &mut pos, max, &[label, dep.as_bytes(), b"\n"]);
        }
    }

    let status: &[u8] = if crate::pkg::is_installed(idx) { b"installed" } else { b"not installed" };
    write_chunks(out, &mut pos, max, &[b"Status:  ", status, b"\n"]);

    pos as i64
}

/// 패키지를 설치 상태로 표시 (deps 포함).
fn sys_pkg_install(name_vaddr: u64) -> i64 {
    let name = match unsafe { read_cstr(name_vaddr) } { Some(s) => s, None => return EINVAL };
    let idx = match crate::pkg::find(name) { Some(i) => i, None => return ENOENT };
    // deps 먼저 설치
    for dep in crate::pkg::PACKAGES[idx].deps {
        if let Some(di) = crate::pkg::find(dep) {
            crate::pkg::install(di);
        }
    }
    crate::pkg::install(idx);
    crate::serial_println!("[mukg] installed: {}", name);
    0
}

/// 패키지를 설치 해제 상태로 표시.
fn sys_pkg_remove(name_vaddr: u64) -> i64 {
    let name = match unsafe { read_cstr(name_vaddr) } { Some(s) => s, None => return EINVAL };
    let idx = match crate::pkg::find(name) { Some(i) => i, None => return ENOENT };
    crate::pkg::remove(idx);
    crate::serial_println!("[mukg] removed: {}", name);
    0
}

/// 설치된 패키지 목록만 buf에 기록.
fn sys_pkg_installed(buf_vaddr: u64, max_len: u64) -> i64 {
    let max = max_len as usize;
    let out = buf_vaddr as *mut u8;
    let mut pos = 0usize;
    let mut found = false;
    for (i, pkg) in crate::pkg::PACKAGES.iter().enumerate() {
        if !crate::pkg::is_installed(i) { continue; }
        found = true;
        for s in &[b"[*] " as &[u8], pkg.name.as_bytes(), b" ", pkg.version.as_bytes(),
                   b" - ", pkg.desc.as_bytes(), b"\n"] {
            let copy = s.len().min(max.saturating_sub(pos));
            if copy == 0 { break; }
            unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), out.add(pos), copy); }
            pos += copy;
        }
        if pos >= max { break; }
    }
    if !found && max > 0 {
        let msg = b"(no packages installed)\n";
        let copy = msg.len().min(max);
        unsafe { core::ptr::copy_nonoverlapping(msg.as_ptr(), out, copy); }
        pos = copy;
    }
    pos as i64
}

/// u64 → 십진수 ASCII, 반환: 길이
fn fmt_u64(buf: &mut [u8; 20], mut n: u64) -> usize {
    if n == 0 { buf[0] = b'0'; return 1; }
    let mut tmp = [0u8; 20];
    let mut len = 0usize;
    while n > 0 {
        tmp[len] = b'0' + (n % 10) as u8;
        n /= 10;
        len += 1;
    }
    for i in 0..len { buf[i] = tmp[len - 1 - i]; }
    len
}

// ── name() ────────────────────────────────────────────────────────────────────

pub fn name(nr: u64) -> &'static str {
    match nr {
        0   => "read",          1   => "write",         2   => "open",
        3   => "close",         4   => "stat",           5   => "fstat",
        6   => "lstat",         7   => "poll",           8   => "lseek",
        9   => "mmap",          10  => "mprotect",       11  => "munmap",
        12  => "brk",           13  => "rt_sigaction",   14  => "rt_sigprocmask",
        15  => "rt_sigreturn",  16  => "ioctl",          17  => "pread64",
        18  => "pwrite64",      19  => "readv",          20  => "writev",
        21  => "access",        24  => "sched_yield",    25  => "mremap",
        26  => "msync",         27  => "mincore",        28  => "madvise",
        32  => "dup",           33  => "dup2",           35  => "nanosleep",
        39  => "getpid",        56  => "clone",          57  => "fork",           59  => "execve",
        60  => "exit",          61  => "wait4",          62  => "kill",
        63  => "uname",         72  => "fcntl",          79  => "getcwd",
        80  => "chdir",         81  => "fchdir",         83  => "mkdir",
        84  => "rmdir",         85  => "creat",          86  => "link",
        87  => "unlink",        88  => "symlink",        89  => "readlink",
        90  => "chmod",         91  => "fchmod",         92  => "chown",
        95  => "umask",         96  => "gettimeofday",   97  => "getrlimit",
        99  => "sysinfo",       102 => "getuid",         104 => "getgid",
        107 => "geteuid",       108 => "getegid",        110 => "getppid",
        111 => "getpgrp",       112 => "setsid",         135 => "personality",
        137 => "statfs",        138 => "fstatfs",        149 => "mlock",
        150 => "munlock",       151 => "mlockall",       152 => "munlockall",
        158 => "arch_prctl",    160 => "setrlimit",      162 => "sync",
        186 => "gettid",        202 => "futex",          217 => "getdents64",
        218 => "set_tid_addr",  228 => "clock_gettime",  229 => "clock_getres",
        231 => "exit_group",    234 => "tgkill",         257 => "openat",
        76  => "truncate",      77  => "ftruncate",      82  => "rename",
        258 => "mkdirat",       263 => "unlinkat",       264 => "renameat",
        265 => "linkat",        266 => "symlinkat",      316 => "renameat2",
        262 => "newfstatat",    273 => "set_robust_list",274 => "get_robust_list",
        302 => "prlimit64",     400 => "pkg_list",       401 => "pkg_getargs",
        402 => "ext4_read",     403 => "pkg_info",
        404 => "pkg_install",   405 => "pkg_remove",
        406 => "pkg_installed", 435 => "clone3",
        22  => "pipe",          23  => "select",
        41  => "socket",        42  => "connect",        43  => "accept",
        44  => "sendto",        45  => "recvfrom",       46  => "sendmsg",
        47  => "recvmsg",       48  => "shutdown",       49  => "bind",
        50  => "listen",        51  => "getsockname",    52  => "getpeername",
        54  => "setsockopt",    55  => "getsockopt",
        232 => "epoll_wait",    233 => "epoll_ctl",
        270 => "pselect6",      281 => "epoll_create",
        291 => "epoll_create1", 293 => "pipe2",
        _   => "?",
    }
}
