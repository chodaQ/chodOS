//! 선점형 라운드로빈 스케줄러 (Preemptive Round-Robin Scheduler)
//!
//! ## ALPHA M1: 협력형 → 선점형 전환
//!
//! 기존(협력형): 프로세스가 직접 `yield_now()` 호출 → `switch_context_asm`(callee-saved 교환).
//!
//! 신규(선점형):
//!   - 타이머 IRQ0 → `isr32` 스텁(ALL 레지스터 push) → `timer_preempt(rsp) -> rsp`
//!   - `timer_preempt`가 TIME_SLICE 틱마다 `preempt(rsp)`를 호출
//!   - `preempt(rsp)`: 현재 RSP 저장, 다음 Ready 프로세스의 RSP 반환
//!   - `mov rsp, rax` → `iretq` → 새 프로세스 실행
//!
//!   - `yield_now()` = `int 0x40` → `isr64` 스텁 → `voluntary_yield(rsp) -> rsp`
//!     (협력적 양보지만 완전히 동일한 ISR 프레임 형식을 사용)
//!
//! ## 컨텍스트 포맷 통일
//!
//! 선점형/자발적 모두 `preempt_rsp` 필드 하나로 상태를 저장.
//! 스택 레이아웃: [r15, r14, r13, r12, rbp, rbx, r11, r10, r9, r8,
//!                 rdi, rsi, rdx, rcx, rax, RIP, CS, RFLAGS]  (18 × u64)

use alloc::vec::Vec;
use super::{Process, ProcessState, Priority, Pid};

/// 우선순위 → 선택 가중치 (선택 빈도 방식, ALPHA 5)
///
/// 라운드로빈 대신 weighted round-robin:
/// High=4, Normal=2, Low=1, Idle=0
/// 매 컨텍스트 스위치마다 현재 프로세스의 remaining_quanta를 소모.
/// 0이 되면 다음 후보를 탐색하면서 가중치를 재충전.
fn priority_weight(p: Priority) -> u8 {
    match p {
        Priority::High   => 4,
        Priority::Normal => 2,
        Priority::Low    => 1,
        Priority::Idle   => 0,
    }
}

/// 우선순위별 기아(starvation) 임계값 (틱, ALPHA 7 Aging)
///
/// Ready 상태로 이 값 이상 대기하면 effective_priority를 한 단계 올림.
/// High는 어차피 자주 선택되므로 aging 불필요.
fn starvation_threshold(p: Priority) -> Option<u64> {
    match p {
        Priority::Low    => Some(72), // 4초
        Priority::Normal => Some(36), // 2초
        Priority::High   => None,
        Priority::Idle   => None,
    }
}

/// 실제 스케줄링에 사용할 우선순위 계산 (boost + aging 반영).
///
/// 적용 순서 (높은 것 우선):
/// 1. boost_ticks > 0 → High (키보드 입력 긴급 부스트)
/// 2. aging 조건 충족  → 한 단계 상향 (기아 방지)
/// 3. 기본 priority
fn effective_priority(p: &super::Process, now: u64) -> Priority {
    if p.boost_ticks > 0 {
        return Priority::High;
    }
    // aging: Ready 대기 시간이 임계값 초과 시 한 단계 상향
    if let Some(threshold) = starvation_threshold(p.priority) {
        let wait = now.saturating_sub(p.ready_since_tick);
        if wait >= threshold {
            return Priority::from_u8(p.priority as u8 + 1);
        }
    }
    p.priority
}

pub struct Scheduler {
    pub processes: Vec<Process>,
    pub current: usize,
    next_pid: Pid,
    /// 현재 프로세스의 잔여 연속 실행 quanta (ALPHA 5 weighted round-robin)
    remaining_quanta: u8,
}

impl Scheduler {
    pub fn new() -> Self {
        Self { processes: Vec::new(), current: 0, next_pid: 1, remaining_quanta: 1 }
    }

