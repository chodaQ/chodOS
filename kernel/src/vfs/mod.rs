//! VFS (Virtual File System) — 가상 파일시스템 계층
//!
//! ## 설계 개요
//!
//! ```
//!  커널 코드 / 시스템콜
//!       │
//!       ▼
//!  ┌─────────────────────────────────┐
//!  │         VFS 인터페이스          │  ← 이 모듈 (경로 파싱, 마운트 테이블)
//!  └─────────────────────────────────┘
//!       │
//!       ▼
//!  ┌─────────────────────────────────┐
//!  │   tmpfs 구현체 (vfs/tmpfs.rs)   │  ← 메모리 기반 파일시스템
//!  └─────────────────────────────────┘
//! ```
//!
//! ## 핵심 타입
//!
//! - `NodeRef`: `Arc<Mutex<VfsNode>>` — VFS 노드의 공유 가변 참조
//! - `VfsNode`: File(Vec<u8>) 또는 Dir(BTreeMap) 열거형
//!
//! ## 경로 규칙
//!
//! - 항상 `/`로 시작하는 절대 경로
//! - 예: `/etc/config`, `/home/user/data.txt`
//! - 루트는 `/`
//!
//! ## 현재 제한
//!
//! - 마운트 포인트 1개 (`/` = tmpfs)
//! - 동기 인터페이스 (비동기 I/O 없음)
//! - 파일 권한/소유자 없음 (Milestone 4에서 추가 예정)

pub mod ext4fs;
pub mod tmpfs;

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use spin::Mutex;

use tmpfs::TmpfsDir;

// ── 공개 타입 ───────────────────────────────────────────────────────────────

/// VFS 노드의 공유 가변 참조.
///
/// `Arc`: 여러 경로가 같은 노드를 참조할 수 있음 (하드링크 기반).
/// `Mutex`: 멀티코어/인터럽트 안전 — 노드 수정 시 잠금 필요.
pub type NodeRef = Arc<Mutex<VfsNode>>;

/// VFS 노드 — 파일 또는 디렉토리
pub enum VfsNode {
    /// 일반 파일: 바이트 시퀀스
    File(Vec<u8>),
    /// 디렉토리: 이름 → 자식 노드 매핑
    Dir(TmpfsDir),
}

/// 디렉토리 열거(`list_dir`) 결과의 한 항목
pub struct DirEntry {
    /// 파일명 (경로 제외)
    pub name: String,
    /// true = 디렉토리, false = 파일
    pub is_dir: bool,
    /// 파일 크기 (바이트), 디렉토리는 0
    pub size: usize,
}

// ── 글로벌 루트 ─────────────────────────────────────────────────────────────

/// 루트 파일시스템 마운트 포인트 (`/`) — tmpfs
static ROOT: Mutex<Option<NodeRef>> = Mutex::new(None);

/// ext4-view의 `Ext4`는 내부에서 `Rc`를 사용해 `!Send`.
/// 단일 코어 커널이므로 실제 데이터 경쟁은 없음 — Mutex 보호로 충분.
struct SendableExt4(ext4fs::Ext4Fs);
unsafe impl Send for SendableExt4 {}

/// ext4 이미지 마운트 (ALPHA 8)
static EXT4: Mutex<Option<SendableExt4>> = Mutex::new(None);

// ── 초기화 ──────────────────────────────────────────────────────────────────

/// VFS 초기화 — tmpfs를 루트(`/`)에 마운트
pub fn init() {
    let root = tmpfs::new_root();
    *ROOT.lock() = Some(root);
    crate::serial_println!("[vfs] tmpfs mounted at /");
}

/// ext4 이미지를 마운트 (ALPHA 8).
///
/// `image`: 커널에 embed된 ext4 파티션 이미지 (`include_bytes!`로 로드).
/// 성공 시 `ext4_read_file` / `ext4_list_dir`로 접근 가능.
pub fn mount_ext4(image: &'static [u8]) -> bool {
    match ext4fs::Ext4Fs::new(image) {
        Ok(fs) => {
            *EXT4.lock() = Some(SendableExt4(fs));
            crate::serial_println!("[vfs] ext4 mounted ({} bytes image)", image.len());
            true
        }
        Err(e) => {
            crate::serial_println!("[vfs] ext4 mount failed: {:?}", e);
            false
        }
    }
}

/// ext4에서 파일을 읽어 반환.
pub fn ext4_read_file(path: &str) -> Option<Vec<u8>> {
    EXT4.lock().as_ref().map(|s| s.0.read_file(path))?
}

/// ext4 디렉토리 항목 열거.
pub fn ext4_list_dir(path: &str) -> Vec<DirEntry> {
    EXT4.lock().as_ref()
        .map(|s| s.0.list_dir(path))
        .unwrap_or_default()
}

