//! BETA 4 / BETA 12: Capability Handle Table ↔ fd 연결
//!
//! ## BETA 12 변경
//! - `FileResource.vfs_node`: 쓰기 가능 파일은 tmpfs NodeRef를 직접 보유 → close 후에도 VFS에 반영
//! - `write_fd`: fd≥3 쓰기 지원
//! - `truncate_fd`: ftruncate(2) 지원
//! - `open_file_writable`: NodeRef를 받아 live-ref 파일 오픈

use alloc::{string::String, vec::Vec};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};
use crate::process::handle::{Capability, HandleTable, Rights};
use crate::vfs::{NodeRef, VfsNode};

// ── 파일 리소스 ──────────────────────────────────────────────────────────────

/// 열린 파일.
///
/// - `vfs_node = Some(...)`: tmpfs live 참조 (쓰기 가능). 읽기/쓰기 모두 VFS 노드를 통함.
/// - `vfs_node = None`: 읽기 전용 캐시 복사본 (ext4 등).
pub struct FileResource {
    pub path:     String,
    pub vfs_node: Option<NodeRef>,  // live VFS 참조 (BETA 12)
    pub data:     Vec<u8>,          // 읽기 전용 캐시 (vfs_node=None 시 사용)
    pos:          AtomicUsize,
    pub write:    bool,
}

impl FileResource {
    pub fn new_readonly(path: String, data: Vec<u8>) -> Self {
        FileResource { path, vfs_node: None, data, pos: AtomicUsize::new(0), write: false }
    }

    pub fn new_live(path: String, node: NodeRef) -> Self {
        FileResource { path, vfs_node: Some(node), data: Vec::new(), pos: AtomicUsize::new(0), write: true }
    }

    // ── 읽기 ─────────────────────────────────────────────────────────────────

    pub fn read(&self, buf: *mut u8, count: usize) -> usize {
        if let Some(ref nr) = self.vfs_node {
            let node = nr.lock();
            if let VfsNode::File(ref f) = *node {
                let pos = self.pos.load(Ordering::Relaxed);
                let avail = f.data.len().saturating_sub(pos);
                let n = count.min(avail);
                if n > 0 {
                    unsafe { core::ptr::copy_nonoverlapping(f.data[pos..].as_ptr(), buf, n); }
                    self.pos.fetch_add(n, Ordering::Relaxed);
                }
                return n;
            }
            return 0;
        }
        // 읽기 전용 캐시
        let pos = self.pos.load(Ordering::Relaxed);
        let avail = self.data.len().saturating_sub(pos);
        let n = count.min(avail);
        if n > 0 {
            unsafe { core::ptr::copy_nonoverlapping(self.data[pos..].as_ptr(), buf, n); }
            self.pos.fetch_add(n, Ordering::Relaxed);
        }
        n
    }

    /// 파일 위치 불변 읽기 (pread64).
    pub fn pread(&self, buf: *mut u8, count: usize, offset: usize) -> usize {
        if let Some(ref nr) = self.vfs_node {
            let node = nr.lock();
            if let VfsNode::File(ref f) = *node {
                let avail = f.data.len().saturating_sub(offset);
                let n = count.min(avail);
                if n > 0 { unsafe { core::ptr::copy_nonoverlapping(f.data[offset..].as_ptr(), buf, n); } }
                return n;
            }
            return 0;
        }
        let avail = self.data.len().saturating_sub(offset);
        let n = count.min(avail);
        if n > 0 { unsafe { core::ptr::copy_nonoverlapping(self.data[offset..].as_ptr(), buf, n); } }
        n
    }

    // ── 쓰기 ─────────────────────────────────────────────────────────────────

