//! 프로세스 서브시스템
//!
//! ## 구조
//! - `mod.rs`       — Process PCB, Message, ProcessState 타입 정의
//! - `context.rs`   — CPU 레지스터 스냅샷 (참조용, 현재는 직접 미사용)
//! - `scheduler.rs` — 선점형 라운드로빈 스케줄러 (ALPHA M1)
//! - `ipc.rs`       — 복사 기반 소형 메시지 IPC
//! - `ipc_cap.rs`   — Zero-copy capability IPC (ALPHA M2)
//!
//! ## ALPHA M1 이후 컨텍스트 스위치 방식
//!
//! 기존(협력형): `switch_context_asm`으로 callee-saved 레지스터만 교환.
//! 새(선점형):   타이머 ISR의 어셈블리 스텁이 ALL 레지스터를 스택에 저장.
//!               Rust의 `preempt(rsp) -> rsp` 함수가 스택 포인터만 교환.
//!               `iretq`가 새 프로세스의 스택에서 RIP/CS/RFLAGS를 꺼내 복귀.
//!
//! ## 초기 스택 프레임 레이아웃 (새 프로세스, ring0 커널 스레드)
//!
//! 타이머 ISR(ring0 인터럽트)이 스택에 쌓는 구조를 미리 흉내냄.
//! CPU가 ring0 인터럽트 시 push하는 항목: RFLAGS, CS, RIP (3개, 24바이트)
//! ISR 스텁이 추가로 push: rax, rcx, rdx, rsi, rdi, r8-r11, rbx, rbp, r12-r15 (15개)
//!
//! ```text
//! [stack_top - 8]   = RFLAGS = 0x202     ← 인터럽트 활성화(IF=1)
//! [stack_top - 16]  = CS     = 0x08      ← 커널 코드 세그먼트
//! [stack_top - 24]  = RIP    = entry_fn  ← iretq가 여기로 점프
//! [stack_top - 32]  = rax    = 0
//! [stack_top - 40]  = rcx    = 0
//! [stack_top - 48]  = rdx    = 0
//! [stack_top - 56]  = rsi    = 0
//! [stack_top - 64]  = rdi    = 0
//! [stack_top - 72]  = r8     = 0
//! [stack_top - 80]  = r9     = 0
//! [stack_top - 88]  = r10    = 0
//! [stack_top - 96]  = r11    = 0
//! [stack_top - 104] = rbx    = 0
//! [stack_top - 112] = rbp    = 0
//! [stack_top - 120] = r12    = 0
//! [stack_top - 128] = r13    = 0
//! [stack_top - 136] = r14    = 0
//! [stack_top - 144] = r15    = 0         ← preempt_rsp 가 여기를 가리킴
//! ```
//!
//! `iretq` 직전 RSP = preempt_rsp + 120 → RIP/CS/RFLAGS 차례로 팝 → entry_fn 실행 시작.

pub mod context;
pub mod handle;
pub mod ipc;
pub mod ipc_cap;
pub mod ipc_fast; // BETA-X 2: 동적 fast channel 레지스트리
pub mod scheduler;
pub mod userproc; // BETA 9: fork/exec/wait4
pub mod vma;      // BETA 16~17: VMA 테이블 + demand paging

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::interrupts::gdt::KERNEL_CODE_SEL;

/// 프로세스 ID
pub type Pid = u64;

/// 커널 스택 크기: 64KB
const KERNEL_STACK_SIZE: usize = 64 * 1024;

/// ISR 스텁이 저장하는 레지스터 수 (15개 범용 레지스터)
const ISR_REG_COUNT: usize = 15;
/// ring0 iretq 프레임 크기 (RIP + CS + RFLAGS = 3 × 8바이트)
const IRETQ_FRAME_SIZE: usize = 3;
/// 초기 선점형 컨텍스트 프레임 총 크기 (바이트)
///
/// = (ISR_REG_COUNT + IRETQ_FRAME_SIZE) × 8
pub const PREEMPT_FRAME_BYTES: usize = (ISR_REG_COUNT + IRETQ_FRAME_SIZE) * 8; // 144

/// 프로세스 실행 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Ready,
    Running,
    Blocked,
    Dead,
}

