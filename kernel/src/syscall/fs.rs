//! BETA 2-1: 파일 I/O syscall — Linux x86-64 ABI, MuKernel VFS 내부 구현

use super::{read_cstr, EBADF, EINVAL, ENOENT, EPERM};

// ── stat 구조체 (Linux x86-64, 144 bytes) ────────────────────────────────────

#[repr(C)]
struct Stat {
    st_dev:      u64,
    st_ino:      u64,
    st_nlink:    u64,
    st_mode:     u32,
    st_uid:      u32,
    st_gid:      u32,
    _pad0:       u32,
    st_rdev:     u64,
    st_size:     i64,
    st_blksize:  i64,
    st_blocks:   i64,
    st_atime:    u64, st_atime_ns: u64,
    st_mtime:    u64, st_mtime_ns: u64,
    st_ctime:    u64, st_ctime_ns: u64,
    _unused:     [i64; 3],
}

const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFCHR: u32 = 0o020000;
const DEFAULT_FILE_PERM: u32 = 0o644;
const DEFAULT_DIR_PERM:  u32 = 0o755;

fn fill_stat(stat_ptr: u64, size: i64, is_dir: bool) {
    let s = unsafe { &mut *(stat_ptr as *mut Stat) };
    *s = Stat {
        st_dev: 1, st_ino: 1, st_nlink: 1,
        st_mode: if is_dir { S_IFDIR | DEFAULT_DIR_PERM }
                 else       { S_IFREG | DEFAULT_FILE_PERM },
        st_uid: 0, st_gid: 0, _pad0: 0, st_rdev: 0,
        st_size: size,
        st_blksize: 4096,
        st_blocks: (size + 511) / 512,
        st_atime: 0, st_atime_ns: 0,
        st_mtime: 0, st_mtime_ns: 0,
        st_ctime: 0, st_ctime_ns: 0,
        _unused: [0; 3],
    };
}

// ── iovec (readv/writev) ──────────────────────────────────────────────────────

#[repr(C)]
struct Iovec { iov_base: u64, iov_len: u64 }

// ── 전역 umask ────────────────────────────────────────────────────────────────

use core::sync::atomic::{AtomicU32, Ordering};
static UMASK: AtomicU32 = AtomicU32::new(0o022);

// ── 구현 ──────────────────────────────────────────────────────────────────────

/// 4: stat(path*, statbuf*)
pub fn sys_stat(path_vaddr: u64, stat_vaddr: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    // 디렉토리 판정
    if path == "/" || path.ends_with('/') {
        fill_stat(stat_vaddr, 0, true);
        return 0;
    }
    // ext4 파일 확인
    if let Some(data) = crate::vfs::ext4_read_file(path) {
        fill_stat(stat_vaddr, data.len() as i64, false);
        return 0;
    }
    // tmpfs 파일 확인
    if let Some(data) = crate::vfs::read_file(path) {
        fill_stat(stat_vaddr, data.len() as i64, false);
        return 0;
    }
    // 디렉토리 목록이 존재하면 디렉토리
    let entries = crate::vfs::list_dir(path);
    if !entries.is_empty() {
        fill_stat(stat_vaddr, 0, true);
        return 0;
    }
    ENOENT
}

/// 5: fstat(fd, statbuf*)  — fd=0/1/2 → 문자 디바이스
pub fn sys_fstat(fd: u64, stat_vaddr: u64) -> i64 {
    let s = unsafe { &mut *(stat_vaddr as *mut Stat) };
    *s = unsafe { core::mem::zeroed() };
    match fd {
        0 | 1 | 2 => {
            // stdin/stdout/stderr → 문자 디바이스
            s.st_mode = S_IFCHR | 0o666;
            s.st_nlink = 1;
            0
        }
        3 => { fill_stat(stat_vaddr, 0, false); 0 } // stub fd
        _ => EBADF,
    }
}

/// 6: lstat(path*, statbuf*) — 심볼릭 링크 없으므로 stat과 동일
pub fn sys_lstat(path_vaddr: u64, stat_vaddr: u64) -> i64 {
    sys_stat(path_vaddr, stat_vaddr)
}

