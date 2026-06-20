/// Linux Compat — 시스템 콜 디스패치 (BETA 2-1)
///
/// ## ABI  (`int 0x80`)
/// RAX=nr  RDI=a1  RSI=a2  RDX=a3  R10=a4  R8=a5  R9=a6
///
/// ## isr128 스택 레이아웃 (push 순서, 낮은 주소 먼저)
/// ```text
/// [+0x00] r11   [+0x08] r10(a4)  [+0x10] r9(a6)  [+0x18] r8(a5)
/// [+0x20] rdi(a1) [+0x28] rsi(a2) [+0x30] rdx(a3) [+0x38] rcx
/// [+0x40] rax(nr)  [+0x48] RIP  [+0x50] CS  [+0x58] RFLAGS  [+0x60] RSP  [+0x68] SS
/// ```

// ── 서브모듈 ─────────────────────────────────────────────────────────────────

pub mod fs;
pub mod proc;
pub mod mem;
pub mod sysinfo;

// ── Linux x86-64 syscall 번호 상수 ──────────────────────────────────────────

pub const SYS_READ:    u64 = 0;
pub const SYS_WRITE:   u64 = 1;
pub const SYS_OPEN:    u64 = 2;
pub const SYS_CLOSE:   u64 = 3;
pub const SYS_STAT:    u64 = 4;
pub const SYS_FSTAT:   u64 = 5;
pub const SYS_LSTAT:   u64 = 6;
pub const SYS_LSEEK:   u64 = 8;
pub const SYS_MMAP:    u64 = 9;
pub const SYS_MPROTECT:u64 = 10;
pub const SYS_MUNMAP:  u64 = 11;
pub const SYS_BRK:     u64 = 12;
pub const SYS_IOCTL:   u64 = 16;
pub const SYS_PREAD64: u64 = 17;
pub const SYS_PWRITE64:u64 = 18;
pub const SYS_READV:   u64 = 19;
pub const SYS_WRITEV:  u64 = 20;
pub const SYS_ACCESS:  u64 = 21;
pub const SYS_SCHED_YIELD: u64 = 24;
pub const SYS_MREMAP:  u64 = 25;
pub const SYS_MSYNC:   u64 = 26;
pub const SYS_MINCORE: u64 = 27;
pub const SYS_MADVISE: u64 = 28;
pub const SYS_DUP:     u64 = 32;
pub const SYS_DUP2:    u64 = 33;
pub const SYS_NANOSLEEP: u64 = 35;
pub const SYS_GETPID:  u64 = 39;
pub const SYS_FORK:    u64 = 57;
pub const SYS_EXECVE:  u64 = 59;
pub const SYS_EXIT:    u64 = 60;
pub const SYS_WAITPID: u64 = 61;
pub const SYS_UNAME:   u64 = 63;
pub const SYS_FCNTL:   u64 = 72;
pub const SYS_GETCWD:  u64 = 79;
pub const SYS_CHDIR:   u64 = 80;
pub const SYS_FCHDIR:  u64 = 81;
pub const SYS_MKDIR:   u64 = 83;
pub const SYS_RMDIR:   u64 = 84;
pub const SYS_CREAT:   u64 = 85;
pub const SYS_LINK:    u64 = 86;
pub const SYS_UNLINK:  u64 = 87;
pub const SYS_SYMLINK: u64 = 88;
pub const SYS_READLINK:u64 = 89;
pub const SYS_CHMOD:   u64 = 90;
pub const SYS_FCHMOD:  u64 = 91;
pub const SYS_CHOWN:   u64 = 92;
pub const SYS_UMASK:   u64 = 95;
pub const SYS_GETTIMEOFDAY: u64 = 96;
pub const SYS_GETRLIMIT: u64 = 97;
pub const SYS_SYSINFO: u64 = 99;
pub const SYS_GETUID:  u64 = 102;
pub const SYS_GETGID:  u64 = 104;
pub const SYS_GETEUID: u64 = 107;
pub const SYS_GETEGID: u64 = 108;
pub const SYS_GETPPID: u64 = 110;
pub const SYS_GETPGRP: u64 = 111;
pub const SYS_SETSID:  u64 = 112;
pub const SYS_MLOCK:   u64 = 149;
pub const SYS_MUNLOCK: u64 = 150;
pub const SYS_MLOCKALL: u64 = 151;
pub const SYS_MUNLOCKALL: u64 = 152;
pub const SYS_PERSONALITY: u64 = 135;
pub const SYS_SETRLIMIT: u64 = 160;
pub const SYS_SYNC:    u64 = 162;
pub const SYS_ARCH_PRCTL: u64 = 158;
pub const SYS_GETTID:  u64 = 186;
pub const SYS_SET_TID_ADDR: u64 = 218;
pub const SYS_CLOCK_GETTIME: u64 = 228;
pub const SYS_CLOCK_GETRES:  u64 = 229;
pub const SYS_EXIT_GROUP: u64 = 231;
pub const SYS_TGKILL:  u64 = 234;
pub const SYS_OPENAT:  u64 = 257;
pub const SYS_NEWFSTATAT: u64 = 262;
pub const SYS_PRLIMIT64: u64 = 302;
pub const SYS_CLONE3:  u64 = 435;