/// 스케줄링 우선순위 (ALPHA 5)
///
/// I/O바운드 → High (자주 선택), CPU바운드 → Low (드물게 선택).
/// Linux CFS의 interactive task detection과 동일한 방향.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Idle   = 0, // kernel idle — hlt 대기, 스케줄 최하위
    Low    = 1, // CPU바운드 — 드물게 선택
    Normal = 2, // 기본값
    High   = 3, // I/O바운드, 키보드 포그라운드 — 자주 선택
}

impl Priority {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Priority::Idle,
            1 => Priority::Low,
            3 => Priority::High,
            _ => Priority::Normal,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Priority::Idle   => "Idle",
            Priority::Low    => "Low",
            Priority::Normal => "Normal",
            Priority::High   => "High",
        }
    }
}

/// IPC 메시지 (복사 기반 소형 메시지)
#[derive(Clone)]
pub struct Message {
    pub sender: Pid,
    pub len: usize,
    /// 64바이트 고정 페이로드
    pub data: [u8; 64],
    /// BETA-X 2: 0 = 일반 메시지, >0 = fast channel CapId (데이터는 SharedBuffer에 있음)
    pub fast_cap: u64,
}

/// 프로세스 제어 블록 (PCB)
pub struct Process {
    pub pid: Pid,
    pub name: &'static str,
    pub state: ProcessState,
    /// 스케줄링 우선순위 (ALPHA 5)
    pub priority: Priority,
    /// 선점형 컨텍스트 스위치에 사용되는 저장된 스택 포인터.
    pub preempt_rsp: u64,
    /// 스택 메모리 (힙에 할당, 드롭 시 자동 해제)
    pub kernel_stack: Vec<u8>,
    /// IPC 수신 큐
    pub message_queue: VecDeque<Message>,

    // ── ALPHA 4: 스케줄링 행동 분류 카운터 ───────────────────────────────
    /// 타이머에 의해 강제 선점된 횟수 → CPU바운드 지표
    pub forced_preempts: u64,
    /// 자발적으로 CPU를 양보한 횟수 (yield_now) → I/O바운드 지표
    pub voluntary_yields: u64,
    /// 키보드 부스트 잔여 틱 (0이면 부스트 없음, ALPHA 6)
    pub boost_ticks: u64,
    /// Ready 상태로 진입한 틱 (aging 대기 시간 측정, ALPHA 7)
    pub ready_since_tick: u64,
    // ── ALPHA 9: Capability Handle Table ─────────────────────────────────────
    /// Linux fd ↔ Handle ↔ Capability<T> 연결 테이블
    pub handle_table: handle::HandleTable,

    // ── BETA-X 5: Core Affinity ───────────────────────────────────────────────
    /// 선호 CPU 코어 (0=BSP, u8::MAX=미설정). Policy Engine이 hot pair에 배정.
    pub preferred_cpu: u8,

    // ── BETA-X 1: IPC 빈도 계측 (voluntary_yield/forced_preempt와 같은 자리) ──
    /// 최근 IPC 수신자 PID (최대 8개, count=0이면 미사용)
    pub ipc_peer_pids: [Pid; 8],
    /// ipc_peer_pids[i]에게 보낸 메시지 누적 횟수
    pub ipc_peer_counts: [u64; 8],
}

