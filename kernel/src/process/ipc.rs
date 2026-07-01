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

    // BETA-X-2 5: switchless 경로 — sentinel 없음, yield 없음, 도어벨만 사용
    if let Some(cap_id) = super::ipc_fast::get_channel(sender, to) {
        return super::ipc_cap::send_switchless(cap_id, data);
        // write_fast + sentinel 경로 제거됨:
        // 수신자는 recv()에서 도어벨을 폴링하므로 sentinel 불필요
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
    // BETA-X-2 5: switchless 우선 확인 — 큐 접근 전에 도어벨 폴링
    // 내 PID로 들어오는 fast channel 목록에서 도어벨이 설정된 것을 찾음
    let my_pid = scheduler::current_pid();
    let mut incoming = [(0u64, 0u64); 8];
    let n = super::ipc_fast::find_incoming_channels(my_pid, &mut incoming, 8);
    for &(cap_id, from) in &incoming[..n] {
        let mut buf = [0u8; 64];
        if let Some(len) = super::ipc_cap::poll_switchless(cap_id, &mut buf) {
            return Some(Message {
                sender: from,
                len: len,
                data: buf,
                fast_cap: 0, // 이미 복사됨, cap 노출 불필요
            });
        }
    }

    // 일반 큐 경로 (switchless 없는 채널 또는 sentinel 기반 레거시)
    let msg = scheduler::recv_msg()?;
    // 레거시 fast_cap sentinel 처리 (switchless로 전환 전 생성된 채널 안전망)
    if msg.fast_cap != 0 {
        let filled = super::ipc_cap::read_shared(msg.fast_cap, |data| {
            let copy_len = data.len().min(64);
            let mut m = Message {
                sender: msg.sender,
                len: copy_len,
                data: [0u8; 64],
                fast_cap: msg.fast_cap,
            };
            m.data[..copy_len].copy_from_slice(&data[..copy_len]);
            m
        });
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