/// ext4 경로 존재 여부 확인.
pub fn ext4_exists(path: &str) -> bool {
    EXT4.lock().as_ref().map(|s| s.0.exists(path)).unwrap_or(false)
}

// ── 경로 탐색 ───────────────────────────────────────────────────────────────

/// 절대 경로로 VFS 노드를 검색.
///
/// 경로 예시: `/`, `/etc`, `/etc/hosts`
///
/// 반환: 찾으면 `Some(NodeRef)`, 없으면 `None`
pub fn lookup(path: &str) -> Option<NodeRef> {
    let root = ROOT.lock().clone()?;

    // 루트 자체 요청
    if path == "/" {
        return Some(root);
    }

    // `/a/b/c` → ["a", "b", "c"] (빈 항목 제거: 선행 / 처리)
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
                VfsNode::Dir(dir) => dir.lookup(part),
                VfsNode::File(_) => return None, // 파일 아래에는 경로가 없음
            }
        };
        current = next?;
    }
    Some(current)
}

/// 부모 디렉토리 경로와 파일명을 분리.
///
/// 예: `/etc/hosts` → (`/etc`, `hosts`)
///     `/foo`       → (`/`,   `foo`)
fn split_parent(path: &str) -> Option<(String, String)> {
    let path = path.trim_end_matches('/');
    let slash = path.rfind('/')?;

    let parent = if slash == 0 { "/".to_string() } else { path[..slash].to_string() };
    let name = path[slash + 1..].to_string();

    if name.is_empty() { return None; }
    Some((parent, name))
}

// ── 파일 및 디렉토리 생성 ────────────────────────────────────────────────────

/// 경로에 빈 파일을 생성하고 `NodeRef`를 반환.
///
/// 부모 디렉토리가 없으면 `None`.
/// 이미 같은 이름이 존재하면 기존 노드를 반환(덮어쓰기 않음).
pub fn create_file(path: &str) -> Option<NodeRef> {
    let (parent_path, name) = split_parent(path)?;
    let parent = lookup(&parent_path)?;
    let mut parent_node = parent.lock();
    match &mut *parent_node {
        VfsNode::Dir(dir) => Some(dir.create_file(&name)),
        VfsNode::File(_) => None,
    }
}

/// 경로에 디렉토리를 생성하고 `NodeRef`를 반환.
pub fn mkdir(path: &str) -> Option<NodeRef> {
    let (parent_path, name) = split_parent(path)?;
    let parent = lookup(&parent_path)?;
    let mut parent_node = parent.lock();
    match &mut *parent_node {
        VfsNode::Dir(dir) => Some(dir.create_dir(&name)),
        VfsNode::File(_) => None,
    }
}

// ── 읽기 / 쓰기 ─────────────────────────────────────────────────────────────

/// 파일 전체 내용을 `Vec<u8>`으로 읽어 반환.
///
/// 경로가 없거나 파일이 아니면 `None`.
pub fn read_file(path: &str) -> Option<Vec<u8>> {
    let node_ref = lookup(path)?;
    let node = node_ref.lock();
    match &*node {
        VfsNode::File(data) => Some(data.clone()),
        VfsNode::Dir(_) => None,
    }
}

/// 파일에 데이터를 (덮어)쓰기.
///
/// 경로가 없거나 디렉토리이면 false.
pub fn write_file(path: &str, data: &[u8]) -> bool {
    let node_ref = match lookup(path) {
        Some(n) => n,
        None => return false,
    };
    let mut node = node_ref.lock();
    match &mut *node {
        VfsNode::File(buf) => {
            buf.clear();
            buf.extend_from_slice(data);
            true
        }
        VfsNode::Dir(_) => false,
    }
}

/// 파일에 데이터를 이어 쓰기(append).
pub fn append_file(path: &str, data: &[u8]) -> bool {
    let node_ref = match lookup(path) {
        Some(n) => n,
        None => return false,
    };
    let mut node = node_ref.lock();
    match &mut *node {
        VfsNode::File(buf) => {
            buf.extend_from_slice(data);
            true
        }
        VfsNode::Dir(_) => false,
    }
}

// ── 디렉토리 열거 ────────────────────────────────────────────────────────────

/// 디렉토리 내용을 `DirEntry` 목록으로 반환 (이름 순 정렬).
///
/// 경로가 없거나 파일이면 빈 Vec.
pub fn list_dir(path: &str) -> Vec<DirEntry> {
    let node_ref = match lookup(path) {
        Some(n) => n,
        None => return Vec::new(),
    };
    let node = node_ref.lock();
    match &*node {
        VfsNode::Dir(dir) => dir
            .list()
            .into_iter()
            .map(|(name, is_dir, size)| DirEntry { name, is_dir, size })
            .collect(),
        VfsNode::File(_) => Vec::new(),
    }
}