    pub fn alloc_pid(&mut self) -> Pid {
        let pid = self.next_pid;
        self.next_pid += 1;
        pid
    }

    pub fn current_pid(&self) -> Pid {
        self.processes[self.current].pid
    }

    pub fn spawn(&mut self, process: Process) {
        crate::serial_println!("[sched] spawned '{}' (pid={})", process.name, process.pid);
        // Policy engine에 PID 등록 (interrupt context 밖에서 호출되므로 안전)
        crate::policy::register_pid(process.pid);
        self.processes.push(process);
    }

    /// 프로세스를 Dead로 표시 (스케줄러가 이후 건너뜀)
    pub fn kill(&mut self, pid: Pid) {
        for proc in self.processes.iter_mut() {
            if proc.pid == pid {
                proc.state = ProcessState::Dead;
                break;
            }
        }
    }

    /// 현재 프로세스를 Dead로 표시하고 다음 프로세스 RSP 반환.
    ///
    /// 반드시 ISR 컨텍스트(인터럽트 비활성 상태)에서 호출해야 함.
    /// 반환값을 `rsp`로 설정하고 `iretq`로 복귀하면 다음 프로세스로 전환됨.
    pub fn exit_current(&mut self, current_rsp: u64) -> u64 {
        self.processes[self.current].state = ProcessState::Dead;
        self.processes[self.current].preempt_rsp = current_rsp;
        self.remaining_quanta = 0;
        self.switch_to_next(current_rsp, true)
    }

    /// 키보드 인터럽트(ALPHA 6): 현재 포그라운드 프로세스에 boost_ticks 부여.
    pub fn keyboard_boost(&mut self) {
        let cur = self.current;
        // Idle(kernel_main)이면 부스트 의미 없음
        if self.processes[cur].priority == Priority::Idle { return; }
        self.processes[cur].boost_ticks = self.processes[cur].boost_ticks.max(8);
    }

    /// 특정 PID의 우선순위를 설정 (Policy Engine에서 호출).
    pub fn set_priority(&mut self, pid: Pid, pri: Priority) {
        for p in self.processes.iter_mut() {
            if p.pid == pid { p.priority = pri; return; }
        }
    }

    /// 선점형 컨텍스트 스위치 — 타이머/소프트 인터럽트 핸들러에서 호출.
    ///
    /// 1. 현재 RSP를 현재 프로세스의 `preempt_rsp`에 저장
    /// 2. 다음 Ready 프로세스를 라운드로빈으로 탐색
    /// 3. 다음 프로세스의 `preempt_rsp` 반환 (스위치 없으면 same RSP)
    ///
    /// 이 함수 자체는 스택 전환을 하지 않음.
    /// 실제 전환은 호출자(isr32/isr64)의 `mov rsp, rax` 명령어가 수행.
    /// 선점형 컨텍스트 스위치 (타이머 / voluntary_yield 공통 진입점).
    ///
    /// `is_voluntary`: true면 현재 프로세스의 voluntary_yields++, false면 forced_preempts++.
    pub fn do_preempt(&mut self, current_rsp: u64, is_voluntary: bool) -> u64 {
        let cur = self.current;
        let from_pid = self.processes[cur].pid;

        // ALPHA 4: 스케줄링 행동 분류
        if is_voluntary {
            self.processes[cur].voluntary_yields += 1;
        } else {
            self.processes[cur].forced_preempts += 1;
        }

        // 부스트 틱 소모 (ALPHA 6)
        if self.processes[cur].boost_ticks > 0 {
            self.processes[cur].boost_ticks -= 1;
        }

        self.processes[cur].preempt_rsp = current_rsp;
        self.processes[cur].state = ProcessState::Ready;
        // Ready 진입 시각 기록 (aging 대기 시간 측정)
        let now = crate::interrupts::handlers::TICK.load(core::sync::atomic::Ordering::Relaxed);
        self.processes[cur].ready_since_tick = now;

        // voluntary yield는 quanta 잔량 무시하고 즉시 전환
        let new_rsp = self.switch_to_next(current_rsp, is_voluntary);

        let to_pid = self.processes[self.current].pid;
        let tick = crate::interrupts::handlers::TICK.load(
            core::sync::atomic::Ordering::Relaxed
        );
        crate::policy::on_switch(from_pid, to_pid, tick);

        new_rsp
    }

