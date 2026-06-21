//! VFS (Virtual File System) — BETA 12: 완전한 쓰기 가능 tmpfs
//!
//! ## 핵심 변경 (BETA 12)
//! - `VfsMeta`: inode 번호 / mode / uid / gid / nlink 추적
//! - `FileNode`: 파일 데이터(Vec<u8>) + 메타데이터 묶음 (fd.rs가 NodeRef로 직접 접근)
//! - `VfsNode::File(FileNode)` / `VfsNode::Dir { dir, meta }`
//! - 신규 연산: `unlink`, `rmdir_vfs`, `rename`, `link_vfs`, `truncate_vfs`
//! - `init()`: /tmp /var /run /etc /proc 등 표준 디렉토리 자동 생성

pub mod ext4fs;
pub mod procfs;
pub mod tmpfs;

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use tmpfs::TmpfsDir;

// ── inode 번호 카운터 ─────────────────────────────────────────────────────────

static INO_SEQ: AtomicU64 = AtomicU64::new(2); // 1은 루트
fn alloc_ino() -> u64 { INO_SEQ.fetch_add(1, Ordering::Relaxed) }

// ── 공개 타입 ────────────────────────────────────────────────────────────────

/// VFS 노드의 공유 가변 참조 (`Arc<Mutex<VfsNode>>`).
pub type NodeRef = Arc<Mutex<VfsNode>>;

/// per-inode 메타데이터.
#[derive(Clone, Copy)]
pub struct VfsMeta {
    pub ino:   u64,
    pub mode:  u32,
    pub uid:   u32,
    pub gid:   u32,
    pub nlink: u32,
}

impl VfsMeta {
    pub fn new_file(mode: u32) -> Self {
        VfsMeta { ino: alloc_ino(), mode, uid: 0, gid: 0, nlink: 1 }
    }
    pub fn new_dir() -> Self {
        VfsMeta { ino: alloc_ino(), mode: 0o755, uid: 0, gid: 0, nlink: 2 }
    }
}

/// 파일 내용 + 메타데이터.
pub struct FileNode {
    pub data: Vec<u8>,
    pub meta: VfsMeta,
}

impl FileNode {
    pub fn new(mode: u32) -> Self {
        FileNode { data: Vec::new(), meta: VfsMeta::new_file(mode) }
    }
}

/// VFS 노드 — 파일 또는 디렉토리.
pub enum VfsNode {
    File(FileNode),
    Dir { dir: TmpfsDir, meta: VfsMeta },
}

/// 디렉토리 열거 결과 한 항목.
pub struct DirEntry {
    pub name:   String,
    pub is_dir: bool,
    pub size:   usize,
}

// ── 글로벌 루트 ─────────────────────────────────────────────────────────────

static ROOT: Mutex<Option<NodeRef>> = Mutex::new(None);

struct SendableExt4(ext4fs::Ext4Fs);
unsafe impl Send for SendableExt4 {}
static EXT4: Mutex<Option<SendableExt4>> = Mutex::new(None);

// ── 초기화 ───────────────────────────────────────────────────────────────────

/// tmpfs 루트 마운트 + 표준 디렉토리 자동 생성.
pub fn init() {
    *ROOT.lock() = Some(tmpfs::new_root());
    // pacman이 필요로 하는 표준 디렉토리
    for d in &[
        "/tmp", "/run", "/run/lock",
        "/var", "/var/lib", "/var/lib/pacman",
        "/var/cache", "/var/cache/pacman", "/var/cache/pacman/pkg",
        "/var/log",
        "/etc", "/etc/pacman.d",
        "/usr", "/usr/bin", "/usr/lib", "/usr/share",
        "/usr/local", "/usr/local/bin",
        "/proc", "/sys", "/dev",
    ] {
        mkdir(d);
    }
    crate::serial_println!("[vfs] tmpfs mounted at / (BETA 12: writable)");
}

