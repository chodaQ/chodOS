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

/// 기아 방지 하한선 (틱) — 실험 31에서 발견한 starvation 버그 수정.
///
/// 기존 aging은 딱 한 단계만 우선순위를 올렸다(Low→Normal, Normal→High).
/// 그런데 Ready&High 프로세스가 영원히 존재하면(예: yield_now() busy-loop
/// IPC 프로세스 쌍) Low 프로세스는 aging으로 Normal까지만 오를 수 있어
/// High를 절대 못 이기고 무기한 스케줄되지 못했다 — 실측: 8-2절(실험 29
/// 후속) 300틱 대기 루프가 8000+틱이 지나도 종료 안 됨.
///
/// 이 값 이상 계속 Ready 대기 중이면 원래 우선순위가 무엇이든 무조건
/// High로 강제 승격해 스케줄을 보장한다. `starvation_threshold` 값들보다
/// 충분히 커서(Normal 36틱의 ~5배) 정상적인 ST-1~4 튜닝 범위(TIME_SLICE
/// 1~4틱, report_interval 8~72틱)에서는 절대 트리거되지 않고, 진짜
/// 기아 상황에서만 개입하는 안전망 역할만 한다.
const FAIRNESS_FLOOR_TICKS: u64 = 200;

/// 실제 스케줄링에 사용할 우선순위 계산 (boost + aging + 기아 방지 하한선 반영).
///
/// 적용 순서 (높은 것 우선):
/// 1. boost_ticks > 0 → High (키보드 입력 긴급 부스트)
/// 2. 기아 방지 하한선 초과 → High 강제 승격 (starvation 방지, 실험 31)
/// 3. aging 조건 충족  → 한 단계 상향 (기존 ALPHA 7)
/// 4. 기본 priority
fn effective_priority(p: &super::Process, now: u64) -> Priority {
    if p.boost_ticks > 0 {
        return Priority::High;
    }
    let wait = now.saturating_sub(p.ready_since_tick);
    // Idle은 "할 일 없을 때만 도는" 의도된 최하위 루프이므로 기아 방지 대상에서 제외.
    if p.priority != Priority::Idle && wait >= FAIRNESS_FLOOR_TICKS {
        return Priority::High;
    }
    // aging: Ready 대기 시간이 임계값 초과 시 한 단계 상향
    if let Some(threshold) = starvation_threshold(p.priority) {
        if wait >= threshold {
            return Priority::from_u8(p.priority as u8 + 1);
        }
    }
    p.priority
}

// ── CFS-1: vruntime 기반 스케줄러 A/B (실험 33) ────────────────────────────────
//
// PE-5(실험 19)에서 "Linux CFS가 처리량 희생 없이 비슷한 반응성을 달성 —
// MuKernel의 이진 High/Low 분류가 구조적 한계"라는 결론을 냈고, 실험 31/32에서
// 기존 weighted round-robin + aging이 구조적으로 starvation에 취약함을 실측
// 확인했다(FAIRNESS_FLOOR_TICKS로 봉합). CFS의 "항상 vruntime이 가장 작은
// Ready 프로세스를 고른다" 원칙은 실행을 못 받은 프로세스의 vruntime이
// 그대로 있어 시간이 지날수록 자동으로 가장 매력적인 후보가 되므로, 별도
// 안전장치 없이 starvation을 구조적으로 없앤다. Linux CFS를 "이긴다"가
// 목표가 아니라(수십 년 튜닝된 물건과 붙는 건 비현실적) 이 구조적 차이를
// 정직하게 비교하는 것이 목표 — 기존 WeightedPriority 경로는 그대로 두고
// 런타임 A/B 토글로 추가한다(PE-4의 policy::set_enabled() 패턴과 동일).

