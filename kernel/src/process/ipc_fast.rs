//! BETA-X 2: 임계값 기반 자동 IPC 채널 레지스트리
//!
//! ## 동작 원리
//!
//! Policy Engine이 프로세스 쌍의 IPC 빈도를 관찰하다가 임계값(IPC_HOT_THRESHOLD)을
//! 초과하면 `ensure_channel(from, to)`를 호출한다.
//! 이후 해당 쌍의 `ipc::send()`는 자동으로 fast path를 사용한다:
//!
//! ```text
//! 일반 경로: send() → Message 복사 → message_queue.push_back()
//! fast 경로: send() → overwrite_shared(cap_id, data)  ← 버퍼 재사용, 재할당 없음
//!            → Message { fast_cap=cap_id, len=0 } → message_queue.push_back()
//!            recv() → read_shared(cap_id)          ← SharedBuffer에서 직접 읽기
//! ```
//!
//! ## 최적화 포인트
//!
//! 1. **버퍼 재사용**: 채널 생성 시 512 바이트 버퍼를 한 번만 할당.
//!    이후 메시지마다 새 힙 할당 없이 overwrite.
//! 2. **알림 최소화**: 데이터 없는 8바이트 sentinel 메시지만 전달.
//! 3. **투명한 recv()**: 수신자는 fast_cap 여부를 신경 쓸 필요 없음.
//!    recv()가 투명하게 SharedBuffer 데이터를 msg.data로 채워 반환.
//!
//! ## BETA-X 6 (decay) 연결
//!
//! 통신 빈도가 떨어진 쌍은 `drop_channel(from, to)`로 채널 회수.
//! SharedBuffer도 함께 해제됨.

use alloc::collections::BTreeMap;
use spin::Mutex;

use super::Pid;
use super::ipc_cap::{self, CapId};

/// 전역 fast channel 레지스트리: (from, to) → CapId
static FAST_CHANNELS: Mutex<BTreeMap<(Pid, Pid), CapId>> = Mutex::new(BTreeMap::new());

/// 채널이 없으면 새로 생성, 있으면 기존 CapId 반환 (idempotent).
///
/// 채널 생성 시 512바이트 SharedBuffer를 사전 할당한다.
/// Policy Engine의 `adapt_and_report`에서 임계값 초과 쌍에 대해 호출됨.
pub fn ensure_channel(from: Pid, to: Pid) -> CapId {
    ensure_channel_cap(from, to, 512)
}

/// capacity를 지정해 fast channel 생성 (A-1 페이로드 스윕용).
///
/// 이미 존재하면 기존 cap_id 반환 (idempotent).
pub fn ensure_channel_cap(from: Pid, to: Pid, capacity: usize) -> CapId {
    {
        let table = FAST_CHANNELS.lock();
        if let Some(&cap_id) = table.get(&(from, to)) {
            return cap_id;
        }
    }
    // BETA-X-2 2: 비대칭 권한 매핑 (frame_backed=true)
    // write_va(HHDM) = 쓰기 가능, read_va(RO_CHANNEL_BASE) = 읽기 전용
    let cap_id = ipc_cap::alloc_shared_frame(from, capacity);
    FAST_CHANNELS.lock().insert((from, to), cap_id);
    crate::tracer::channel_created(from as u32, to as u32, cap_id as u32, capacity);

    // BETA-X-2 3: 격리 검증 — read_va PTE_WRITABLE 없음 + write→read 왕복 확인
    let iso = ipc_cap::verify_channel_isolation(cap_id);
    match iso {
        ipc_cap::IsolationResult::Ok => {
            crate::serial_println!(
                "[ipc-X] fast channel 생성: pid{}→pid{} cap={}  (비대칭매핑 {}B, W:HHDM R:RO_WIN) ✓ 격리검증 OK",
                from, to, cap_id, capacity,
            );
        }
        other => {
            crate::tracer::anomaly(from as u32, to as u32, cap_id as u32, other as u64);
            crate::serial_println!(
                "[ipc-X] !! 격리검증 실패: pid{}→pid{} cap={} result={:?}",
                from, to, cap_id, other,
            );
        }
    }
    cap_id
}

/// fast channel이 존재하면 CapId 반환.
#[inline]
pub fn get_channel(from: Pid, to: Pid) -> Option<CapId> {
    FAST_CHANNELS.lock().get(&(from, to)).copied()
}

/// fast channel SharedBuffer에 데이터를 기록 (기존 내용 교체).
///
/// 반환: 성공 여부 (false = CapId 만료 또는 SharedBuffer 없음)
#[inline]
pub fn write_fast(cap_id: CapId, data: &[u8]) -> bool {
    ipc_cap::overwrite_shared(cap_id, data)
}

/// fast channel 회수 (BETA-X 6 decay에서 사용).
///
/// SharedBuffer도 함께 해제된다.
pub fn drop_channel(from: Pid, to: Pid) {
    if let Some(cap_id) = FAST_CHANNELS.lock().remove(&(from, to)) {
        ipc_cap::drop_shared(cap_id);
        crate::tracer::channel_dropped(from as u32, to as u32, cap_id as u32, 0);
        crate::serial_println!(
            "[ipc-X] fast channel 회수: pid{}→pid{} cap={}",
            from, to, cap_id,
        );
    }
}

/// 현재 등록된 fast channel 수 (디버깅/리포트용).
pub fn channel_count() -> usize {
    FAST_CHANNELS.lock().len()
}