pub fn mount_ext4(image: &'static [u8]) -> bool {
    match ext4fs::Ext4Fs::new(image) {
        Ok(fs) => {
            *EXT4.lock() = Some(SendableExt4(fs));
            crate::serial_println!("[vfs] ext4 mounted ({} bytes)", image.len());
            true
        }
        Err(e) => { crate::serial_println!("[vfs] ext4 mount failed: {:?}", e); false }
    }
}

pub fn ext4_read_file(path: &str)     -> Option<Vec<u8>>    { EXT4.lock().as_ref().map(|s| s.0.read_file(path))? }
pub fn ext4_list_dir(path: &str)      -> Vec<DirEntry>       {
    EXT4.lock().as_ref()
        .map(|s| s.0.list_dir(path))
        .unwrap_or_default()
}
pub fn ext4_exists(path: &str)        -> bool               { EXT4.lock().as_ref().map(|s| s.0.exists(path)).unwrap_or(false) }

// ── 경로 탐색 ───────────────────────────────────────────────────────────────

/// 절대 경로로 NodeRef 반환.
pub fn lookup(path: &str) -> Option<NodeRef> {
    let root = ROOT.lock().clone()?;
    if path == "/" { return Some(root); }

    let parts: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    let mut current = root;
    for part in parts {
        let next = {
            let node = current.lock();
            match &*node {
                VfsNode::Dir { ref dir, .. } => dir.lookup(part),
                VfsNode::File(_) => return None,
            }
        };
        current = next?;
    }
    Some(current)
}

fn split_parent(path: &str) -> Option<(String, String)> {
    let path = path.trim_end_matches('/');
    let slash = path.rfind('/')?;
    let parent = if slash == 0 { "/".to_string() } else { path[..slash].to_string() };
    let name   = path[slash + 1..].to_string();
    if name.is_empty() { return None; }
    Some((parent, name))
}

// ── 생성 ─────────────────────────────────────────────────────────────────────

pub fn create_file(path: &str) -> Option<NodeRef> {
    let (parent_path, name) = split_parent(path)?;
    let parent = lookup(&parent_path)?;
    let mut n = parent.lock();
    match &mut *n {
        VfsNode::Dir { ref mut dir, .. } => Some(dir.create_file(&name)),
        _ => None,
    }
}

pub fn mkdir(path: &str) -> Option<NodeRef> {
    let (parent_path, name) = split_parent(path)?;
    let parent = lookup(&parent_path)?;
    let mut n = parent.lock();
    match &mut *n {
        VfsNode::Dir { ref mut dir, .. } => Some(dir.create_dir(&name)),
        _ => None,
    }
}

// ── 읽기 / 쓰기 (커널 내부 API) ─────────────────────────────────────────────

pub fn read_file(path: &str) -> Option<Vec<u8>> {
    let node_ref = lookup(path)?;
    let node = node_ref.lock();
    match &*node {
        VfsNode::File(f) => Some(f.data.clone()),
        _ => None,
    }
}

pub fn write_file(path: &str, data: &[u8]) -> bool {
    let node_ref = match lookup(path) { Some(n) => n, None => return false };
    let mut node = node_ref.lock();
    match &mut *node {
        VfsNode::File(f) => { f.data.clear(); f.data.extend_from_slice(data); true }
        _ => false,
    }
}

pub fn append_file(path: &str, data: &[u8]) -> bool {
    let node_ref = match lookup(path) { Some(n) => n, None => return false };
    let mut node = node_ref.lock();
    match &mut *node {
        VfsNode::File(f) => { f.data.extend_from_slice(data); true }
        _ => false,
    }
}

// ── 디렉토리 열거 ────────────────────────────────────────────────────────────

pub fn list_dir(path: &str) -> Vec<DirEntry> {
    let node_ref = match lookup(path) { Some(n) => n, None => return Vec::new() };
    let node = node_ref.lock();
    match &*node {
        VfsNode::Dir { ref dir, .. } => dir
            .list()
            .into_iter()
            .map(|(name, is_dir, size)| DirEntry { name, is_dir, size })
            .collect(),
        _ => Vec::new(),
    }
}

