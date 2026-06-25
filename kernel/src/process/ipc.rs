//! IPC — 프로세스 간 메시지 전달 (Inter-Process Communication)
//!
//! ## 설계 원칙 (마이크로커널 관점)
//!
//! 마이크로커널에서 모든 프로세스 간 통신은 메시지로 이루어짐.
//! 파일 읽기, 네트워크 요청, 드라이버 호출 모두 결국 메시지 전송.
//!
//! ## 현재 구현 (Milestone 3 — 단순 메시지 큐)
//!
//! 각 프로세스는 FIFO 메시지 큐를 가짐 (Process.message_queue).
//! - send(to, data): to의 큐 끝에 추가
//! - recv():         현재 프로세스의 큐 앞에서 꺼냄
//!
//! 비동기: send는 즉시 반환 (큐에 넣고 끝).
//!         recv는 큐가 비어있으면 None 반환 (blocking 없음).
//!
//! ## 향후 계획 (Milestone 4+)
//!
//! - Capability 기반 채널: 권한 없으면 send 불가
//! - Blocking recv: 메시지가 올 때까지 Blocked 상태로 대기
//! - 응답 메시지(reply): call() = send + blocking recv
//! - 크기 제한 큐: 가득 차면 send 실패 또는 sender를 block

use super::scheduler;
use super::{Message, Pid};

/// 메시지 전송
///
/// `to`: 수신할 프로세스의 PID.
/// `data`: 전송할 바이트 슬라이스 (최대 64바이트; 초과분은 버림).
///
/// # 반환값
/// - `true`: 전송 성공 (수신자 큐에 메시지 추가됨)
/// - `false`: 해당 PID의 프로세스 없음
///
/// # 비고
/// 비동기 전송: 수신자가 아직 recv()를 호출하지 않아도 즉시 반환.
/// 수신자 큐에 공간이 있는 한 실패하지 않음 (큐 크기 제한 없음).
pub fn send(to: Pid, data: &[u8]) -> bool {
    let sender = scheduler::current_pid();

    // BETA-X 2: fast channel이 있으면 SharedBuffer에 기록 후 sentinel 전송
    if let Some(cap_id) = super::ipc_fast::get_channel(sender, to) {
        if super::ipc_fast::write_fast(cap_id, data) {
            let msg = Message { sender, len: 0, data: [0u8; 64], fast_cap: cap_id };
            return scheduler::send_msg(to, msg);
        }
        // write 실패 시 (CapId 만료) 일반 경로로 fallback
    }

    // 일반 경로: 64바이트 인라인 복사
    let mut msg = Message {
        sender,
        len: data.len().min(64),
        data: [0u8; 64],
        fast_cap: 0,
    };
    msg.data[..msg.len].copy_from_slice(&data[..msg.len]);
    scheduler::send_msg(to, msg)
}

/// 메시지 수신 (현재 프로세스의 큐에서 꺼냄)
///
/// # 반환값
/// - `Some(Message)`: 수신된 메시지
/// - `None`:          큐가 비어있음 (메시지 없음)
///
/// # 비고
/// 논블로킹: 큐가 비면 바로 None 반환.
/// 블로킹이 필요하면 호출자가 직접 루프+yield 패턴을 써야 함:
///
/// ```rust
/// loop {
///     if let Some(msg) = ipc::recv() {
///         // 처리
///         break;
///     }
///     scheduler::yield_now(); // 메시지 올 때까지 다른 프로세스에 양보
/// }
/// ```
pub fn recv() -> Option<Message> {
    let msg = scheduler::recv_msg()?;

    // BETA-X 2: fast channel 알림 → SharedBuffer에서 실제 데이터를 투명하게 채움
    if msg.fast_cap != 0 {
        let filled = super::ipc_cap::read_shared(msg.fast_cap, |data| {
            let mut m = Message {
                sender: msg.sender,
                len: data.len().min(64),
                data: [0u8; 64],
                fast_cap: msg.fast_cap, // 보존: 64B 초과 데이터는 직접 read_shared 사용
            };
            m.data[..m.len].copy_from_slice(&data[..m.len]);
            m
        });
        // SharedBuffer가 이미 해제됐으면 sentinel 그대로 반환 (안전 fallback)
        return Some(filled.unwrap_or(msg));
    }

    Some(msg)
}

/// u64 값을 IPC 메시지로 전송하는 헬퍼
pub fn send_u64(to: Pid, value: u64) -> bool {
    send(to, &value.to_le_bytes())
}

/// IPC 메시지에서 u64 값 추출 헬퍼
pub fn msg_as_u64(msg: &Message) -> u64 {
    let mut bytes = [0u8; 8];
    let len = msg.len.min(8);
    bytes[..len].copy_from_slice(&msg.data[..len]);
    u64::from_le_bytes(bytes)
}
