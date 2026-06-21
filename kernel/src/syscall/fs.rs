//! BETA 2-1: 파일 I/O syscall — Linux x86-64 ABI, MuKernel VFS 내부 구현

use super::{read_cstr, EBADF, EINVAL, ENOENT, EPERM, FD_DEV_NULL};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// ── 마지막 open된 경로 추적 (getdents64용) ───────────────────────────────────

static mut LAST_OPEN_BUF: [u8; 256] = [0u8; 256];
static     LAST_OPEN_LEN: AtomicUsize = AtomicUsize::new(0);
static     LAST_OPEN_DIR: AtomicBool  = AtomicBool::new(false);

pub(crate) fn set_last_open(path: &str, is_dir: bool) {
    let bytes = path.as_bytes();
    let n = bytes.len().min(255);
    unsafe { LAST_OPEN_BUF[..n].copy_from_slice(&bytes[..n]); }
    LAST_OPEN_LEN.store(n, Ordering::Relaxed);
    LAST_OPEN_DIR.store(is_dir, Ordering::Relaxed);
}

fn last_open_path() -> &'static str {
    let n = LAST_OPEN_LEN.load(Ordering::Relaxed);
    if n == 0 { return "/"; }
    core::str::from_utf8(unsafe { &LAST_OPEN_BUF[..n] }).unwrap_or("/")
}

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

pub fn fill_stat_pub(stat_ptr: u64, size: i64, is_dir: bool) { fill_stat(stat_ptr, size, is_dir); }
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

use core::sync::atomic::AtomicU32;
static UMASK: AtomicU32 = AtomicU32::new(0o022);

// ── 구현 ──────────────────────────────────────────────────────────────────────

/// 4: stat(path*, statbuf*) — BETA 13: stat_any 통합
pub fn sys_stat(path_vaddr: u64, stat_vaddr: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    if path == "/" || path.ends_with('/') {
        fill_stat(stat_vaddr, 0, true);
        return 0;
    }
    if let Some((ino, mode, uid, gid, nlink, size)) = crate::vfs::stat_any(path) {
        let s = unsafe { &mut *(stat_vaddr as *mut Stat) };
        *s = Stat {
            st_dev: 1, st_ino: ino, st_nlink: nlink as u64,
            st_mode: mode, st_uid: uid, st_gid: gid, _pad0: 0, st_rdev: 0,
            st_size: size, st_blksize: 4096,
            st_blocks: (size + 511) / 512,
            st_atime: 0, st_atime_ns: 0,
            st_mtime: 0, st_mtime_ns: 0,
            st_ctime: 0, st_ctime_ns: 0,
            _unused: [0; 3],
        };
        let _ = (ino, uid, gid, nlink);
        return 0;
    }
    ENOENT
}