// ── BETA 12: 신규 VFS 연산 ───────────────────────────────────────────────────

/// 파일 또는 빈 디렉토리 제거 (unlink/rmdir 공통).
///
/// `allow_dir`: true면 빈 디렉토리도 제거 허용.
pub fn remove_entry(path: &str, allow_dir: bool) -> i64 {
    let (parent_str, name) = match split_parent(path) {
        Some(p) => p,
        None    => return crate::syscall::EINVAL,
    };
    let parent = match lookup(&parent_str) {
        Some(p) => p,
        None    => return crate::syscall::ENOENT,
    };

    // 제거 전 검증
    let target = {
        let pn = parent.lock();
        match &*pn {
            VfsNode::Dir { ref dir, .. } => dir.lookup(&name),
            _ => return crate::syscall::ENOENT,
        }
    };
    let target = match target {
        Some(t) => t,
        None    => return crate::syscall::ENOENT,
    };

    {
        let tn = target.lock();
        match &*tn {
            VfsNode::Dir { ref dir, .. } if !allow_dir => return -21, // EISDIR
            VfsNode::Dir { ref dir, .. } if !dir.is_empty() => return -39, // ENOTEMPTY
            VfsNode::File(_) if allow_dir => {} // unlink on file in rmdir mode → allow
            _ => {}
        }
    }

    let mut pn = parent.lock();
    match &mut *pn {
        VfsNode::Dir { ref mut dir, .. } => {
            dir.remove(&name);
            0
        }
        _ => crate::syscall::ENOENT,
    }
}

/// rename(old, new) — 원자적 이동/교체.
pub fn rename(old_path: &str, new_path: &str) -> i64 {
    let (old_parent_str, old_name) = match split_parent(old_path) {
        Some(p) => p,
        None    => return crate::syscall::EINVAL,
    };
    let (new_parent_str, new_name) = match split_parent(new_path) {
        Some(p) => p,
        None    => return crate::syscall::EINVAL,
    };

    let old_parent = match lookup(&old_parent_str) {
        Some(p) => p,
        None    => return crate::syscall::ENOENT,
    };
    let new_parent = match lookup(&new_parent_str) {
        Some(p) => p,
        None    => return crate::syscall::ENOENT,
    };

    if Arc::ptr_eq(&old_parent, &new_parent) {
        // 같은 디렉토리 내 이름 변경 (단순)
        let mut pn = old_parent.lock();
        match &mut *pn {
            VfsNode::Dir { ref mut dir, .. } => {
                let node = match dir.remove(&old_name) {
                    Some(n) => n,
                    None    => return crate::syscall::ENOENT,
                };
                dir.insert(new_name, node);
                0
            }
            _ => crate::syscall::ENOENT,
        }
    } else {
        // 다른 디렉토리 간 이동
        let node = {
            let mut op = old_parent.lock();
            match &mut *op {
                VfsNode::Dir { ref mut dir, .. } => match dir.remove(&old_name) {
                    Some(n) => n,
                    None    => return crate::syscall::ENOENT,
                },
                _ => return crate::syscall::ENOENT,
            }
        };
        let mut np = new_parent.lock();
        match &mut *np {
            VfsNode::Dir { ref mut dir, .. } => {
                dir.insert(new_name, node);
                0
            }
            _ => crate::syscall::ENOENT,
        }
    }
}