// MuKernel 전용 (300번대 이후)
pub const SYS_PKG_LIST:    u64 = 200;
pub const SYS_PKG_GETARGS: u64 = 201;
pub const SYS_EXT4_READ:   u64 = 202;

// ── errno (음수 반환값) — pub(crate)으로 서브모듈에서 사용 ──────────────────

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

// ── 유틸 ─────────────────────────────────────────────────────────────────────

/// 유저 공간 가상 주소에서 null-terminated C 문자열 읽기.
pub(crate) unsafe fn read_cstr(vaddr: u64) -> Option<&'static str> {
    if vaddr == 0 { return None; }
    let ptr = vaddr as *const u8;
    let mut len = 0usize;
    while *ptr.add(len) != 0 && len < 4096 { len += 1; }
    core::str::from_utf8(core::slice::from_raw_parts(ptr, len)).ok()
}

/// sys_exit / sys_exit_group 공통 구현 (longjmp, noreturn)
pub(crate) fn sys_exit_impl(code: u64) -> ! {
    use core::sync::atomic::Ordering;
    crate::interrupts::handlers::USER_EXIT_CODE.store(code as i32, Ordering::Relaxed);
    crate::serial_println!("[syscall] exit({}) — longjmp to kernel", code);
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

// ── 메인 디스패처 ────────────────────────────────────────────────────────────

pub fn dispatch(frame: *const u64) -> i64 {
    let (nr, a1, a2, a3, a4, a5, a6) = unsafe {
        let nr = *frame.add(8); // rax
        let a1 = *frame.add(4); // rdi
        let a2 = *frame.add(5); // rsi
        let a3 = *frame.add(6); // rdx
        let a4 = *frame.add(1); // r10
        let a5 = *frame.add(3); // r8
        let a6 = *frame.add(2); // r9
        (nr, a1, a2, a3, a4, a5, a6)
    };

    crate::serial_println!(
        "[syscall] nr={:<3} ({})  a1={:#x} a2={:#x} a3={:#x}",
        nr, name(nr), a1, a2, a3
    );

    let ret: i64 = match nr {
        // ── 기존 구현 ──────────────────────────────────────────────────────
        SYS_READ    => sys_read(a1, a2, a3),
        SYS_WRITE   => sys_write(a1, a2, a3),
        SYS_OPEN    => sys_open(a1, a2),
        SYS_CLOSE   => { let _ = a1; 0 },
        SYS_MMAP    => sys_mmap(a1, a2, a3, a4, a5 as i64, a6),
        SYS_MUNMAP  => { 0 },
        SYS_BRK     => ENOMEM,
        SYS_IOCTL   => ENOTTY,
        SYS_GETPID  => crate::process::scheduler::current_pid() as i64,
        SYS_FORK    => {
            let child = crate::process::scheduler::alloc_pid();
            child as i64
        }
        SYS_EXECVE  => sys_execve(a1, a2),
        SYS_EXIT    => sys_exit_impl(a1),
        SYS_WAITPID => -10, // ECHILD
        SYS_PKG_LIST    => sys_pkg_list(a1, a2),
        SYS_PKG_GETARGS => sys_pkg_getargs(a1, a2),
        SYS_EXT4_READ   => sys_ext4_read(a1, a2, a3),

        // ── BETA 2-1: fs.rs ────────────────────────────────────────────────
        SYS_STAT       => fs::sys_stat(a1, a2),
        SYS_FSTAT      => fs::sys_fstat(a1, a2),
        SYS_LSTAT      => fs::sys_lstat(a1, a2),
        SYS_LSEEK      => fs::sys_lseek(a1, a2, a3),
        SYS_PREAD64    => fs::sys_pread64(a1, a2, a3, a4),
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
        SYS_PRLIMIT64  => fs::sys_prlimit64(a1, a2, a3, a4),

        // ── BETA 2-1: proc.rs ──────────────────────────────────────────────
        SYS_SCHED_YIELD  => proc::sys_sched_yield(),
        SYS_NANOSLEEP    => proc::sys_nanosleep(a1, a2),
        SYS_UNAME        => proc::sys_uname(a1),
        SYS_GETUID       => proc::sys_getuid(),
        SYS_GETGID       => proc::sys_getgid(),
        SYS_GETEUID      => proc::sys_geteuid(),
        SYS_GETEGID      => proc::sys_getegid(),
        SYS_GETPPID      => proc::sys_getppid(),
        SYS_GETPGRP      => proc::sys_getpgrp(),
        SYS_SETSID       => proc::sys_setsid(),
        SYS_PERSONALITY  => proc::sys_personality(a1),
        SYS_ARCH_PRCTL   => proc::sys_arch_prctl(a1, a2),
        SYS_SETRLIMIT    => proc::sys_setrlimit(a1, a2),
        SYS_SYNC         => proc::sys_sync(),
        SYS_GETTID       => proc::sys_gettid(),
        SYS_SET_TID_ADDR => proc::sys_set_tid_address(a1),
        SYS_EXIT_GROUP   => proc::sys_exit_group(a1),
        SYS_TGKILL       => sysinfo::sys_tgkill(a1, a2, a3),
        SYS_CLONE3       => sysinfo::sys_clone3(a1, a2),

        // ── BETA 2-1: mem.rs ───────────────────────────────────────────────
        SYS_MPROTECT  => mem::sys_mprotect(a1, a2, a3),
        SYS_MREMAP    => mem::sys_mremap(a1, a2, a3, a4),
        SYS_MSYNC     => mem::sys_msync(a1, a2, a3),
        SYS_MINCORE   => mem::sys_mincore(a1, a2, a3),
        SYS_MADVISE   => mem::sys_madvise(a1, a2, a3),
        SYS_MLOCK     => mem::sys_mlock(a1, a2),
        SYS_MUNLOCK   => mem::sys_munlock(a1, a2),
        SYS_MLOCKALL  => mem::sys_mlockall(a1),
        SYS_MUNLOCKALL => mem::sys_munlockall(),

        // ── BETA 2-1: sysinfo.rs ───────────────────────────────────────────
        SYS_GETTIMEOFDAY => sysinfo::sys_gettimeofday(a1, a2),
        SYS_GETRLIMIT    => sysinfo::sys_getrlimit(a1, a2),
        SYS_SYSINFO      => sysinfo::sys_sysinfo(a1),
        SYS_CLOCK_GETTIME => sysinfo::sys_clock_gettime(a1, a2),
        SYS_CLOCK_GETRES  => sysinfo::sys_clock_getres(a1, a2),

        _ => {
            crate::serial_println!("[syscall] ENOSYS nr={}", nr);
            ENOSYS
        }
    };

    crate::serial_println!("[syscall] → {}", ret);
    ret
}

// ── 기존 구현 함수들 (SYS_READ, SYS_WRITE, ...) ─────────────────────────────

fn sys_write(fd: u64, buf: u64, count: u64) -> i64 {
    match fd {
        1 | 2 => {
            let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, count as usize) };
            for &b in bytes { crate::serial::write_byte(b); }
            count as i64
        }
        _ => EBADF,
    }
}