/// 5: fstat(fd, statbuf*)
pub fn sys_fstat(fd: u64, stat_vaddr: u64) -> i64 {
    let s = unsafe { &mut *(stat_vaddr as *mut Stat) };
    *s = unsafe { core::mem::zeroed() };
    match fd {
        0 | 1 | 2 | FD_DEV_NULL => {
            s.st_mode = S_IFCHR | 0o666;
            s.st_nlink = 1;
            0
        }
        3 => {
            if LAST_OPEN_DIR.load(Ordering::Relaxed) {
                fill_stat(stat_vaddr, 0, true);
            } else {
                fill_stat(stat_vaddr, 0, false);
            }
            0
        }
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

/// 18: pwrite64(fd, buf*, count, offset) — BETA 13: fd≥3 지원
pub fn sys_pwrite64(fd: u64, buf: u64, count: u64, offset: u64) -> i64 {
    if fd == 1 || fd == 2 {
        // stdout/stderr: offset 무시하고 그냥 출력
        let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, count as usize) };
        for &b in bytes { crate::serial::write_byte(b); }
        crate::term::write_bytes(bytes);
        return count as i64;
    }
    if fd >= 3 {
        // fd≥3: seek → write → seek back (pwrite semantics via seek)
        let old_pos = crate::syscall::fd::seek(fd as u32, 0, 1); // SEEK_CUR
        if old_pos < 0 { return old_pos; }
        let r = crate::syscall::fd::seek(fd as u32, offset as i64, 0); // SEEK_SET
        if r < 0 { return r; }
        let n = crate::syscall::fd::write_fd(fd as u32, buf as *const u8, count as usize);
        crate::syscall::fd::seek(fd as u32, old_pos, 0);
        return n;
    }
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

/// 20: writev(fd, iov*, iovcnt) — stdout/stderr 출력 또는 파일 쓰기
pub fn sys_writev(fd: u64, iov_vaddr: u64, iovcnt: u64) -> i64 {
    let mut total = 0i64;
    for i in 0..iovcnt as usize {
        let iov = unsafe { &*((iov_vaddr as *const Iovec).add(i)) };
        if iov.iov_len == 0 { continue; }
        let bytes = unsafe {
            core::slice::from_raw_parts(iov.iov_base as *const u8, iov.iov_len as usize)
        };
        let n = match fd {
            1 | 2 => {
                for &b in bytes { crate::serial::write_byte(b); }
                crate::term::write_bytes(bytes);
                iov.iov_len as i64
            }
            f if f >= 3 => crate::syscall::fd::write_fd(f as u32, bytes.as_ptr(), bytes.len()),
            _ => return EBADF,
        };
        if n < 0 { return n; }
        total += n;
    }
    total
}

/// 21: access(path*, mode) — BETA 13: exists_any 통합
pub fn sys_access(path_vaddr: u64, _mode: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    if crate::vfs::exists_any(path) { 0 } else { ENOENT }
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

/// 79: getcwd(buf*, size) — 실제 CWD 반환 (BETA 12).
pub fn sys_getcwd(buf_vaddr: u64, size: u64) -> i64 {
    let cwd = crate::process::userproc::current_cwd();
    let bytes = cwd.as_bytes();
    let need = bytes.len() + 1;
    if (size as usize) < need { return EINVAL; }
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf_vaddr as *mut u8, bytes.len());
        *((buf_vaddr + bytes.len() as u64) as *mut u8) = 0;
    }
    buf_vaddr as i64
}

/// 80: chdir(path*) — BETA 13: exists_any 통합
pub fn sys_chdir(path_vaddr: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    let normalized = if path != "/" { path.trim_end_matches('/') } else { path };
    if !crate::vfs::exists_any(normalized) { return ENOENT; }
    crate::process::userproc::set_cwd(normalized);
    0
}

/// 81: fchdir(fd).
pub fn sys_fchdir(fd: u64) -> i64 {
    if let Some(path) = crate::syscall::fd::dir_path(fd as u32) {
        crate::process::userproc::set_cwd(&path);
        return 0;
    }
    if let Some(path) = crate::syscall::fd::file_path(fd as u32) {
        crate::process::userproc::set_cwd(&path);
        return 0;
    }
    EBADF
}

/// 83: mkdir(path*, mode) — tmpfs에 디렉토리 생성
pub fn sys_mkdir(path_vaddr: u64, _mode: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    match crate::vfs::mkdir(path) {
        Some(_) => 0,
        None    => -17, // EEXIST
    }
}

/// 84: rmdir(path*) — 실제 VFS 디렉토리 제거 (BETA 12).
pub fn sys_rmdir(path_vaddr: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    crate::vfs::remove_entry(path, true)
}

/// 85: creat(path*, mode) → tmpfs에 파일 생성
pub fn sys_creat(path_vaddr: u64, _mode: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    match crate::vfs::create_file(path) {
        Some(_) => 3, // stub fd
        None    => -17, // EEXIST
    }
}

/// 76: truncate(path*, length) — BETA 12.
pub fn sys_truncate(path_vaddr: u64, length: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    crate::vfs::truncate_vfs(path, length as usize)
}

/// 77: ftruncate(fd, length) — BETA 12.
pub fn sys_ftruncate(fd: u64, length: u64) -> i64 {
    crate::syscall::fd::truncate_fd(fd as u32, length as usize)
}

/// 82: rename(old*, new*) — BETA 12.
pub fn sys_rename(old_vaddr: u64, new_vaddr: u64) -> i64 {
    let old = match unsafe { read_cstr(old_vaddr) } { Some(p) => p, None => return EINVAL };
    let new = match unsafe { read_cstr(new_vaddr) } { Some(p) => p, None => return EINVAL };
    crate::vfs::rename(old, new)
}

/// 86: link(old*, new*) — BETA 12: 하드 링크.
pub fn sys_link(old_vaddr: u64, new_vaddr: u64) -> i64 {
    let old = match unsafe { read_cstr(old_vaddr) } { Some(p) => p, None => return EINVAL };
    let new = match unsafe { read_cstr(new_vaddr) } { Some(p) => p, None => return EINVAL };
    crate::vfs::link_vfs(old, new)
}