    /// 현재 위치에 쓰기 (write-through to VFS).
    pub fn write_at(&self, buf: *const u8, count: usize) -> i64 {
        if !self.write { return super::EBADF; }
        if let Some(ref nr) = self.vfs_node {
            let mut node = nr.lock();
            if let VfsNode::File(ref mut f) = *node {
                let pos = self.pos.load(Ordering::Relaxed);
                let new_len = pos + count;
                if f.data.len() < new_len { f.data.resize(new_len, 0); }
                unsafe {
                    core::ptr::copy_nonoverlapping(buf, f.data[pos..].as_mut_ptr(), count);
                }
                self.pos.fetch_add(count, Ordering::Relaxed);
                return count as i64;
            }
        }
        super::EBADF
    }

    // ── lseek ────────────────────────────────────────────────────────────────

    pub fn seek(&self, offset: i64, whence: u64) -> i64 {
        let len = self.size() as i64;
        let cur = self.pos.load(Ordering::Relaxed) as i64;
        let new_pos = match whence {
            0 => offset,
            1 => cur + offset,
            2 => len + offset,
            _ => return super::EINVAL,
        };
        if new_pos < 0 { return super::EINVAL; }
        self.pos.store(new_pos as usize, Ordering::Relaxed);
        new_pos
    }

    // ── ftruncate ────────────────────────────────────────────────────────────

    pub fn truncate(&self, size: usize) -> i64 {
        if let Some(ref nr) = self.vfs_node {
            return crate::vfs::truncate_node(nr, size);
        }
        super::EINVAL
    }

    // ── 메타 ─────────────────────────────────────────────────────────────────

    pub fn size(&self) -> usize {
        if let Some(ref nr) = self.vfs_node {
            let node = nr.lock();
            return match &*node {
                VfsNode::File(f) => f.data.len(),
                _ => 0,
            };
        }
        self.data.len()
    }

    pub fn pos(&self) -> usize { self.pos.load(Ordering::Relaxed) }
}

// ── 디렉토리 리소스 ───────────────────────────────────────────────────────────

pub struct DirResource {
    pub path: String,
}

// ── 전역 FD 테이블 ────────────────────────────────────────────────────────────

struct FdTableCell(UnsafeCell<Option<HandleTable>>);
unsafe impl Sync for FdTableCell {}
static FD_TABLE: FdTableCell = FdTableCell(UnsafeCell::new(None));

/// dev.rs에서 DevResource 등록 시 사용하는 공개 래퍼.
pub fn with_table_pub<R>(f: impl FnOnce(&mut HandleTable) -> R) -> R {
    with_table(f)
}

fn with_table<R>(f: impl FnOnce(&mut HandleTable) -> R) -> R {
    unsafe {
        let ptr = FD_TABLE.0.get();
        if (*ptr).is_none() { *ptr = Some(HandleTable::new()); }
        f((*ptr).as_mut().unwrap())
    }
}

pub fn init() {
    unsafe { *FD_TABLE.0.get() = Some(HandleTable::new()); }
    crate::serial_println!("[fd] table reset");
}

// ── 공개 API ─────────────────────────────────────────────────────────────────

/// 읽기 전용 파일 오픈 (ext4 등 캐시 복사본).
pub fn open_file(path: &str, data: Vec<u8>, writable: bool) -> u32 {
    if writable {
        // 쓰기 요청인데 NodeRef 없으면 tmpfs에 생성
        let nr = crate::vfs::lookup(path)
            .or_else(|| crate::vfs::create_file(path));
        if let Some(nr) = nr {
            // 기존 데이터를 NodeRef에 기록 (O_TRUNC 없으면 덮어쓰지 않음)
            // 단: data가 비어 있지 않으면 NodeRef에 복사 (초기 내용)
            if !data.is_empty() {
                let mut node = nr.lock();
                if let crate::vfs::VfsNode::File(ref mut f) = *node {
                    if f.data.is_empty() {
                        f.data.extend_from_slice(&data);
                    }
                }
            }
            return open_file_writable(path, nr);
        }
    }
    with_table(|t| {
        let cap = Capability::new(FileResource::new_readonly(path.into(), data), Rights::READ);
        let h = t.insert(cap);
        crate::serial_println!("[fd] open_ro {:?} → fd {}", path, h.id);
        h.id
    })
}