/// 8: lseek(fd, offset, whence) → stub (항상 offset 반환)
pub fn sys_lseek(fd: u64, offset: u64, _whence: u64) -> i64 {
    let _ = fd;
    offset as i64
}

/// 17: pread64(fd, buf*, count, offset) → ext4 파일 읽기 (offset 지원)
pub fn sys_pread64(fd: u64, buf_vaddr: u64, count: u64, offset: u64) -> i64 {
    if fd != 3 { return EBADF; }
    // stub: offset 무시하고 처음부터 읽기 (fd=3은 마지막 open된 파일)
    let _ = offset;
    let _ = (buf_vaddr, count);
    0
}

/// 18: pwrite64(fd, buf*, count, offset)
pub fn sys_pwrite64(fd: u64, _buf: u64, count: u64, _offset: u64) -> i64 {
    if fd == 1 || fd == 2 { return count as i64; }
    EBADF
}

/// 19: readv(fd, iov*, iovcnt)
pub fn sys_readv(fd: u64, iov_vaddr: u64, iovcnt: u64) -> i64 {
    if fd != 0 { return EBADF; }
    let mut total = 0i64;
    for i in 0..iovcnt as usize {
        let iov = unsafe { &*((iov_vaddr as *const Iovec).add(i)) };
        if iov.iov_len == 0 { continue; }
        let byte = crate::kbd::read_key_blocking();
        unsafe { *(iov.iov_base as *mut u8) = byte; }
        total += 1;
        break; // 한 번에 1바이트만
    }
    total
}

/// 20: writev(fd, iov*, iovcnt) — stdout/stderr 출력
pub fn sys_writev(fd: u64, iov_vaddr: u64, iovcnt: u64) -> i64 {
    if fd != 1 && fd != 2 { return EBADF; }
    let mut total = 0i64;
    for i in 0..iovcnt as usize {
        let iov = unsafe { &*((iov_vaddr as *const Iovec).add(i)) };
        let bytes = unsafe {
            core::slice::from_raw_parts(iov.iov_base as *const u8, iov.iov_len as usize)
        };
        for &b in bytes { crate::serial::write_byte(b); }
        total += iov.iov_len as i64;
    }
    total
}

/// 21: access(path*, mode) → 존재하면 0, 없으면 ENOENT
pub fn sys_access(path_vaddr: u64, _mode: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    if path == "/" { return 0; }
    if crate::vfs::ext4_read_file(path).is_some() { return 0; }
    if crate::vfs::read_file(path).is_some() { return 0; }
    if !crate::vfs::list_dir(path).is_empty() { return 0; }
    ENOENT
}

/// 32: dup(oldfd)
pub fn sys_dup(oldfd: u64) -> i64 {
    match oldfd { 0 => 0, 1 => 1, 2 => 2, 3 => 3, _ => EBADF }
}

/// 33: dup2(oldfd, newfd)
pub fn sys_dup2(oldfd: u64, newfd: u64) -> i64 {
    if oldfd > 3 { return EBADF; }
    newfd as i64
}

/// 72: fcntl(fd, cmd, arg) — F_GETFL/F_SETFL만 처리
pub fn sys_fcntl(fd: u64, cmd: u64, _arg: u64) -> i64 {
    const F_GETFL: u64 = 3;
    const F_SETFL: u64 = 4;
    const F_GETFD: u64 = 1;
    const F_SETFD: u64 = 2;
    match (fd, cmd) {
        (_, F_GETFD)               => 0,       // FD_CLOEXEC 없음
        (_, F_SETFD)               => 0,
        (0, F_GETFL)               => 0,       // O_RDONLY
        (1 | 2, F_GETFL)           => 1,       // O_WRONLY
        (3, F_GETFL)               => 0,
        (_, F_SETFL)               => 0,
        _                          => EINVAL,
    }
}