/// 87: unlink(path*) — BETA 12: 실제 파일 제거.
pub fn sys_unlink(path_vaddr: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    crate::vfs::remove_entry(path, false)
}

/// 88: symlink(target*, linkpath*) → EPERM (심볼릭 링크 미지원)
pub fn sys_symlink(_target: u64, _linkpath: u64) -> i64 { EPERM }

/// 89: readlink(path*, buf*, size) — BETA 12: /proc 경로 지원.
pub fn sys_readlink(path_vaddr: u64, buf_vaddr: u64, size: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    if let Some(target) = crate::vfs::procfs::readlink(path) {
        let n = target.len().min(size as usize);
        unsafe { core::ptr::copy_nonoverlapping(target.as_ptr(), buf_vaddr as *mut u8, n); }
        return n as i64;
    }
    EINVAL
}

/// 90: chmod(path*, mode) — BETA 12.
pub fn sys_chmod(path_vaddr: u64, mode: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    crate::vfs::chmod_vfs(path, mode as u32)
}

/// 91: fchmod(fd, mode) → stub 0
pub fn sys_fchmod(_fd: u64, _mode: u64) -> i64 { 0 }

/// 92: chown(path*, uid, gid) — BETA 12.
pub fn sys_chown(path_vaddr: u64, uid: u64, gid: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };
    crate::vfs::chown_vfs(path, uid as u32, gid as u32)
}

/// 95: umask(mask) → 이전 umask 반환
pub fn sys_umask(mask: u64) -> i64 {
    let old = UMASK.swap(mask as u32 & 0o777, Ordering::Relaxed);
    old as i64
}

/// 257: openat(dirfd, path*, flags, mode) — BETA 13: copy-up overlay
pub fn sys_openat(_dirfd: i64, path_vaddr: u64, flags: u64, _mode: u64) -> i64 {
    sys_open_impl(path_vaddr, flags)
}