/// link(old, new) — 하드 링크 (같은 NodeRef를 두 이름에 연결).
pub fn link_vfs(old_path: &str, new_path: &str) -> i64 {
    let node = match lookup(old_path) {
        Some(n) => n,
        None    => return crate::syscall::ENOENT,
    };
    // 디렉토리 링크 불가
    {
        let n = node.lock();
        if matches!(&*n, VfsNode::Dir { .. }) { return -1; } // EPERM
    }
    let (parent_str, name) = match split_parent(new_path) {
        Some(p) => p,
        None    => return crate::syscall::EINVAL,
    };
    let parent = match lookup(&parent_str) {
        Some(p) => p,
        None    => return crate::syscall::ENOENT,
    };
    let mut pn = parent.lock();
    match &mut *pn {
        VfsNode::Dir { ref mut dir, .. } => {
            dir.insert(name, node.clone());
            // nlink 증가
            let mut n = node.lock();
            if let VfsNode::File(ref mut f) = *n { f.meta.nlink += 1; }
            0
        }
        _ => crate::syscall::ENOENT,
    }
}

/// truncate(path, size) — 파일을 `size` 바이트로 축소/확장.
pub fn truncate_vfs(path: &str, size: usize) -> i64 {
    let node_ref = match lookup(path) {
        Some(n) => n,
        None    => return crate::syscall::ENOENT,
    };
    let mut node = node_ref.lock();
    match &mut *node {
        VfsNode::File(ref mut f) => {
            f.data.resize(size, 0);
            0
        }
        _ => -21, // EISDIR
    }
}

/// truncate via NodeRef (ftruncate용).
pub fn truncate_node(node_ref: &NodeRef, size: usize) -> i64 {
    let mut node = node_ref.lock();
    match &mut *node {
        VfsNode::File(ref mut f) => { f.data.resize(size, 0); 0 }
        _ => -21,
    }
}

/// chmod(path, mode).
pub fn chmod_vfs(path: &str, mode: u32) -> i64 {
    let node_ref = match lookup(path) { Some(n) => n, None => return crate::syscall::ENOENT };
    let mut node = node_ref.lock();
    match &mut *node {
        VfsNode::File(ref mut f) => { f.meta.mode = mode & 0o7777; 0 }
        VfsNode::Dir { ref mut meta, .. } => { meta.mode = mode & 0o7777; 0 }
    }
}

/// chown(path, uid, gid).
pub fn chown_vfs(path: &str, uid: u32, gid: u32) -> i64 {
    let node_ref = match lookup(path) { Some(n) => n, None => return crate::syscall::ENOENT };
    let mut node = node_ref.lock();
    match &mut *node {
        VfsNode::File(ref mut f) => { f.meta.uid = uid; f.meta.gid = gid; 0 }
        VfsNode::Dir { ref mut meta, .. } => { meta.uid = uid; meta.gid = gid; 0 }
    }
}

/// stat 정보를 NodeRef에서 추출.
pub fn stat_node(node_ref: &NodeRef) -> (u64, u32, u32, u32, u32, i64) {
    // returns (ino, mode, uid, gid, nlink, size)
    let node = node_ref.lock();
    match &*node {
        VfsNode::File(f) => (f.meta.ino, f.meta.mode | 0o100000, f.meta.uid, f.meta.gid, f.meta.nlink, f.data.len() as i64),
        VfsNode::Dir { ref meta, .. } => (meta.ino, meta.mode | 0o040000, meta.uid, meta.gid, meta.nlink, 0),
    }
}

/// lookup + stat 한 번에.
pub fn stat_path(path: &str) -> Option<(u64, u32, u32, u32, u32, i64)> {
    let node_ref = lookup(path)?;
    Some(stat_node(&node_ref))
}

// ── BETA 13: copy-up overlay ──────────────────────────────────────────────────

/// 경로의 모든 상위 디렉토리를 tmpfs에 생성 (없으면 생성, copy-up).
pub fn ensure_parent_dirs(path: &str) {
    let path = path.trim_end_matches('/');
    let parts: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() <= 1 { return; }

    let mut cur = String::with_capacity(64);
    for part in &parts[..parts.len() - 1] {
        cur.push('/');
        cur.push_str(part);
        if lookup(&cur).is_none() {
            mkdir(&cur);
        }
    }
}