    /// 다음 실행할 프로세스를 weighted round-robin으로 선택 (ALPHA 5).
    ///
    /// ## 알고리즘
    /// 1. 현재 프로세스에 quanta가 남아있고 boost_ticks > 0이면 계속 실행.
    /// 2. 아니면 remaining_quanta를 소모 (-1). 0이 되면 다음 후보 탐색.
    /// 3. 후보 중 가장 높은 우선순위의 Ready 프로세스를 선택.
    ///    동순위면 현재 위치 다음부터 라운드로빈.
    fn switch_to_next(&mut self, current_rsp: u64, force_switch: bool) -> u64 {
        let count = self.processes.len();
        if count <= 1 {
            self.processes[self.current].state = ProcessState::Running;
            return current_rsp;
        }

        // voluntary yield가 아닌 타이머 선점이고 quanta가 남아있으면 계속 실행
        if !force_switch && self.remaining_quanta > 1 {
            self.remaining_quanta -= 1;
            self.processes[self.current].state = ProcessState::Running;
            return current_rsp;
        }

        // 다음 후보 탐색: Ready 중 최고 effective_priority 찾기 (aging 반영)
        let now = crate::interrupts::handlers::TICK.load(core::sync::atomic::Ordering::Relaxed);
        let start = self.current;
        let mut best_idx: Option<usize> = None;
        let mut best_pri = Priority::Idle;

        for i in 1..=count {
            let idx = (start + i) % count;
            let p = &self.processes[idx];
            if p.state != ProcessState::Ready { continue; }

            let effective_pri = effective_priority(p, now);

            if best_idx.is_none() || effective_pri > best_pri {
                best_pri = effective_pri;
                best_idx = Some(idx);
            }
        }

        let next = match best_idx {
            Some(idx) => idx,
            None => {
                self.processes[self.current].state = ProcessState::Running;
                self.remaining_quanta = 1;
                return current_rsp;
            }
        };

        // 선택된 프로세스의 effective_priority로 quanta 충전
        let epri = effective_priority(&self.processes[next], now);
        self.remaining_quanta = priority_weight(epri).max(1);

        self.processes[next].state = ProcessState::Running;
        self.current = next;
        self.processes[next].preempt_rsp
    }

    pub fn recv_message(&mut self) -> Option<super::Message> {
        self.processes[self.current].message_queue.pop_front()
    }

    pub fn send_message(&mut self, to: Pid, msg: super::Message) -> bool {
        for proc in self.processes.iter_mut() {
            if proc.pid == to {
                proc.message_queue.push_back(msg);
                return true;
            }
        }
        false
    }
}

// ── 전역 스케줄러 싱글톤 ──────────────────────────────────────────────────────

static mut SCHEDULER: Option<Scheduler> = None;

pub fn init() {
    unsafe {
        let mut s = Scheduler::new();
        s.processes.push(Process::new_kernel_main());
        SCHEDULER = Some(s);
    }
    crate::policy::register_pid(0);
    crate::serial_println!("[sched] scheduler initialized (kernel_main = pid 0, preemptive)");
}

pub fn spawn(process: Process) {
    unsafe { get().spawn(process); }
}

pub fn alloc_pid() -> Pid {
    unsafe { get().alloc_pid() }
}

