//! tmpfs — 메모리 기반 파일시스템
//!
//! BETA 12: VfsMeta 지원, remove/rename 연산 추가.

use alloc::{
    collections::BTreeMap,
    string::String,
    sync::Arc,
    vec::Vec,
};
use spin::Mutex;

use super::{NodeRef, VfsNode, VfsMeta, FileNode};

// ── 디렉토리 노드 ────────────────────────────────────────────────────────────

/// tmpfs 디렉토리.
pub struct TmpfsDir {
    pub children: BTreeMap<String, NodeRef>,
}

impl TmpfsDir {
    pub fn new() -> Self {
        Self { children: BTreeMap::new() }
    }

    pub fn lookup(&self, name: &str) -> Option<NodeRef> {
        self.children.get(name).cloned()
    }

    pub fn create_file(&mut self, name: &str) -> NodeRef {
        self.children
            .entry(name.into())
            .or_insert_with(|| Arc::new(Mutex::new(VfsNode::File(FileNode::new(0o644)))))
            .clone()
    }

    pub fn create_dir(&mut self, name: &str) -> NodeRef {
        self.children
            .entry(name.into())
            .or_insert_with(|| Arc::new(Mutex::new(VfsNode::Dir {
                dir:  TmpfsDir::new(),
                meta: VfsMeta::new_dir(),
            })))
            .clone()
    }

    /// 항목 제거. 반환: 제거된 NodeRef (없으면 None).
    pub fn remove(&mut self, name: &str) -> Option<NodeRef> {
        self.children.remove(name)
    }

    /// 항목 삽입 (rename 대상 또는 link).
    pub fn insert(&mut self, name: String, node: NodeRef) {
        self.children.insert(name, node);
    }

    /// 디렉토리가 비어 있는지.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// (이름, is_dir, size) 목록 — BTreeMap 이므로 이름 순 정렬 보장.
    pub fn list(&self) -> Vec<(String, bool, usize)> {
        self.children
            .iter()
            .map(|(name, node_ref)| {
                let node = node_ref.lock();
                let (is_dir, size) = match &*node {
                    VfsNode::File(f) => (false, f.data.len()),
                    VfsNode::Dir { .. } => (true, 0),
                };
                (name.clone(), is_dir, size)
            })
            .collect()
    }
}

// ── 팩토리 ──────────────────────────────────────────────────────────────────

pub fn new_root() -> NodeRef {
    Arc::new(Mutex::new(VfsNode::Dir {
        dir:  TmpfsDir::new(),
        meta: VfsMeta::new_dir(),
    }))
}