impl Process {
    /// 새 커널 스레드 프로세스 생성.
    ///
    /// 초기 스택에 "타이머 ISR이 중단한 것처럼" 보이는 가짜 프레임을 구성한다.
    /// 이 프로세스가 처음 스케줄되면:
    ///   1. ISR 스텁이 `mov rsp, preempt_rsp` → 이 스택으로 전환
    ///   2. `pop r15..rax` → rax = entry_fn, 나머지 0으로 복원
    ///   3. `iretq` → RIP=process_start(트램폴린), CS=0x08, RFLAGS=0x202
    ///   4. process_start 트램폴린: 'T' 출력 후 `jmp rax` → entry_fn 실행 시작
    pub fn new(pid: Pid, name: &'static str, entry_fn: fn() -> !) -> Self {
        // process_start: handlers.rs global_asm에 정의된 어셈블리 트램폴린
        extern "C" { fn process_start(); }

        let mut stack = Vec::with_capacity(KERNEL_STACK_SIZE);
        stack.resize(KERNEL_STACK_SIZE, 0u8);
        let stack_top = stack.as_ptr() as u64 + KERNEL_STACK_SIZE as u64;

        // 스택의 상단에서 PREEMPT_FRAME_BYTES 만큼 내려와서 프레임 시작
        let frame_start = stack_top - PREEMPT_FRAME_BYTES as u64;
        let frame = frame_start as *mut u64;

        unsafe {
            // 인덱스 0..13 = r15..rcx → 0
            for i in 0..ISR_REG_COUNT - 1 {
                *frame.add(i) = 0;
            }
            // 인덱스 5 = rbx: stack_top 저장 (process_start 트램폴린이 `mov rsp, rbx`로 RSP 복원)
            // iretq가 ring-change 팝(5개)을 수행하는 경우 RSP=0이 될 수 있으므로
            // rbx에 stack_top을 미리 적어 두어 process_start에서 명시적으로 RSP를 교정.
            *frame.add(5) = stack_top;
            // 인덱스 14 = rax: entry_fn 주소 (트램폴린이 `jmp rax`로 진입)
            *frame.add(14) = entry_fn as *const () as u64;
            // 인덱스 15 = RIP: process_start 트램폴린 (iretq가 여기로 점프)
            *frame.add(15) = process_start as *const () as u64;
            // 인덱스 16 = CS: 커널 코드 세그먼트 (DPL=0)
            *frame.add(16) = KERNEL_CODE_SEL as u64;
            // 인덱스 17 = RFLAGS: 인터럽트 활성(IF=1), 다른 비트는 기본값
            *frame.add(17) = 0x202;
        }

        Process {
            pid,
            name,
            state: ProcessState::Ready,
            priority: Priority::Normal,
            preempt_rsp: frame_start,
            kernel_stack: stack,
            message_queue: VecDeque::new(),
            forced_preempts: 0,
            voluntary_yields: 0,
            boost_ticks: 0,
            ready_since_tick: 0,
            handle_table: handle::HandleTable::new(),
            preferred_cpu: u8::MAX, // 미설정
            ipc_peer_pids: [0; 8],
            ipc_peer_counts: [0; 8],
        }
    }

    pub fn new_kernel_main() -> Self {
        Process {
            pid: 0,
            name: "kernel_main",
            state: ProcessState::Running,
            priority: Priority::Normal, // Policy Engine이 hlt 루프 진입 후 Idle로 강등
            preempt_rsp: 0,
            kernel_stack: Vec::new(),
            message_queue: VecDeque::new(),
            forced_preempts: 0,
            voluntary_yields: 0,
            boost_ticks: 0,
            ready_since_tick: 0,
            handle_table: handle::HandleTable::new(),
            preferred_cpu: 0, // kernel_main은 BSP(core 0) 고정
            ipc_peer_pids: [0; 8],
            ipc_peer_counts: [0; 8],
        }
    }

    /// BETA-X 1: IPC 송신 카운터 갱신 — 수신자 PID별 누적 횟수 반환.
    pub fn record_ipc_send(&mut self, to: Pid) -> u64 {
        // 기존 슬롯 검색
        for i in 0..8 {
            if self.ipc_peer_counts[i] > 0 && self.ipc_peer_pids[i] == to {
                self.ipc_peer_counts[i] = self.ipc_peer_counts[i].saturating_add(1);
                return self.ipc_peer_counts[i];
            }
        }
        // 빈 슬롯(count=0) 사용
        for i in 0..8 {
            if self.ipc_peer_counts[i] == 0 {
                self.ipc_peer_pids[i] = to;
                self.ipc_peer_counts[i] = 1;
                return 1;
            }
        }
        // 슬롯 가득 참 — 최소 카운트 슬롯 교체
        let mut min_i = 0;
        for i in 1..8 {
            if self.ipc_peer_counts[i] < self.ipc_peer_counts[min_i] {
                min_i = i;
            }
        }
        self.ipc_peer_pids[min_i] = to;
        self.ipc_peer_counts[min_i] = 1;
        1
    }
}