fn sys_read(fd: u64, buf: u64, count: u64) -> i64 {
    match fd {
        0 => {
            if count == 0 { return 0; }
            let byte = crate::kbd::read_key_blocking();
            unsafe { *(buf as *mut u8) = byte; }
            1
        }
        _ => EAGAIN,
    }
}

fn sys_open(path_vaddr: u64, _flags: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    if crate::vfs::ext4_read_file(path).is_some() { return 3; }
    if crate::vfs::read_file(path).is_some()      { return 3; }
    ENOENT
}

fn sys_mmap(addr: u64, len: u64, _prot: u64, flags: u64, fd: i64, _off: u64) -> i64 {
    const MAP_ANONYMOUS: u64 = 0x20;
    if flags & MAP_ANONYMOUS == 0 || fd != -1 { return EINVAL; }
    let pages = (len as usize + 4095) / 4096;
    let layout = alloc::alloc::Layout::from_size_align(pages * 4096, 4096)
        .unwrap_or_else(|_| panic!("bad mmap layout"));
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if ptr.is_null() { ENOMEM } else { let _ = addr; ptr as i64 }
}

fn sys_execve(path_vaddr: u64, argv_vaddr: u64) -> i64 {
    use core::sync::atomic::Ordering;
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    let name = path.rsplit('/').next().unwrap_or(path);
    let idx = match crate::pkg::find(name) {
        Some(i) => i,
        None => { crate::serial_println!("[mukg] not found: {:?}", name); return ENOENT; }
    };
    let args = if argv_vaddr != 0 {
        unsafe { read_cstr(argv_vaddr) }.unwrap_or("").as_bytes()
    } else { b"" };
    crate::pkg::set_args(args);
    crate::interrupts::handlers::PENDING_PKG_IDX.store(idx as i32, Ordering::Relaxed);
    crate::serial_println!("[mukg] exec {:?}", name);
    sys_exit_impl(0) as i64
}