/// 공통 파일 오픈 로직 (BETA 13: copy-up + 통합 경로).
pub(crate) fn sys_open_impl(path_vaddr: u64, flags: u64) -> i64 {
    let path = match unsafe { read_cstr(path_vaddr) } { Some(p) => p, None => return EINVAL };

    // /dev/* 가상 디바이스 (BETA 14)
    if crate::syscall::dev::is_dev(path) {
        // 디렉토리
        if crate::syscall::dev::is_dir(path) {
            return crate::syscall::fd::open_dir(path) as i64;
        }
        // null/zero는 기존 stub fd 유지 (read/write에서 빠른 경로)
        if path == "/dev/null" || path == "/dev/zero" {
            return super::FD_DEV_NULL as i64;
        }
        return match crate::syscall::dev::open(path) {
            Some(fd) => fd as i64,
            None     => super::ENOENT,
        };
    }

    let o_dir    = flags & 0o200000 != 0; // O_DIRECTORY
    let o_creat  = flags & 0x40  != 0;    // O_CREAT
    let o_trunc  = flags & 0x200 != 0;    // O_TRUNC
    let writable = flags & 0x03  != 0;    // O_WRONLY | O_RDWR

    // 디렉토리 오픈 요청
    if o_dir || path == "/" || path.ends_with('/') {
        return if crate::vfs::exists_any(path) {
            crate::syscall::fd::open_dir(path) as i64
        } else { ENOENT };
    }

    // /proc 가상 FS (항상 읽기 전용)
    if crate::vfs::procfs::is_proc(path) {
        if let Some(data) = crate::vfs::procfs::read(path) {
            return crate::syscall::fd::open_file(path, data, false) as i64;
        }
        if crate::vfs::procfs::exists(path) {
            return crate::syscall::fd::open_dir(path) as i64;
        }
        return ENOENT;
    }

    // 쓰기 모드: open_writable (tmpfs 우선 → ext4 copy-up → O_CREAT)
    if writable || o_creat {
        if let Some(node) = crate::vfs::open_writable(path, o_creat) {
            if o_trunc { crate::vfs::truncate_node(&node, 0); }
            return crate::syscall::fd::open_file_writable(path, node) as i64;
        }
        return ENOENT;
    }

    // 읽기 전용: tmpfs → ext4
    if let Some(data) = crate::vfs::open_readonly(path) {
        return crate::syscall::fd::open_file(path, data, false) as i64;
    }

    // 디렉토리
    if crate::vfs::is_dir(path) {
        return crate::syscall::fd::open_dir(path) as i64;
    }

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
        let (cur, max): (u64, u64) = if resource == 3 {
            (8 * 1024 * 1024, 8 * 1024 * 1024) // RLIMIT_STACK = 8MB
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

// ── getdents64 ────────────────────────────────────────────────────────────────

/// linux_dirent64 구조체 (가변 길이 이름 포함)
/// d_reclen은 전체 크기이고 8바이트 정렬.
#[repr(C, packed)]
struct Dirent64Hdr {
    d_ino:    u64,
    d_off:    i64,
    d_reclen: u16,
    d_type:   u8,
    // d_name: [u8] 가변 길이 — hdr 바로 뒤에 위치
}

const DT_UNKNOWN: u8 = 0;
const DT_REG:     u8 = 8;
const DT_DIR:     u8 = 4;

/// 217: getdents64(fd, buf*, count) → 읽은 바이트 수
///
/// fd=3으로 열린 마지막 경로의 VFS 목록을 반환.
/// 실제 fd→path 테이블이 없으므로 마지막 open 경로를 전역으로 추적.
pub fn sys_getdents64(fd: u64, buf_vaddr: u64, count: u64) -> i64 {
    if fd == 0 || fd == 1 || fd == 2 { return EBADF; }
    if buf_vaddr == 0 { return EFAULT; }
    if count < 32 { return EINVAL; }

    // BETA 4: HandleTable의 DirResource에서 경로 조회
    // 없으면 기존 LAST_OPEN_BUF 폴백
    let dir_path_owned;
    let path: &str = if let Some(p) = crate::syscall::fd::dir_path(fd as u32) {
        dir_path_owned = p;
        &dir_path_owned
    } else if LAST_OPEN_DIR.load(Ordering::Relaxed) {
        last_open_path()
    } else {
        return 0;
    };

    // VFS 목록 수집 (dev / proc / tmpfs / ext4)
    let entries: alloc::vec::Vec<(alloc::string::String, bool)> =
        if crate::syscall::dev::is_dev(path) {
            crate::syscall::dev::list(path)
        } else if crate::vfs::procfs::is_proc(path) {
            crate::vfs::procfs::list(path)
        } else {
            let mut v: alloc::vec::Vec<(alloc::string::String, bool)> =
                crate::vfs::list_dir(path)
                    .into_iter()
                    .map(|e| (e.name, e.is_dir))
                    .collect();
            for e in crate::vfs::ext4_list_dir(path) {
                if !v.iter().any(|(n, _)| n == &e.name) {
                    v.push((e.name, e.is_dir));
                }
            }
            v
        };

    let buf = buf_vaddr as *mut u8;
    let max = count as usize;
    let mut pos = 0usize;
    let mut ino: u64 = 2;

    for (name, is_dir) in &entries {
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len();
        // reclen = sizeof(Dirent64Hdr) + name_len + 1(NUL), 8-byte aligned
        let raw_len = core::mem::size_of::<Dirent64Hdr>() + name_len + 1;
        let rec_len = (raw_len + 7) & !7;

        if pos + rec_len > max { break; }

        let hdr = unsafe { &mut *(buf.add(pos) as *mut Dirent64Hdr) };
        hdr.d_ino    = ino;
        hdr.d_off    = (pos + rec_len) as i64;
        hdr.d_reclen = rec_len as u16;
        hdr.d_type   = if *is_dir { DT_DIR } else { DT_REG };

        // 이름 복사 (hdr 바로 뒤)
        let name_dst = unsafe { buf.add(pos + core::mem::size_of::<Dirent64Hdr>()) };
        unsafe {
            core::ptr::copy_nonoverlapping(name_bytes.as_ptr(), name_dst, name_len);
            *name_dst.add(name_len) = 0; // NUL
            // 패딩 바이트 0으로 초기화
            if rec_len > raw_len {
                core::ptr::write_bytes(name_dst.add(name_len + 1), 0, rec_len - raw_len);
            }
        }

        pos += rec_len;
        ino += 1;
    }

    pos as i64
}

// ── statfs ────────────────────────────────────────────────────────────────────

/// struct statfs (Linux x86-64, 120 bytes)
#[repr(C)]
struct Statfs {
    f_type:    i64,
    f_bsize:   i64,
    f_blocks:  u64,
    f_bfree:   u64,
    f_bavail:  u64,
    f_files:   u64,
    f_ffree:   u64,
    f_fsid:    [i32; 2],
    f_namelen: i64,
    f_frsize:  i64,
    f_flags:   i64,
    f_spare:   [i64; 4],
}

fn fill_statfs(ptr: u64) {
    let s = unsafe { &mut *(ptr as *mut Statfs) };
    *s = Statfs {
        f_type:    0xEF53,   // EXT4_SUPER_MAGIC
        f_bsize:   4096,
        f_blocks:  65536,
        f_bfree:   32768,
        f_bavail:  32768,
        f_files:   1024,
        f_ffree:   512,
        f_fsid:    [0; 2],
        f_namelen: 255,
        f_frsize:  4096,
        f_flags:   0,
        f_spare:   [0; 4],
    };
}

/// 137: statfs(path*, statfs*)
pub fn sys_statfs(_path_vaddr: u64, buf_vaddr: u64) -> i64 {
    if buf_vaddr == 0 { return EFAULT; }
    fill_statfs(buf_vaddr);
    0
}

/// 138: fstatfs(fd, statfs*)
pub fn sys_fstatfs(_fd: u64, buf_vaddr: u64) -> i64 {
    if buf_vaddr == 0 { return EFAULT; }
    fill_statfs(buf_vaddr);
    0
}

// ── BETA 12: *at 변형 syscall ─────────────────────────────────────────────────

const AT_FDCWD: i64 = -100;

/// dirfd + path 조합으로 절대 경로 생성 (AT_FDCWD는 현재 CWD 사용)
fn resolve_at(dirfd: i64, path_vaddr: u64) -> Option<alloc::string::String> {
    let path = unsafe { read_cstr(path_vaddr) }?;
    if path.starts_with('/') {
        return Some(alloc::string::String::from(path));
    }
    let base = if dirfd == AT_FDCWD {
        crate::process::userproc::current_cwd()
    } else {
        crate::syscall::fd::dir_path(dirfd as u32)
            .or_else(|| crate::syscall::fd::file_path(dirfd as u32))?
    };
    let sep = if base.ends_with('/') { "" } else { "/" };
    Some(alloc::format!("{}{}{}", base, sep, path))
}

/// 258: mkdirat(dirfd, path*, mode)
pub fn sys_mkdirat(dirfd: i64, path_vaddr: u64, _mode: u64) -> i64 {
    let full = match resolve_at(dirfd, path_vaddr) { Some(p) => p, None => return EINVAL };
    match crate::vfs::mkdir(&full) { Some(_) => 0, None => -17 }
}

/// 263: unlinkat(dirfd, path*, flags)
pub fn sys_unlinkat(dirfd: i64, path_vaddr: u64, flags: u64) -> i64 {
    let full = match resolve_at(dirfd, path_vaddr) { Some(p) => p, None => return EINVAL };
    let allow_dir = flags & 0x200 != 0; // AT_REMOVEDIR
    crate::vfs::remove_entry(&full, allow_dir)
}

/// 264/316: renameat(olddirfd, oldpath*, newdirfd, newpath*)
pub fn sys_renameat(olddirfd: i64, old_vaddr: u64, newdirfd: i64, new_vaddr: u64) -> i64 {
    let old = match resolve_at(olddirfd, old_vaddr) { Some(p) => p, None => return EINVAL };
    let new = match resolve_at(newdirfd, new_vaddr) { Some(p) => p, None => return EINVAL };
    crate::vfs::rename(&old, &new)
}

/// 265: linkat(olddirfd, oldpath*, newdirfd, newpath*, flags)
pub fn sys_linkat(olddirfd: i64, old_vaddr: u64, newdirfd: i64, new_vaddr: u64, _flags: u64) -> i64 {
    let old = match resolve_at(olddirfd, old_vaddr) { Some(p) => p, None => return EINVAL };
    let new = match resolve_at(newdirfd, new_vaddr) { Some(p) => p, None => return EINVAL };
    crate::vfs::link_vfs(&old, &new)
}

/// 266: symlinkat(target*, newdirfd, linkpath*) → EPERM (미지원)
pub fn sys_symlinkat(_target_vaddr: u64, _newdirfd: i64, _linkpath_vaddr: u64) -> i64 { EPERM }

use super::EFAULT;
use alloc;
