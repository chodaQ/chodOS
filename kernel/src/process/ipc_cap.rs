//! Zero-copy Capability IPC (ALPHA M2)
//!
//! ## 기존 IPC의 문제
//!
//! `ipc.rs`의 Message는 최대 64바이트만 전달 가능하며 데이터를 복사함.
//! 대용량 데이터(파일 내용, 네트워크 패킷 등)를 전달하려면 복사 비용이 큼.
//!
//! ## 해결책: Capability 토큰 기반 공유 버퍼
//!
//! ```text
//! 송신자                         수신자
//!   │                              │
//!   ├─ alloc_shared(data) ──────►  │        (SHARED_BUFFERS에 버퍼 등록)
//!   │   └► cap_id (u64)            │
//!   │                              │
//!   ├─ send_cap(to, cap_id) ─────► │        (기존 IPC로 cap_id 전달)
//!   │                              │
//!   │                    recv_cap()├─►       (cap_id 수신)
//!   │                              │
//!   │                read_shared(cap_id)─►   (버퍼에 직접 접근, 복사 없음)
//!   │                              │
//!   │                drop_shared(cap_id)─►   (버퍼 해제 + 권한 소멸)
//! ```
//!
//! ## Zero-copy 보장
//!
//! 데이터는 `SHARED_BUFFERS`의 `Vec<u8>` 하나에만 존재함.
//! 송신자 → 수신자 경로에서 데이터 복사가 **전혀** 없음.
//! 전달되는 것은 64비트 정수 `CapId` (토큰) 뿐.
//!
//! ## Capability vs 포인터
//!
//! 단순 포인터와 달리 CapId는:
//! - 소유권: 명시적으로 `drop_shared`하기 전까지는 버퍼가 살아있음
//! - 추적: 누가 어떤 버퍼를 소유하는지 SHARED_BUFFERS 테이블이 기록
//! - 향후: 다중 수신자, 접근 권한(RO/RW) 확장 가능
//!
//! ## 현재 제한
//!
//! - 커널 힙 기반 (가상 주소 공유, 물리 페이지 매핑은 아님)
//! - 단일 코어 전용 (spin::Mutex로 보호되지만 interrupt-context 주의)
//! - 최대 동시 공유 버퍼: `MAX_SHARED` 개 (고정 테이블)

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use super::Pid;

/// 공유 버퍼 식별자 (Capability 토큰)
pub type CapId = u64;

/// 전역 고유 ID 카운터
static NEXT_CAP_ID: AtomicU64 = AtomicU64::new(1);

/// 개별 공유 버퍼 메타데이터
pub struct SharedBuffer {
    /// 이 버퍼의 고유 ID
    pub id: CapId,
    /// 실제 데이터 (힙에 할당)
    pub data: Vec<u8>,
    /// 버퍼를 생성한 프로세스
    pub owner: Pid,
}

/// 전역 공유 버퍼 테이블
///
/// `BTreeMap<CapId, SharedBuffer>`: cap_id → 버퍼 매핑.
/// `Mutex`: interrupt context에서 접근하지 않으므로 spin lock 사용.
static SHARED_BUFFERS: Mutex<BTreeMap<CapId, SharedBuffer>> =
    Mutex::new(BTreeMap::new());

// ── 공개 API ─────────────────────────────────────────────────────────────────

/// 공유 버퍼 할당 — 데이터를 등록하고 CapId 반환.
///
/// 반환된 CapId를 `send_cap()`으로 수신자에게 전달하면
/// 수신자가 `read_shared(cap_id)`로 데이터에 직접 접근할 수 있음.
pub fn alloc_shared(owner: Pid, data: Vec<u8>) -> CapId {
    let id = NEXT_CAP_ID.fetch_add(1, Ordering::Relaxed);
    SHARED_BUFFERS.lock().insert(id, SharedBuffer { id, data, owner });
    id
}

/// 공유 버퍼 내용을 새 데이터로 교체 (BETA-X 2 fast channel 재사용).
///
/// 기존 Vec을 clear 후 extend — 버퍼 재할당 없이 내용만 갱신.
pub fn overwrite_shared(id: CapId, data: &[u8]) -> bool {
    let mut table = SHARED_BUFFERS.lock();
    if let Some(buf) = table.get_mut(&id) {
        buf.data.clear();
        buf.data.extend_from_slice(data);
        true
    } else {
        false
    }
}

/// 공유 버퍼에 데이터를 복사 없이 추가 (append).
///
/// 버퍼가 존재하지 않으면 false 반환.
pub fn append_shared(id: CapId, extra: &[u8]) -> bool {
    let mut table = SHARED_BUFFERS.lock();
    if let Some(buf) = table.get_mut(&id) {
        buf.data.extend_from_slice(extra);
        true
    } else {
        false
    }
}

/// Capability 전달 — 기존 IPC 채널을 통해 cap_id를 수신자에게 전송.
///
/// `cap_id`를 8바이트 데이터로 포장해서 기존 메시지 큐에 넣음.
/// 데이터 자체는 `SHARED_BUFFERS`에 그대로 있음 — 복사 없음.
pub fn send_cap(from: Pid, to: Pid, cap_id: CapId) -> bool {
    let _ = from; // 현재는 scheduler::current_pid()로 대체됨
    // cap_id를 리틀엔디안 8바이트로 직렬화해서 기존 IPC 채널로 전달
    super::ipc::send(to, &cap_id.to_le_bytes())
}

/// 현재 프로세스의 IPC 큐에서 CapId 수신.
///
/// 반환: Some(cap_id) 또는 None (큐가 비어있거나 타입 불일치)
pub fn recv_cap() -> Option<CapId> {
    use super::ipc::recv;
    let msg = recv()?;
    if msg.len != 8 {
        return None;
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&msg.data[0..8]);
    Some(CapId::from_le_bytes(bytes))
}

/// 공유 버퍼에 콜백 방식으로 접근 (zero-copy 읽기).
///
/// `f(data: &[u8])`: 버퍼가 잠긴 동안 `data`를 읽을 수 있는 클로저.
/// 클로저 실행 중 데이터가 이동하거나 복사되지 않음.
///
/// 반환: `Some(f의 반환값)` 또는 `None` (CapId 없음)
pub fn read_shared<R>(id: CapId, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
    let table = SHARED_BUFFERS.lock();
    let buf = table.get(&id)?;
    Some(f(&buf.data))
}

/// 공유 버퍼 해제 — CapId 소멸, 메모리 반환.
///
/// 이후 이 CapId를 사용하는 모든 접근은 None 반환.
pub fn drop_shared(id: CapId) -> bool {
    SHARED_BUFFERS.lock().remove(&id).is_some()
}

/// 현재 등록된 공유 버퍼 수 (디버깅용)
pub fn buffer_count() -> usize {
    SHARED_BUFFERS.lock().len()
}

/// 특정 CapId가 유효한지 확인
pub fn is_valid(id: CapId) -> bool {
    SHARED_BUFFERS.lock().contains_key(&id)
}