/// 79: getcwd(buf*, size)
pub fn sys_getcwd(buf_vaddr: u64, size: u64) -> i64 {
    let cwd = b"/\0";
    if size < 2 { return EINVAL; }
    unsafe {
        core::ptr::copy_nonoverlapping(cwd.as_ptr(), buf_vaddr as *mut u8, cwd.len());
    }
    buf_vaddr as i64
}

/// 80: chdir(path*) → 항상 성공 (stub)
pub fn sys_chdir(_path_vaddr: u64) -> i64 { 0 }

/// 81: fchdir(fd) → 항상 성공 (stub)
pub fn sys_fchdir(_fd: u64) -> i64 { 0 }

/// 83: mkdir(path*, mode) — tmpfs에 디렉토리 생성
pub fn sys_mkdir(path_vaddr: u64, _mode: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    match crate::vfs::mkdir(path) {
        Some(_) => 0,
        None    => -17, // EEXIST
    }
}

/// 84: rmdir(path*) → stub 0
pub fn sys_rmdir(_path_vaddr: u64) -> i64 { 0 }

/// 85: creat(path*, mode) → tmpfs에 파일 생성
pub fn sys_creat(path_vaddr: u64, _mode: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    match crate::vfs::create_file(path) {
        Some(_) => 3, // stub fd
        None    => -17, // EEXIST
    }
}

/// 86: link(old*, new*) → EPERM (하드 링크 미지원)
pub fn sys_link(_old: u64, _new: u64) -> i64 { EPERM }

/// 87: unlink(path*) → stub 0
pub fn sys_unlink(_path_vaddr: u64) -> i64 { 0 }

/// 88: symlink(target*, linkpath*) → EPERM
pub fn sys_symlink(_target: u64, _linkpath: u64) -> i64 { EPERM }

/// 89: readlink(path*, buf*, size) → EINVAL (심볼릭 링크 없음)
pub fn sys_readlink(_path: u64, _buf: u64, _size: u64) -> i64 { EINVAL }

/// 90: chmod(path*, mode) → stub 0
pub fn sys_chmod(_path: u64, _mode: u64) -> i64 { 0 }

/// 91: fchmod(fd, mode) → stub 0
pub fn sys_fchmod(_fd: u64, _mode: u64) -> i64 { 0 }

/// 92: chown(path*, uid, gid) → stub 0
pub fn sys_chown(_path: u64, _uid: u64, _gid: u64) -> i64 { 0 }

/// 95: umask(mask) → 이전 umask 반환
pub fn sys_umask(mask: u64) -> i64 {
    let old = UMASK.swap(mask as u32 & 0o777, Ordering::Relaxed);
    old as i64
}

/// 257: openat(dirfd, path*, flags, mode)
///      AT_FDCWD(-100)이면 절대 경로처럼 처리
pub fn sys_openat(_dirfd: i64, path_vaddr: u64, flags: u64, _mode: u64) -> i64 {
    // flags O_CREAT(0x40) | O_TRUNC(0x200) 등 무시하고 읽기 전용처럼
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    if crate::vfs::ext4_read_file(path).is_some() { return 3; }
    if crate::vfs::read_file(path).is_some()      { return 3; }
    let _ = flags;
    ENOENT
}

/// 262: newfstatat(dirfd, path*, statbuf*, flags)
pub fn sys_newfstatat(dirfd: i64, path_vaddr: u64, stat_vaddr: u64, _flags: u64) -> i64 {
    let _ = dirfd;
    sys_stat(path_vaddr, stat_vaddr)
}

/// 302: prlimit64(pid, resource, new_limit*, old_limit*)
pub fn sys_prlimit64(_pid: u64, resource: u64, _new: u64, old_vaddr: u64) -> i64 {
    if old_vaddr != 0 {
        // RLIMIT_STACK(3): 8MB, 나머지: 무제한
        let (cur, max): (u64, u64) = if resource == 3 {
            (8 * 1024 * 1024, 8 * 1024 * 1024)
        } else {
            (u64::MAX, u64::MAX)
        };
        unsafe {
            *(old_vaddr as *mut u64) = cur;
            *((old_vaddr + 8) as *mut u64) = max;
        }
    }
    0
}
