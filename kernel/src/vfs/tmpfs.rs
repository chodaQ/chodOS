//! tmpfs — 메모리 기반 파일시스템 구현
//!
//! ## 구조
//!
//! ```
//! VfsNode::Dir(TmpfsDir)
//!   └── children: BTreeMap<String, NodeRef>
//!         ├── "etc" → VfsNode::Dir(TmpfsDir)
//!         │     └── "hosts" → VfsNode::File(Vec<u8>)
//!         └── "hello.txt" → VfsNode::File(Vec<u8>)
//! ```
//!
//! ## 특징
//!
//! - 모든 데이터는 커널 힙(`alloc`)에 저장
//! - 시스템 리셋(재부팅)시 소멸 — 비휘발성 저장소 없음
//! - `BTreeMap`: 삽입 순서 독립적, 이름 순 정렬 열거 보장
//! - 중첩 깊이 제한 없음 (힙 크기에만 의존)

use alloc::{
    collections::BTreeMap,
    string::String,
    sync::Arc,
    vec::Vec,
};
use spin::Mutex;

use super::{NodeRef, VfsNode};

// ── 파일 노드 ────────────────────────────────────────────────────────────────

// TmpfsFile은 별도 구조체가 필요 없음:
// VfsNode::File(Vec<u8>) 자체가 파일 데이터를 보유.

// ── 디렉토리 노드 ────────────────────────────────────────────────────────────

/// tmpfs 디렉토리.
///
/// `children`: 항목 이름 → 자식 `NodeRef` 매핑.
/// BTreeMap이므로 항상 이름 순(오름차순)으로 열거됨.
pub struct TmpfsDir {
    pub children: BTreeMap<String, NodeRef>,
}

impl TmpfsDir {
    pub fn new() -> Self {
        Self { children: BTreeMap::new() }
    }

    /// 이름으로 자식 노드 검색.
    ///
    /// 경로 컴포넌트 탐색에 사용 (예: `"etc"` 검색 → 재귀적으로 다음 컴포넌트 탐색).
    pub fn lookup(&self, name: &str) -> Option<NodeRef> {
        self.children.get(name).cloned()
    }

    /// 빈 파일 노드를 생성하고 등록.
    ///
    /// 이미 같은 이름이 있으면 기존 노드를 그대로 반환(덮어쓰지 않음).
    /// 이렇게 하면 `create_file` 후 바로 `write_file`해도 같은 노드를 수정.
    pub fn create_file(&mut self, name: &str) -> NodeRef {
        self.children
            .entry(name.into())
            .or_insert_with(|| Arc::new(Mutex::new(VfsNode::File(Vec::new()))))
            .clone()
    }

    /// 빈 디렉토리 노드를 생성하고 등록.
    ///
    /// 이미 같은 이름이 있으면 기존 노드 반환.
    pub fn create_dir(&mut self, name: &str) -> NodeRef {
        self.children
            .entry(name.into())
            .or_insert_with(|| Arc::new(Mutex::new(VfsNode::Dir(TmpfsDir::new()))))
            .clone()
    }

    /// 디렉토리 내용을 `(이름, is_dir, size)` 목록으로 반환.
    ///
    /// BTreeMap 이므로 이름 오름차순 정렬이 보장됨.
    /// `size`: 파일이면 바이트 수, 디렉토리면 0.
    pub fn list(&self) -> Vec<(String, bool, usize)> {
        self.children
            .iter()
            .map(|(name, node_ref)| {
                let node = node_ref.lock();
                let (is_dir, size) = match &*node {
                    VfsNode::File(data) => (false, data.len()),
                    VfsNode::Dir(_)     => (true,  0),
                };
                (name.clone(), is_dir, size)
            })
            .collect()
    }
}

// ── 팩토리 ──────────────────────────────────────────────────────────────────

/// 새 빈 tmpfs 루트 디렉토리(`/`)를 생성.
///
/// `vfs::init()`이 이것을 글로벌 마운트 포인트로 설치.
pub fn new_root() -> NodeRef {
    Arc::new(Mutex::new(VfsNode::Dir(TmpfsDir::new())))
}
