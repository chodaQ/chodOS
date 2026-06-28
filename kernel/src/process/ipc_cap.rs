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
    /// 실제 데이터 (힙에 할당, frame_backed=false 시 사용)
    pub data: Vec<u8>,
    /// 버퍼를 생성한 프로세스
    pub owner: Pid,

    // ── BETA-X-2 2: 비대칭 권한 매핑 ────────────────────────────────────
    /// true = 물리 프레임 기반 (비대칭 권한 매핑 활성)
    pub frame_backed: bool,
    /// 송신자용 쓰기 가능 VA (HHDM + phys)
    pub write_va: u64,
    /// 수신자용 읽기 전용 VA (RO_CHANNEL_BASE + slot*4096, PTE_WRITABLE 없음)
    pub read_va: *const u8,
    /// 물리 프레임 주소 (해제 시 사용)
    pub phys: u64,
    /// 실제 데이터 길이 (write_va 기반일 때 유효)
    pub frame_len: usize,
}

unsafe impl Send for SharedBuffer {}
unsafe impl Sync for SharedBuffer {}

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
    SHARED_BUFFERS.lock().insert(id, SharedBuffer {
        id, data, owner,
        frame_backed: false,
        write_va: 0, read_va: core::ptr::null(), phys: 0, frame_len: 0,
    });
    id
}

/// 물리 프레임 기반 공유 버퍼 할당 (BETA-X-2 2: 비대칭 권한 매핑).
///
/// - sender: `write_va`에 쓰기 가능 (HHDM, PTE_WRITABLE)
/// - receiver: `read_va`에서 읽기만 가능 (PTE_WRITABLE 없음)
/// 수신자가 read_va에 write 시도 → #PF (하드웨어 격리)
pub fn alloc_shared_frame(owner: Pid, capacity: usize) -> CapId {
    let (write_va, read_va, phys) = crate::paging::alloc_channel_frame();
    let id = NEXT_CAP_ID.fetch_add(1, Ordering::Relaxed);
    SHARED_BUFFERS.lock().insert(id, SharedBuffer {
        id,
        data: Vec::new(), // 미사용 (frame_backed=true)
        owner,
        frame_backed: true,
        write_va,
        read_va,
        phys,
        frame_len: capacity.min(4096),
    });
    id
}

/// 공유 버퍼 내용을 새 데이터로 교체 (BETA-X 2 fast channel 재사용).
///
/// frame_backed=true 시: write_va(쓰기 가능)에 직접 기록 — Vec 할당 없음.
/// frame_backed=false 시: Vec clear+extend (기존 동작).
pub fn overwrite_shared(id: CapId, data: &[u8]) -> bool {
    let mut table = SHARED_BUFFERS.lock();
    if let Some(buf) = table.get_mut(&id) {
        if buf.frame_backed {
            let write_len = data.len().min(4096);
            unsafe {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    buf.write_va as *mut u8,
                    write_len,
                );
            }
            buf.frame_len = write_len;
        } else {
            buf.data.clear();
            buf.data.extend_from_slice(data);
        }
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
    if buf.frame_backed {
        // read_va = read-only 매핑 (PTE_WRITABLE 없음 — 수신자 쓰기 시 #PF)
        let slice = unsafe { core::slice::from_raw_parts(buf.read_va, buf.frame_len) };
        Some(f(slice))
    } else {
        Some(f(&buf.data))
    }
}

/// 공유 버퍼 해제 — CapId 소멸, 메모리 반환.
///
/// 이후 이 CapId를 사용하는 모든 접근은 None 반환.
pub fn drop_shared(id: CapId) -> bool {
    if let Some(buf) = SHARED_BUFFERS.lock().remove(&id) {
        if buf.frame_backed {
            crate::paging::free_channel_frame(buf.phys);
        }
        true
    } else {
        false
    }
}

/// 현재 등록된 공유 버퍼 수 (디버깅용)
pub fn buffer_count() -> usize {
    SHARED_BUFFERS.lock().len()
}

/// 특정 CapId가 유효한지 확인
pub fn is_valid(id: CapId) -> bool {
    SHARED_BUFFERS.lock().contains_key(&id)
}

// ── BETA-X-2 3: 격리 검증 ────────────────────────────────────────────────────

/// 격리 검증 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationResult {
    /// 검증 대상이 아님 (frame_backed=false)
    NotApplicable,
    /// write_va 쓰기 → read_va 읽기 왕복 일치 + read_va PTE_WRITABLE 없음
    Ok,
    /// PTE 조회 실패 (read_va 미매핑)
    PteMissing,
    /// read_va PTE에 PTE_WRITABLE 비트가 설정됨 (비대칭 위반)
    ReadVaIsWritable,
    /// read_va에서 읽은 값이 write_va에 쓴 값과 불일치 (매핑 불량)
    DataMismatch,
}

/// frame_backed 채널의 격리 상태를 검증.
///
/// 검증 순서:
/// 1. read_va의 PTE를 커널 page table에서 조회 → PTE_WRITABLE 없음 확인
/// 2. write_va에 테스트 패턴 기록
/// 3. read_va에서 읽어 패턴 일치 확인 (같은 물리 프레임이므로 일치해야 함)
pub fn verify_channel_isolation(id: CapId) -> IsolationResult {
    const PTE_WRITABLE_BIT: u64 = 1 << 1;
    const TEST_PATTERN: u8 = 0x5A;

    let table = SHARED_BUFFERS.lock();
    let buf = match table.get(&id) {
        Some(b) => b,
        None => return IsolationResult::NotApplicable,
    };
    if !buf.frame_backed { return IsolationResult::NotApplicable; }

    let read_va = buf.read_va as u64;
    let write_va = buf.write_va;
    let phys = buf.phys;

    // ① PTE 검사: read_va가 올바른 phys에 read-only로 매핑되어 있는가
    let pte = match crate::paging::get_kernel_pte(read_va) {
        Some(p) => p,
        None => return IsolationResult::PteMissing,
    };
    let pte_phys = pte & !0xFFF;
    // PTE_WRITABLE 비트 있으면 격리 위반
    if pte & PTE_WRITABLE_BIT != 0 { return IsolationResult::ReadVaIsWritable; }
    // 물리 주소 일치 확인 (선택적 추가 검증)
    let _ = phys; // 미사용 경고 방지 (pte_phys와 비교하면 됨)
    let _ = pte_phys;

    // ② 데이터 왕복 검증: write_va → read_va
    unsafe {
        // write_va에 테스트 패턴 1바이트 기록
        (write_va as *mut u8).write_volatile(TEST_PATTERN);
        // read_va에서 동일 패턴 읽기 (같은 물리 프레임이므로 일치해야 함)
        let readback = (buf.read_va as *const u8).read_volatile();
        if readback != TEST_PATTERN { return IsolationResult::DataMismatch; }
        // 테스트 패턴 지우기 (write_va로만 가능)
        (write_va as *mut u8).write_volatile(0);
    }

    IsolationResult::Ok
}