/// ext4 파일을 tmpfs に copy-up. 이미 tmpfs에 있으면 그것을 반환.
///
/// 반환: Some(NodeRef) → 쓰기 가능한 tmpfs 노드
pub fn copy_up(path: &str) -> Option<NodeRef> {
    // 이미 tmpfs에 있으면 그대로
    if let Some(node) = lookup(path) { return Some(node); }
    // ext4에서 내용 읽기
    let data = ext4_read_file(path)?;
    // 부모 디렉토리 확보
    ensure_parent_dirs(path);
    // tmpfs에 노드 생성 + 데이터 복사
    let node = create_file(path)?;
    {
        let mut n = node.lock();
        if let VfsNode::File(ref mut f) = *n {
            f.data = data;
        }
    }
    crate::serial_println!("[vfs] copy-up: {}", path);
    Some(node)
}

/// 쓰기용 통합 오픈:
/// tmpfs 존재 → 그대로 / ext4 존재 → copy-up / o_creat → 새로 생성.
pub fn open_writable(path: &str, o_creat: bool) -> Option<NodeRef> {
    // 1. tmpfs 직접
    if let Some(node) = lookup(path) { return Some(node); }
    // 2. ext4 copy-up
    if ext4_exists(path) { return copy_up(path); }
    // 3. O_CREAT
    if o_creat {
        ensure_parent_dirs(path);
        return create_file(path);
    }
    None
}

/// 읽기 전용 통합 오픈: tmpfs → ext4 순으로 검색.
pub fn open_readonly(path: &str) -> Option<Vec<u8>> {
    // tmpfs 파일
    if let Some(v) = read_file(path) { return Some(v); }
    // ext4 파일
    ext4_read_file(path)
}

/// 경로가 디렉토리인지 확인 (tmpfs + ext4 + /dev).
pub fn is_dir(path: &str) -> bool {
    if path == "/" { return true; }
    if crate::syscall::dev::is_dev(path) { return crate::syscall::dev::is_dir(path); }
    if let Some(node) = lookup(path) {
        let n = node.lock();
        return matches!(*n, VfsNode::Dir { .. });
    }
    !ext4_list_dir(path).is_empty()
}

/// 경로 존재 확인 (tmpfs + ext4 + /proc + /dev).
pub fn exists_any(path: &str) -> bool {
    if path == "/" { return true; }
    if procfs::is_proc(path) { return procfs::exists(path); }
    if crate::syscall::dev::is_dev(path) { return crate::syscall::dev::exists(path); }
    if lookup(path).is_some() { return true; }
    if ext4_read_file(path).is_some() { return true; }
    !ext4_list_dir(path).is_empty()
}

/// 통합 stat: tmpfs → ext4 → /proc → /dev 순으로 검색.
/// 반환: Some((ino, mode, uid, gid, nlink, size))
pub fn stat_any(path: &str) -> Option<(u64, u32, u32, u32, u32, i64)> {
    if path == "/" {
        return Some((1, 0o040755, 0, 0, 2, 0));
    }
    // /dev
    if crate::syscall::dev::is_dev(path) {
        let (mode, size) = crate::syscall::dev::stat(path)?;
        return Some((0, mode, 0, 0, 1, size));
    }
    // /proc
    if procfs::is_proc(path) && procfs::exists(path) {
        let dir = matches!(path, "/proc" | "/proc/self" | "/proc/self/fd"
            | "/proc/sys" | "/proc/sys/kernel");
        let size = if dir { 0 } else {
            procfs::read(path).map(|v| v.len()).unwrap_or(0) as i64
        };
        return Some((0, if dir { 0o040555 } else { 0o100444 }, 0, 0, 1, size));
    }
    // tmpfs
    if let Some(s) = stat_path(path) { return Some(s); }
    // ext4 파일
    if let Some(data) = ext4_read_file(path) {
        return Some((0, 0o100644, 0, 0, 1, data.len() as i64));
    }
    // ext4 디렉토리
    if !ext4_list_dir(path).is_empty() {
        return Some((0, 0o040755, 0, 0, 2, 0));
    }
    None
}