/// 쓰기 가능 파일 오픈 (live NodeRef).
pub fn open_file_writable(path: &str, node: NodeRef) -> u32 {
    with_table(|t| {
        let cap = Capability::new(
            FileResource::new_live(path.into(), node),
            Rights::READ | Rights::WRITE,
        );
        let h = t.insert(cap);
        crate::serial_println!("[fd] open_rw {:?} → fd {}", path, h.id);
        h.id
    })
}

/// 디렉토리 오픈.
pub fn open_dir(path: &str) -> u32 {
    with_table(|t| {
        let cap = Capability::new(DirResource { path: path.into() }, Rights::READ);
        let h = t.insert(cap);
        crate::serial_println!("[fd] open_dir {:?} → fd {}", path, h.id);
        h.id
    })
}

/// 파일 읽기.
pub fn read(fd: u32, buf: *mut u8, count: usize) -> i64 {
    with_table(|t| {
        if let Some(f) = t.get::<FileResource>(fd, Rights::READ) {
            f.read(buf, count) as i64
        } else {
            super::EBADF
        }
    })
}

/// 오프셋 지정 읽기.
pub fn pread(fd: u32, buf: *mut u8, count: usize, offset: usize) -> i64 {
    with_table(|t| {
        if let Some(f) = t.get::<FileResource>(fd, Rights::READ) {
            f.pread(buf, count, offset) as i64
        } else {
            super::EBADF
        }
    })
}

/// 파일 쓰기 (BETA 12: VFS write-through).
pub fn write_fd(fd: u32, buf: *const u8, count: usize) -> i64 {
    with_table(|t| {
        if let Some(f) = t.get::<FileResource>(fd, Rights::WRITE) {
            f.write_at(buf, count)
        } else {
            super::EBADF
        }
    })
}

/// ftruncate.
pub fn truncate_fd(fd: u32, size: usize) -> i64 {
    with_table(|t| {
        if let Some(f) = t.get::<FileResource>(fd, Rights::WRITE) {
            f.truncate(size)
        } else {
            super::EINVAL
        }
    })
}

/// lseek.
pub fn seek(fd: u32, offset: i64, whence: u64) -> i64 {
    with_table(|t| {
        if let Some(f) = t.get::<FileResource>(fd, Rights::NONE) {
            f.seek(offset, whence)
        } else {
            super::EBADF
        }
    })
}

/// fd 닫기.
pub fn close(fd: u32) -> bool {
    let ok = with_table(|t| t.close(fd));
    if ok { crate::serial_println!("[fd] close fd {}", fd); }
    ok
}

/// fstat: (파일크기, is_dir).
pub fn fstat(fd: u32) -> Option<(i64, bool)> {
    with_table(|t| {
        if let Some(f) = t.get::<FileResource>(fd, Rights::NONE) {
            Some((f.size() as i64, false))
        } else if t.get::<DirResource>(fd, Rights::NONE).is_some() {
            Some((0i64, true))
        } else {
            None
        }
    })
}

/// getdents64용 디렉토리 경로 조회.
pub fn dir_path(fd: u32) -> Option<String> {
    with_table(|t| t.get::<DirResource>(fd, Rights::NONE).map(|d| d.path.clone()))
}

/// 파일 경로 조회 (fstat 등 확장용).
pub fn file_path(fd: u32) -> Option<String> {
    with_table(|t| t.get::<FileResource>(fd, Rights::NONE).map(|f| f.path.clone()))
}

pub fn is_open(fd: u32) -> bool {
    with_table(|t| {
        t.get::<FileResource>(fd, Rights::NONE).is_some()
            || t.get::<DirResource>(fd, Rights::NONE).is_some()
    })
}

pub fn list() -> alloc::vec::Vec<(u32, Rights)> {
    with_table(|t| t.list())
}