fn sys_pkg_list(buf_vaddr: u64, max_len: u64) -> i64 {
    let max = max_len as usize;
    let out = buf_vaddr as *mut u8;
    let mut pos = 0usize;
    for pkg in crate::pkg::PACKAGES {
        for s in &[pkg.name.as_bytes(), b" ", pkg.version.as_bytes(),
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

// ── name() — 로깅용 syscall 이름 ─────────────────────────────────────────────

pub fn name(nr: u64) -> &'static str {
    match nr {
        0  => "read",       1  => "write",      2  => "open",
        3  => "close",      4  => "stat",        5  => "fstat",
        6  => "lstat",      8  => "lseek",       9  => "mmap",
        10 => "mprotect",   11 => "munmap",      12 => "brk",
        16 => "ioctl",      17 => "pread64",     18 => "pwrite64",
        19 => "readv",      20 => "writev",      21 => "access",
        24 => "sched_yield",25 => "mremap",      26 => "msync",
        27 => "mincore",    28 => "madvise",     32 => "dup",
        33 => "dup2",       35 => "nanosleep",   39 => "getpid",
        57 => "fork",       59 => "execve",      60 => "exit",
        61 => "wait4",      63 => "uname",       72 => "fcntl",
        79 => "getcwd",     80 => "chdir",       81 => "fchdir",
        83 => "mkdir",      84 => "rmdir",       85 => "creat",
        86 => "link",       87 => "unlink",      88 => "symlink",
        89 => "readlink",   90 => "chmod",       91 => "fchmod",
        92 => "chown",      95 => "umask",       96 => "gettimeofday",
        97 => "getrlimit",  99 => "sysinfo",    102 => "getuid",
       104 => "getgid",    107 => "geteuid",    108 => "getegid",
       110 => "getppid",   111 => "getpgrp",    112 => "setsid",
       135 => "personality",149 => "mlock",     150 => "munlock",
       151 => "mlockall",  152 => "munlockall", 158 => "arch_prctl",
       160 => "setrlimit", 162 => "sync",       186 => "gettid",
       200 => "pkg_list",  201 => "pkg_getargs",202 => "ext4_read",
       218 => "set_tid_addr",228 => "clock_gettime",229 => "clock_getres",
       231 => "exit_group",234 => "tgkill",     257 => "openat",
       262 => "newfstatat",302 => "prlimit64",  435 => "clone3",
        _  => "?",
    }
}
