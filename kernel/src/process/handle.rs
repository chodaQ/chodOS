//! Capability Handle Table (ALPHA 9)
//!
//! ## 설계
//!
//! Linux의 fd(file descriptor)에 해당하는 커널 추상화.
//! 각 프로세스는 PCB에 `HandleTable`을 보유하고,
//! 핸들 ID로 타입 소거된 Capability에 접근한다.
//!
//! ```
//! Linux fd  ↔  Handle  ↔  RawCapability  ↔  Arc<T>
//! ```
//!
//! ## 권한 모델
//!
//! Capability는 `Rights` 비트마스크로 접근 권한을 제한한다.
//! 핸들을 공유할 때 권한을 줄일 수 있지만 늘릴 수는 없다.
//!
//! ## 타입 소거
//!
//! `HandleTable`은 모든 타입의 리소스를 `Arc<dyn Any + Send + Sync>`로 저장한다.
//! 조회 시 `Arc::downcast::<T>()`로 원래 타입을 복원한다.

use alloc::{collections::BTreeMap, sync::Arc};
use core::any::Any;

// ── 권한 비트마스크 ───────────────────────────────────────────────────────────

/// 접근 권한 비트마스크
///
/// Linux의 O_RDONLY/O_RDWR/O_WRONLY와 개념적으로 대응.
/// Capability를 위임할 때 부분집합만 전달할 수 있다.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rights(pub u32);

impl Rights {
    pub const NONE:  Rights = Rights(0);
    pub const READ:  Rights = Rights(1 << 0);
    pub const WRITE: Rights = Rights(1 << 1);
    pub const EXEC:  Rights = Rights(1 << 2);
    /// 다른 프로세스에 위임 가능 (seL4 Grant 개념)
    pub const GRANT: Rights = Rights(1 << 3);
    pub const ALL:   Rights = Rights(0b1111);

    /// `other`의 모든 권한 비트가 self에 포함되는지 확인
    #[inline]
    pub fn contains(self, other: Rights) -> bool {
        (self.0 & other.0) == other.0
    }

    fn fmt_bits(self) -> &'static str {
        match self.0 & 0b111 {
            0b000 => "---",
            0b001 => "r--",
            0b010 => "-w-",
            0b011 => "rw-",
            0b100 => "--x",
            0b101 => "r-x",
            0b110 => "-wx",
            0b111 => "rwx",
            _     => "???",
        }
    }
}

impl core::fmt::Debug for Rights {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let grant = if self.contains(Rights::GRANT) { "+grant" } else { "" };
        write!(f, "{}{}", self.fmt_bits(), grant)
    }
}

impl core::fmt::Display for Rights {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

impl core::ops::BitOr for Rights {
    type Output = Rights;
    fn bitor(self, rhs: Rights) -> Rights { Rights(self.0 | rhs.0) }
}

impl core::ops::BitAnd for Rights {
    type Output = Rights;
    fn bitand(self, rhs: Rights) -> Rights { Rights(self.0 & rhs.0) }
}

// ── 내부 타입 소거 Capability ─────────────────────────────────────────────────

/// 타입이 소거된 Capability — HandleTable 내부 저장용
///
/// `object`는 `Arc<dyn Any + Send + Sync>`로 저장되고,
/// `downcast::<T>()`로 원래 타입을 복원한다.
pub struct RawCapability {
    pub object: Arc<dyn Any + Send + Sync>,
    pub rights: Rights,
}

impl RawCapability {
    /// 원래 타입 `T`로 다운캐스트 (권한 체크 없음 — 호출자가 책임)
    pub fn downcast<T: Any + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        Arc::clone(&self.object).downcast::<T>().ok()
    }
}

// ── 공개 Capability<T> ───────────────────────────────────────────────────────

/// 타입 있는 Capability — 생성 / 등록 시 사용
///
/// `HandleTable::insert`에 넘기면 `RawCapability`로 변환되어 저장된다.
pub struct Capability<T: Any + Send + Sync + 'static> {
    pub object: Arc<T>,
    pub rights: Rights,
}