use core::sync::atomic::{AtomicU8, Ordering as AtomicOrdering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedMode {
    WeightedPriority = 0,
    Cfs = 1,
}

// 기본값 WeightedPriority (실험 44에서 CFS로 바꿔봤다가 롤백 — 실험 45
// 참고). CFS-1/CFS-2(실험 33/34) 벤치마크 자체는 유효하지만, CFS를
// 기본값으로 켠 채 전체 데모를 완주시켜보니 BETA-X 3(WM↔GFX fast
// channel)의 Policy Engine 승격 조건이 CFS의 타이머 선점 방식(quanta
// 없이 매 틱마다 min-vruntime 재선택)과 상호작용해 fast channel이
// 끝까지 승격되지 않고 무한정 일반 IPC 경로로만 도는 회귀를 발견 —
// 힙 고갈로 OOM 패닉까지 이어짐. 이 상호작용의 근본 원인은 아직
// 미해결이라, 안전하게 기본값을 원상복구했다. CFS는 계속
// set_mode(SchedMode::Cfs)로 켤 수 있는 A/B 옵션으로 유지.
static SCHED_MODE: AtomicU8 = AtomicU8::new(SchedMode::WeightedPriority as u8);

pub fn set_mode(mode: SchedMode) {
    SCHED_MODE.store(mode as u8, AtomicOrdering::SeqCst);
    // CFS 모드 진입마다 새 공정성 epoch 시작 (실험 34 — rebase_vruntime 주석 참고)
    if mode == SchedMode::Cfs {
        unsafe { get().rebase_vruntime(); }
    }
}

pub fn mode() -> SchedMode {
    if SCHED_MODE.load(AtomicOrdering::Relaxed) == SchedMode::Cfs as u8 {
        SchedMode::Cfs
    } else {
        SchedMode::WeightedPriority
    }
}

/// CFS 모드 전용 가중치 — Linux nice 값 스타일의 지수적 가중치.
/// `priority_weight`(quanta 개수용, 1/2/4)와는 의미가 다름: 이건 "vruntime이
/// 얼마나 천천히 느냐" = 평균 CPU 점유율에 직결된다. NICE0_WEIGHT(=1024,
/// Normal 기준)보다 크면 그만큼 vruntime이 느리게 늘어 더 자주 선택된다.
const NICE0_WEIGHT: u64 = 1024;
fn cfs_weight(p: Priority) -> u64 {
    match p {
        Priority::High   => 88761, // Linux nice -10 상당 — 훨씬 자주 선택
        Priority::Normal => 1024,  // nice 0 기준
        Priority::Low    => 335,   // nice 5 상당
        Priority::Idle   => 15,    // nice 19 상당 — 남는 시간에만
    }
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

    /// CFS 모드 진입 시 모든 프로세스의 vruntime을 0으로 리베이스한다.
    ///
    /// 실험 34(CFS-2)에서 발견: 반복 시행(WP 3회 → CFS 3회)에서 뒤로 갈수록
    /// CFS 시행의 kbd_lat_avg가 0으로 나오는 문제를 발견했다. 원인은
    /// kernel_main이 오케스트레이터로서 CFS 모드가 켜져 있는 내내
    /// `cur`로서 계속 vruntime을 누적하는데, 매 시행마다 새로 spawn되는
    /// hog/kbd는 그때그때 "현재 Ready 중 최소 vruntime"(대개 0에 가까움)로
    /// 시작해서 — 시행이 거듭될수록 kernel_main의 누적 vruntime이 새
    /// 프로세스들보다 계속 커져 kernel_main 자신이 점점 더 심하게 밀려남.
    /// kernel_main이 못 돌면 벤치마크 오케스트레이션 자체(키입력 트리거
    /// 기록)가 멈춰버림 — CFS의 "공정함"이 오히려 벤치마크 하네스를
    /// 굶기는 역설. Linux CFS가 `min_vruntime`을 주기적으로 재기준하는
    /// 것과 같은 이유로, 여기서는 "CFS 모드 진입 = 새 공정성 epoch 시작"
    /// 의미로 진입 시점마다 전체 리베이스한다.
    fn rebase_vruntime(&mut self) {
        for p in self.processes.iter_mut() {
            p.vruntime = 0;
        }
    }

    pub fn alloc_pid(&mut self) -> Pid {
        let pid = self.next_pid;
        self.next_pid += 1;
        pid
    }

    pub fn current_pid(&self) -> Pid {
        self.processes[self.current].pid
    }

    pub fn spawn(&mut self, mut process: Process) {
        crate::serial_println!("[sched] spawned '{}' (pid={})", process.name, process.pid);
        // Policy engine에 PID 등록 (interrupt context 밖에서 호출되므로 안전)
        crate::policy::register_pid(process.pid);
        // CFS-1(실험 33): 새 프로세스를 vruntime=0으로 그냥 넣으면 기존
        // 프로세스들의 누적 vruntime을 무시하고 당분간 스케줄러를 독점하게
        // 된다(Linux의 `place_entity`와 동일 문제). 현재 Ready 프로세스들 중
        // 최소 vruntime으로 맞춰서 시작한다.
        if mode() == SchedMode::Cfs {
            let min_vr = self.processes.iter()
                .filter(|p| p.state == ProcessState::Ready)
                .map(|p| p.vruntime)
                .min();
            if let Some(v) = min_vr {
                process.vruntime = v;
            }
        }
        self.processes.push(process);
    }

    /// 프로세스를 Dead로 표시 (스케줄러가 이후 건너뜀)
    ///
    /// ST-3 버그 수정 (실험 24 후속): 죽은 프로세스를 Policy Engine에도
    /// 통보해 stats 슬롯을 비활성화한다. 그렇지 않으면 죽기 직전의
    /// Priority(대개 High)로 영원히 재분류되어 cnt_high가 구조적으로
    /// 줄어들지 않는 누수가 생긴다 (자세한 내용은 policy::unregister 주석 참고).
    pub fn kill(&mut self, pid: Pid) {
        for proc in self.processes.iter_mut() {
            if proc.pid == pid {
                proc.state = ProcessState::Dead;
                break;
            }
        }
        crate::policy::unregister_pid(pid);
    }

    /// Dead 프로세스의 힙 자원 중 안전한 것(message_queue/handle_table)만
    /// 회수한다 (실험 34/35). `kernel_stack`(64KB)은 의도적으로 건드리지
    /// 않는다 — `Process::reap()` 문서 주석 참고.
    ///
    /// **호출은 인터럽트 컨텍스트 밖(kernel_main의 일반 실행 흐름)에서
    /// 할 것.** (처음엔 ISR 재진입이 원인이라 추정해 회수 로직을 여기로
    /// 옮겼었는데, 실제로는 kernel_stack을 free하는 것 자체가 문제였고
    /// 위치는 무관했다 — 그래도 힙을 건드리는 코드는 일반 컨텍스트에
    /// 두는 게 여러모로 안전하므로 이 위치는 유지한다.)
    ///
    /// 자기 자신(`self.current`)은 제외한다 — 방어적으로 남겨둠(이론상
    /// 일반 컨텍스트에서 자기 자신이 Dead인 채로 이 함수를 호출할 일은 없음,
    /// Dead 전환은 `exit_current`를 통해서만 일어나고 즉시 다른 프로세스로
    /// 전환되므로).
    pub fn reap_dead(&mut self) {
        let keep = self.current;
        for (i, p) in self.processes.iter_mut().enumerate() {
            if i != keep && p.state == ProcessState::Dead && !p.reaped {
                p.reap();
            }
        }
    }

    /// 현재 프로세스를 Dead로 표시하고 다음 프로세스 RSP 반환.
    ///
    /// 반드시 ISR 컨텍스트(인터럽트 비활성 상태)에서 호출해야 함.
    /// 반환값을 `rsp`로 설정하고 `iretq`로 복귀하면 다음 프로세스로 전환됨.
    pub fn exit_current(&mut self, current_rsp: u64) -> u64 {
        let pid = self.processes[self.current].pid;
        self.processes[self.current].state = ProcessState::Dead;
        self.processes[self.current].preempt_rsp = current_rsp;
        self.remaining_quanta = 0;
        crate::policy::unregister_pid(pid); // kill()과 동일한 stale-slot 방지
        self.switch_to_next(current_rsp, true)
    }

    /// 키보드 인터럽트(ALPHA 6): 현재 포그라운드 프로세스에 boost_ticks 부여.
    ///
    /// 실제 IRQ1 핸들러에서만 의미가 있다 — 그 시점의 `self.current`가
    /// "인터럽트당한 진짜 포그라운드 프로세스"이기 때문. 시뮬레이션 코드처럼
    /// kernel_main이 직접 호출하면 kernel_main 자신이 `self.current`라서
    /// 엉뚱하게 자기 자신을 부스트하게 된다 — 특정 프로세스를 부스트하려면
    /// `boost_pid()`를 쓸 것 (실험 33 CFS-1에서 발견: bench_pe4의 kbd_task
    /// 시뮬레이션이 이 함수를 직접 호출해 kernel_main을 부스트하고 있었고,
    /// WeightedPriority에서는 라운드로빈으로 묻혔지만 CFS의 큰 weight
    /// 격차 때문에 kernel_main이 무기한 스케줄을 독점하는 형태로 드러남).
    pub fn keyboard_boost(&mut self) {
        let cur = self.current;
        // Idle(kernel_main)이면 부스트 의미 없음
        if self.processes[cur].priority == Priority::Idle { return; }
        self.processes[cur].boost_ticks = self.processes[cur].boost_ticks.max(8);
    }

    /// 특정 PID에 boost_ticks 부여 — "누가 인터럽트당했는지"가 아니라
    /// "어떤 프로세스를 부스트할지"를 명시적으로 아는 경우(bench_pe4의
    /// kbd_task 시뮬레이션 등)에 사용.
    pub fn boost_pid(&mut self, pid: Pid, ticks: u64) {
        for p in self.processes.iter_mut() {
            if p.pid == pid {
                if p.priority == Priority::Idle { return; }
                p.boost_ticks = p.boost_ticks.max(ticks);
                return;
            }
        }
    }

    /// 특정 PID의 우선순위를 설정 (Policy Engine에서 호출).
    pub fn set_priority(&mut self, pid: Pid, pri: Priority) {
        for p in self.processes.iter_mut() {
            if p.pid == pid { p.priority = pri; return; }
        }
    }

    pub fn get_priority(&self, pid: Pid) -> Priority {
        for p in self.processes.iter() {
            if p.pid == pid { return p.priority; }
        }
        Priority::Normal
    }

    /// 현재 프로세스에 대해 다음 voluntary yield에서 소비할 sleep 요청을
    /// 심어둔다 (`sleep_ticks(n)` 전역 함수의 내부 구현).
    pub fn request_sleep(&mut self, ticks: u64) {
        let cur = self.current;
        self.processes[cur].pending_sleep_ticks = ticks.max(1);
    }

    /// 특정 PID의 선호 CPU를 설정 (BETA-X 5 core affinity).
    pub fn set_preferred_cpu(&mut self, pid: Pid, cpu: u8) {
        for p in self.processes.iter_mut() {
            if p.pid == pid { p.preferred_cpu = cpu; return; }
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
    /// 반환값: `(new_rsp, from_pid, to_pid, tick)`.
    ///
    /// `policy::on_switch()` 호출은 일부러 여기서 하지 않는다 — 이 메서드는
    /// 살아있는 `&mut self`(전역 `SCHEDULER`에 대한 참조)를 쥔 채 실행 중인데,
    /// `policy::on_switch()`가 내부적으로 `scheduler::set_priority()` 등을
    /// 재진입 호출하면 그것들이 각자 `unsafe fn get()`으로 같은 전역에 대한
    /// *두 번째* `&mut`을 새로 만들어버린다 — Rust aliasing 모델 위반(실험
    /// 36에서 발견된 잠재 UB). 대신 이 메서드는 필요한 값만 반환하고,
    /// 호출자(`preempt()`/`voluntary_preempt()`)가 `get().do_preempt(...)`
    /// 문장이 끝나 `&mut Scheduler` 대여가 완전히 해제된 뒤에
    /// `policy::on_switch()`를 호출한다.
    pub fn do_preempt(&mut self, current_rsp: u64, is_voluntary: bool) -> (u64, Pid, Pid, u64) {
        let cur = self.current;
        let from_pid = self.processes[cur].pid;
        SWITCH_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

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
        let now = crate::interrupts::handlers::TICK.load(core::sync::atomic::Ordering::Relaxed);

        // CFS-1(실험 33): outgoing 프로세스의 vruntime 갱신. rdtsc 기반 —
        // TICK은 voluntary yield/sleep_ticks에서 안 늘어나므로(실험 30~32),
        // 실제 소비한 CPU 사이클을 반영하려면 rdtsc가 필요하다.
        if mode() == SchedMode::Cfs {
            let now_ts = crate::tracer::rdtsc();
            let elapsed = now_ts.saturating_sub(self.processes[cur].run_start_ts);
            let w = cfs_weight(effective_priority(&self.processes[cur], now)).max(1);
            let delta = elapsed.saturating_mul(NICE0_WEIGHT) / w;
            self.processes[cur].vruntime = self.processes[cur].vruntime.saturating_add(delta);
        }

        // sleep_ticks(n): pending_sleep_ticks가 설정돼 있으면 Ready 대신
        // Blocked로 전환 — switch_to_next의 후보 탐색에서 완전히 제외되므로
        // (실험 30과 달리) 이 프로세스는 wake_at_tick까지 idle_pct 계산에서
        // 진짜로 "쉬는" 것으로 잡힌다.
        let sleep_req = self.processes[cur].pending_sleep_ticks;
        if sleep_req > 0 {
            self.processes[cur].pending_sleep_ticks = 0;
            self.processes[cur].state = ProcessState::Blocked;
            self.processes[cur].wake_at_tick = now + sleep_req;
        } else {
            self.processes[cur].state = ProcessState::Ready;
            // Ready 진입 시각 기록 (aging 대기 시간 측정)
            self.processes[cur].ready_since_tick = now;
        }

        // voluntary yield는 quanta 잔량 무시하고 즉시 전환
        let new_rsp = self.switch_to_next(current_rsp, is_voluntary);

        let to_pid = self.processes[self.current].pid;
        let tick = crate::interrupts::handlers::TICK.load(
            core::sync::atomic::Ordering::Relaxed
        );

        (new_rsp, from_pid, to_pid, tick)
    }

    /// 다음 실행할 프로세스를 weighted round-robin으로 선택 (ALPHA 5).
    ///
    /// ## 알고리즘
    /// 1. 현재 프로세스에 quanta가 남아있고 boost_ticks > 0이면 계속 실행.
    /// 2. 아니면 remaining_quanta를 소모 (-1). 0이 되면 다음 후보 탐색.
    /// 3. 후보 중 가장 높은 우선순위의 Ready 프로세스를 선택.
    ///    동순위면 현재 위치 다음부터 라운드로빈.
    fn switch_to_next(&mut self, current_rsp: u64, force_switch: bool) -> u64 {
        let now = crate::interrupts::handlers::TICK.load(core::sync::atomic::Ordering::Relaxed);

        // sleep_ticks(n) 깨우기: wake_at_tick이 만료된 Blocked 프로세스를
        // Ready로 되돌린다. 매 스위치마다 전체 스캔하므로(별도 타이머 큐
        // 없음) 최악의 경우 만료 후 최대 1 TIME_SLICE만큼 깨어남이 지연될
        // 수 있음 — 이 커널의 틱 단위 정밀도(수십 ms급)에서는 무시 가능.
        for p in self.processes.iter_mut() {
            if p.state == ProcessState::Blocked && p.wake_at_tick != 0 && now >= p.wake_at_tick {
                p.state = ProcessState::Ready;
                p.wake_at_tick = 0;
                p.ready_since_tick = now;
            }
        }

        let count = self.processes.len();
        if count <= 1 {
            self.processes[self.current].state = ProcessState::Running;
            return current_rsp;
        }

        let cfs = mode() == SchedMode::Cfs;

        // voluntary yield가 아닌 타이머 선점이고 quanta가 남아있으면 계속 실행.
        // CFS 모드는 quanta 개념이 없음 — 매 스위치마다 vruntime을 새로 비교.
        if !cfs && !force_switch && self.remaining_quanta > 1 {
            self.remaining_quanta -= 1;
            self.processes[self.current].state = ProcessState::Running;
            return current_rsp;
        }

        if cfs {
            return self.switch_to_next_cfs(current_rsp);
        }

        // 다음 후보 탐색: Ready 중 최고 effective_priority 찾기 (aging 반영)
        // BETA-X 5: 동순위 시 현재 코어 affinity 선호 프로세스 우선
        let my_cpu = crate::smp::current_cpu_id();
        let start  = self.current;
        let mut best_idx:      Option<usize> = None;
        let mut best_pri       = Priority::Idle;
        let mut best_same_cpu  = false; // 현재 best가 같은 코어 선호인지

        for i in 1..=count {
            let idx = (start + i) % count;
            let p = &self.processes[idx];
            if p.state != ProcessState::Ready { continue; }

            let effective_pri = effective_priority(p, now);
            // 미설정(u8::MAX) = 어느 코어든 무관 → same_cpu로 간주
            let same_cpu = p.preferred_cpu == u8::MAX || p.preferred_cpu == my_cpu;

            let better = match best_idx {
                None => true,
                Some(_) => {
                    effective_pri > best_pri
                    || (effective_pri == best_pri && same_cpu && !best_same_cpu)
                }
            };
            if better {
                best_pri      = effective_pri;
                best_idx      = Some(idx);
                best_same_cpu = same_cpu;
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
        // CFS 모드로 전환될 경우를 대비해 run_start_ts는 항상 갱신해둔다
        // (그렇지 않으면 WeightedPriority로 오래 실행된 프로세스가 CFS로
        // 전환된 첫 회계에서 부팅 이후 전체 rdtsc 경과를 통째로 vruntime에
        // 더해버리는 landmine이 생김).
        self.processes[next].run_start_ts = crate::tracer::rdtsc();
        self.current = next;
        self.processes[next].preempt_rsp
    }

    /// CFS-1(실험 33): vruntime이 가장 작은 Ready 프로세스를 선택.
    ///
    /// `boost_ticks > 0`(키보드 인터랙티브 부스트)인 프로세스가 있으면
    /// vruntime과 무관하게 그걸 즉시 우선 선택 — WeightedPriority 경로의
    /// boost 의미(README "게임 실행됨" 반응성 비전, PE-4 실험)를 그대로 유지.
    /// 동률 vruntime은 기존과 동일하게 `preferred_cpu` 힌트로, 그다음은
    /// 라운드로빈 스캔에서 먼저 발견된 것으로 타이브레이크.
    fn switch_to_next_cfs(&mut self, current_rsp: u64) -> u64 {
        let count = self.processes.len();
        let my_cpu = crate::smp::current_cpu_id();
        let start = self.current;

        let mut boost_idx: Option<usize> = None;
        let mut best_idx: Option<usize> = None;
        let mut best_vruntime = u64::MAX;
        let mut best_same_cpu = false;

        for i in 1..=count {
            let idx = (start + i) % count;
            let p = &self.processes[idx];
            if p.state != ProcessState::Ready { continue; }

            if p.boost_ticks > 0 && boost_idx.is_none() {
                boost_idx = Some(idx);
            }

            let same_cpu = p.preferred_cpu == u8::MAX || p.preferred_cpu == my_cpu;
            let better = match best_idx {
                None => true,
                Some(_) => {
                    p.vruntime < best_vruntime
                    || (p.vruntime == best_vruntime && same_cpu && !best_same_cpu)
                }
            };
            if better {
                best_vruntime = p.vruntime;
                best_idx      = Some(idx);
                best_same_cpu = same_cpu;
            }
        }

        let next = match boost_idx.or(best_idx) {
            Some(idx) => idx,
            None => {
                self.processes[self.current].state = ProcessState::Running;
                return current_rsp;
            }
        };

        self.processes[next].state = ProcessState::Running;
        self.processes[next].run_start_ts = crate::tracer::rdtsc();
        self.current = next;
        self.processes[next].preempt_rsp
    }

    pub fn recv_message(&mut self) -> Option<super::Message> {
        self.processes[self.current].message_queue.pop_front()
    }

    pub fn send_message(&mut self, to: Pid, msg: super::Message) -> bool {
        // BETA-X 1: 송신 카운터 갱신 → Policy Engine에 이벤트 알림
        let from = self.processes[self.current].pid;
        let count = self.processes[self.current].record_ipc_send(to);
        crate::policy::observe_ipc(from, to, count, msg.len as u64);

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
    // do_preempt()가 반환한 뒤(=&mut Scheduler 대여가 끝난 뒤) on_switch를
    // 호출 — on_switch가 재진입으로 scheduler::get()을 다시 부를 수 있으므로
    // 이 시점엔 살아있는 &mut Scheduler가 없어야 한다 (실험 36 aliasing 수정).
    let (new_rsp, from_pid, to_pid, tick) = unsafe { get().do_preempt(current_rsp, false) };
    crate::policy::on_switch(from_pid, to_pid, tick);
    new_rsp
}

/// 자발적 양보 (voluntary_yields++)
pub fn voluntary_preempt(current_rsp: u64) -> u64 {
    let (new_rsp, from_pid, to_pid, tick) = unsafe { get().do_preempt(current_rsp, true) };
    crate::policy::on_switch(from_pid, to_pid, tick);
    new_rsp
}

/// PE-4: 전체 컨텍스트 스위치 횟수 (A/B 벤치마크 지표).
/// on/off 무관하게 항상 집계 — 순수 관측이므로 오버헤드 없음.
pub static SWITCH_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn switch_count() -> u64 {
    SWITCH_COUNT.load(core::sync::atomic::Ordering::Relaxed)
}

/// 키보드 입력 부스트 (ALPHA 6)
/// PE-4: Policy Engine off 상태에서는 부스트도 생략 (A/B 비교 대상 신호)
pub fn keyboard_boost() {
    if !crate::policy::is_enabled() { return; }
    unsafe { get().keyboard_boost(); }
}

/// 특정 PID를 명시적으로 부스트 (실험 33에서 발견한 keyboard_boost() 자기
/// 부스트 버그의 수정 — bench_pe4처럼 "누구를 부스트할지 이미 아는" 시뮬레이션
/// 코드용). PE off 상태에서는 동일하게 생략.
pub fn boost_pid(pid: Pid, ticks: u64) {
    if !crate::policy::is_enabled() { return; }
    unsafe { get().boost_pid(pid, ticks); }
}

/// 우선순위 설정 (Policy Engine에서 호출, ALPHA 5)
pub fn set_priority(pid: Pid, pri: Priority) {
    unsafe { get().set_priority(pid, pri); }
}

/// PID의 현재 우선순위 반환 (없으면 Normal).
pub fn get_priority(pid: Pid) -> Priority {
    unsafe { get().get_priority(pid) }
}

/// 선호 CPU 설정 (BETA-X 5 core affinity)
pub fn set_preferred_cpu(pid: Pid, cpu: u8) {
    unsafe { get().set_preferred_cpu(pid, cpu); }
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

/// 진짜 블로킹 sleep (실험 30 후속).
///
/// `yield_now()`와 달리 이 프로세스를 `switch_to_next`의 후보 탐색에서
/// 완전히 제외한다(`ProcessState::Blocked`) — 최소 `ticks`틱 동안 스케줄러가
/// 이 프로세스를 실행 후보로 보지 않으므로, Policy Engine의 idle_pct 계산에
/// 실제 idle로 잡힌다. voluntary_yield 기반의 협조적 전환(yield_now)은
/// 스케줄 여부만 바꿀 뿐 "안 도는" 상태를 표현하지 못했던 것과 대비된다.
///
/// 구현은 기존 `int 0x40`(yield_now) 경로를 그대로 탄다 — 현재 프로세스에
/// sleep 요청을 먼저 심어두고("pending_sleep_ticks") voluntary yield를
/// 트리거하면, `do_preempt`가 이를 보고 Ready 대신 Blocked+wake_at_tick으로
/// 전환한다. 별도 인터럽트 벡터나 어셈블리 변경이 필요 없다.
#[inline(always)]
pub fn sleep_ticks(ticks: u64) {
    if ticks == 0 { return; }
    unsafe { get().request_sleep(ticks); }
    yield_now();
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

/// Dead 프로세스의 힙 자원을 실제로 회수 (실험 35).
///
/// **인터럽트 컨텍스트 밖(kernel_main의 일반 실행 흐름)에서만 호출할 것** —
/// `Scheduler::reap_dead()` 문서 주석 참고. 보통 `kill_pid()` 호출 직후
/// (또는 짧게 반복되는 spawn/kill 루프의 매 반복 끝)에 호출하면 된다.
pub fn reap_dead() {
    unsafe { get().reap_dead(); }
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
    (*core::ptr::addr_of_mut!(SCHEDULER)).as_mut().expect("scheduler not initialized")
}