/// 선점형 컨텍스트 스위치 (타이머/소프트 인터럽트 핸들러에서 호출).
///
/// `current_rsp`: ISR 스텁이 ALL 레지스터를 push한 직후의 RSP.
/// 반환값: 다음 프로세스의 `preempt_rsp` (스위치 없으면 동일).
/// 타이머 강제 선점 (forced_preempts++)
pub fn preempt(current_rsp: u64) -> u64 {
    unsafe { get().do_preempt(current_rsp, false) }
}

/// 자발적 양보 (voluntary_yields++)
pub fn voluntary_preempt(current_rsp: u64) -> u64 {
    unsafe { get().do_preempt(current_rsp, true) }
}

/// 키보드 입력 부스트 (ALPHA 6)
pub fn keyboard_boost() {
    unsafe { get().keyboard_boost(); }
}

/// 우선순위 설정 (Policy Engine에서 호출, ALPHA 5)
pub fn set_priority(pid: Pid, pri: Priority) {
    unsafe { get().set_priority(pid, pri); }
}

/// 자발적 CPU 양보.
///
/// `int 0x40` 소프트웨어 인터럽트를 발생시켜 `isr64 → voluntary_yield()` 경로를 탄다.
/// 타이머 선점과 동일한 ISR 프레임 형식을 사용하므로 컨텍스트 전환 방식이 통일됨.
#[inline(always)]
pub fn yield_now() {
    unsafe {
        core::arch::asm!("int 0x40", options(nomem, nostack, preserves_flags));
    }
}

/// 현재 프로세스를 종료하고 다음 프로세스로 전환.
///
/// 반드시 ISR 핸들러(`voluntary_yield`)를 통해 간접 호출해야 함.
/// 직접 호출 시 스택 상태가 맞지 않아 패닉.
pub fn exit_current_rsp(current_rsp: u64) -> u64 {
    unsafe { get().exit_current(current_rsp) }
}

pub fn get_stats(pid: Pid) -> Option<(u64, u64)> {
    let s = unsafe { get() };
    s.processes.iter()
        .find(|p| p.pid == pid)
        .map(|p| (p.voluntary_yields, p.forced_preempts))
}

/// 특정 PID를 Dead로 표시
pub fn kill_pid(pid: Pid) {
    unsafe { get().kill(pid); }
}

pub fn current_pid() -> Pid {
    unsafe { get().current_pid() }
}

pub(super) fn recv_msg() -> Option<super::Message> {
    unsafe { get().recv_message() }
}

pub(super) fn send_msg(to: Pid, msg: super::Message) -> bool {
    unsafe { get().send_message(to, msg) }
}

// ── ALPHA 9: Handle Table API ─────────────────────────────────────────────────

/// 현재 프로세스의 핸들 테이블에 Capability를 등록하고 Handle 반환.
pub fn insert_capability<T: core::any::Any + Send + Sync + 'static>(
    cap: super::handle::Capability<T>,
) -> super::handle::Handle {
    unsafe { get().processes[get().current].handle_table.insert(cap) }
}

/// 현재 프로세스의 핸들 테이블에서 `Arc<T>` 조회 (권한 체크 포함).
pub fn get_capability<T: core::any::Any + Send + Sync + 'static>(
    id: u32,
    required: super::handle::Rights,
) -> Option<alloc::sync::Arc<T>> {
    unsafe { get().processes[get().current].handle_table.get::<T>(id, required) }
}

/// 현재 프로세스의 핸들 닫기.
pub fn close_handle(id: u32) -> bool {
    unsafe { get().processes[get().current].handle_table.close(id) }
}

/// 현재 프로세스의 열린 핸들 수.
pub fn handle_count() -> usize {
    unsafe { get().processes[get().current].handle_table.len() }
}

/// 현재 프로세스의 핸들 목록 (id, rights) — 디버깅용.
pub fn list_handles() -> alloc::vec::Vec<(u32, super::handle::Rights)> {
    unsafe { get().processes[get().current].handle_table.list() }
}

unsafe fn get() -> &'static mut Scheduler {
    SCHEDULER.as_mut().expect("scheduler not initialized")
}