impl<T: Any + Send + Sync + 'static> Capability<T> {
    /// 새 리소스 `T`를 주어진 권한으로 감싼다
    pub fn new(object: T, rights: Rights) -> Self {
        Capability { object: Arc::new(object), rights }
    }

    /// 이미 `Arc<T>`가 있을 때 사용
    pub fn from_arc(object: Arc<T>, rights: Rights) -> Self {
        Capability { object, rights }
    }

    /// 권한을 줄인 파생 핸들 생성 (늘리기는 불가)
    pub fn restrict(self, mask: Rights) -> Self {
        Capability { object: self.object, rights: self.rights & mask }
    }

    pub(super) fn into_raw(self) -> RawCapability {
        RawCapability {
            object: self.object as Arc<dyn Any + Send + Sync>,
            rights: self.rights,
        }
    }
}

// ── Handle (Linux fd 역할) ────────────────────────────────────────────────────

/// 프로세스가 보유하는 핸들 — Linux fd에 해당
///
/// HandleTable에서 발급되며, id로 Capability를 조회한다.
/// 닫히면 HandleTable에서 제거되고 Arc 참조 카운트가 감소한다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle {
    /// 핸들 ID (Linux fd와 같은 역할)
    pub id: u32,
    /// 이 핸들이 허용하는 권한 (원본 Capability 권한의 부분집합)
    pub rights: Rights,
}

// ── HandleTable ──────────────────────────────────────────────────────────────

/// PCB에 들어가는 핸들 테이블
///
/// fd 0 = stdin, 1 = stdout, 2 = stderr 예약 (현재 미구현).
/// 새 핸들은 3번부터 할당된다.
pub struct HandleTable {
    table: BTreeMap<u32, RawCapability>,
    next_id: u32,
}

impl HandleTable {
    pub fn new() -> Self {
        HandleTable { table: BTreeMap::new(), next_id: 3 }
    }

    // ── 핸들 발급 ────────────────────────────────────────────────────────────

    /// Capability를 테이블에 등록하고 Handle을 반환.
    ///
    /// 반환된 Handle의 id는 Linux fd 역할.
    pub fn insert<T: Any + Send + Sync + 'static>(&mut self, cap: Capability<T>) -> Handle {
        let id = self.next_id;
        self.next_id += 1;
        let rights = cap.rights;
        self.table.insert(id, cap.into_raw());
        Handle { id, rights }
    }

    // ── 핸들 조회 ────────────────────────────────────────────────────────────

    /// 핸들 ID로 `Arc<T>`를 조회.
    ///
    /// 실패 조건:
    /// - id가 없음
    /// - 저장된 타입이 T가 아님
    /// - `required` 권한이 부족함
    pub fn get<T: Any + Send + Sync + 'static>(
        &self,
        id: u32,
        required: Rights,
    ) -> Option<Arc<T>> {
        let raw = self.table.get(&id)?;
        if !raw.rights.contains(required) {
            return None; // 권한 부족
        }
        raw.downcast::<T>()
    }

    /// 권한 체크 없이 존재 여부만 확인
    pub fn contains(&self, id: u32) -> bool {
        self.table.contains_key(&id)
    }

    /// 핸들의 현재 권한 조회 (위임 전 확인용)
    pub fn rights_of(&self, id: u32) -> Option<Rights> {
        self.table.get(&id).map(|r| r.rights)
    }

    // ── 핸들 닫기 ────────────────────────────────────────────────────────────

    /// 핸들을 닫고 `Arc` 참조 카운트를 감소시킨다.
    ///
    /// 다른 프로세스가 같은 리소스를 공유하고 있지 않으면 여기서 drop됨.
    pub fn close(&mut self, id: u32) -> bool {
        self.table.remove(&id).is_some()
    }

    // ── 메타 ────────────────────────────────────────────────────────────────

    pub fn len(&self) -> usize { self.table.len() }
    pub fn is_empty(&self) -> bool { self.table.is_empty() }

    /// 모든 열린 핸들의 (id, rights) 목록 (디버깅용)
    pub fn list(&self) -> alloc::vec::Vec<(u32, Rights)> {
        self.table.iter().map(|(&id, raw)| (id, raw.rights)).collect()
    }
}
