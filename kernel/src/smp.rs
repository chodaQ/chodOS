//! BETA 5: SMP (Symmetric Multi-Processing) 지원
//!
//! ## 설계
//!
//! Limine의 `MpRequest`가 AP(Application Processor) 시작을 처리.
//! BSP(Bootstrap Processor)가 `smp::init()`을 호출하면:
//!   1. MpRespData에서 CPU 목록 열거
//!   2. 각 AP에 정적 스택 할당 + `bootstrap()` 호출
//!   3. AP들이 `ap_entry()`에서 GDT/IDT 로드 후 HLT 루프 진입
//!
//! ## 제약
//!
//! AP들은 현재 스케줄러에 통합되지 않음 — idle 상태로 대기.
//! 향후 SMP 스케줄러와 연결 예정.

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;
use limine::mp::{MpGotoFunction, MpInfo, MpRespData};

use crate::process::{Pid, Priority};

// ── 상수 ─────────────────────────────────────────────────────────────────────

pub const MAX_CPUS: usize = 8;
const AP_STACK_SIZE: usize = 16 * 1024; // AP당 16 KB 스택

// ── 전역 상태 ─────────────────────────────────────────────────────────────────

/// 온라인 AP 수 (BSP 제외)
static AP_ONLINE: AtomicUsize = AtomicUsize::new(0);

/// 전체 CPU 수 (BSP 포함)
static CPU_TOTAL: AtomicUsize = AtomicUsize::new(1);

/// A-2: AP별 실행할 함수 포인터 (0=idle, >0=fn() -> ! 주소)
///
/// BSP가 `assign_ap_work(idx, f)`를 호출하면 AP가 다음 poll에서 실행.
/// 함수는 절대 반환하지 않아야 함 (-> !).
static AP_WORK_FN: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

// ── AP 스택 ───────────────────────────────────────────────────────────────────

/// AP별 정적 스택 (16바이트 정렬 필요 — x86_64 ABI)
#[repr(C, align(16))]
struct ApStack([u8; AP_STACK_SIZE]);

static AP_STACKS: [ApStack; MAX_CPUS] = [const { ApStack([0u8; AP_STACK_SIZE]) }; MAX_CPUS];

// ── AP 진입점 ─────────────────────────────────────────────────────────────────

/// AP 진입점 — Limine이 `MpInfo::bootstrap()` 호출 시 각 AP에서 실행됨
///
/// `extra_argument`에 AP 인덱스(0-based)를 담아 전달받음.
/// 함수 서명은 `MpGotoFunction = unsafe extern "C" fn(&MpInfo) -> !` 을 따름.
unsafe extern "C" fn ap_entry(info: &MpInfo) -> ! {
    let ap_idx = info.extra_argument() as usize;

    // ap_init(GDT/IDT/STI)을 호출하지 않는다.
    // 이유: Q35 I/O APIC가 timer IRQ를 AP에 배달 → timer handler가
    // serial_println 호출 → QEMU MTTCG BQL 경합 → THRE 고착 → hang.
    // Limine이 AP를 Long Mode + 커널 페이지 테이블로 부팅하므로
    // 인터럽트 없이도 atomic 연산과 메모리 접근은 정상 동작한다.

    AP_ONLINE.fetch_add(1, Ordering::SeqCst);

    if ap_idx == 0 {
        // AP #0: Phase A sender — A2_PONG=1 신호를 기다렸다 N번 ping 전송.
        // 스택은 Limine 임시 스택 사용 (bench_a2::ap_sender_phase_a는 재귀 없음).
        crate::bench_a2::ap_sender_phase_a()
    } else {
        // AP #1, #2: 아무 일도 안 함.
        // STI 없이 HLT = NMI/INIT을 제외한 인터럽트로 깨어나지 않음.
        // QEMU vCPU 스레드가 잠들어 BSP 성능에 영향 없음.
        loop {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

// ── 공개 API ─────────────────────────────────────────────────────────────────

/// SMP 초기화 — 모든 AP를 시작하고 온라인 대기
///
/// `interrupts::init()` 이후 + 나머지 커널 초기화 이전에 호출.
pub fn init(mp_resp: &MpRespData) {
    let cpus      = mp_resp.cpus();
    let bsp_lapic = mp_resp.bsp_lapic_id;
    let total     = cpus.len();

    CPU_TOTAL.store(total, Ordering::Relaxed);
    crate::serial_println!(
        "[smp] {} CPU(s) detected  (BSP lapic_id={})",
        total, bsp_lapic
    );

    // ── AP 시작 단계: bootstrap 호출 사이에 serial 출력 금지 ─────────────────
    // bootstrap() 이후 AP가 실행되는 동안 BSP가 serial을 쓰면
    // QEMU MTTCG BQL 경합으로 UART THRE 비트가 고착(stuck at 0)되어
    // wait_for_empty_transmit()이 무한 스핀한다.
    // → AP들이 모두 온라인이 된 후에 serial 출력을 재개한다.
    let mut ap_idx = 0usize;
    for cpu in cpus {
        if cpu.lapic_id == bsp_lapic { continue; }
        if ap_idx >= MAX_CPUS { break; }
        cpu.bootstrap(ap_entry as MpGotoFunction, ap_idx as u64);
        ap_idx += 1;
    }

    // 모든 AP가 온라인이 될 때까지 스핀 대기 (최대 ~500ms)
    let expected = ap_idx;
    let mut spins = 0u64;
    while AP_ONLINE.load(Ordering::SeqCst) < expected {
        core::hint::spin_loop();
        spins += 1;
        if spins > 100_000_000 {
            break; // timeout — 이후 serial 출력에서 실제 카운트 표시
        }
    }

    // ── AP 온라인 확인 후 serial 출력 재개 ───────────────────────────────────
    let online = AP_ONLINE.load(Ordering::SeqCst);
    if online < expected {
        crate::serial_println!(
            "[smp] timeout — {}/{} APs responded",
            online, expected
        );
    }
    crate::serial_println!(
        "[smp] init done — BSP + {} AP(s) = {} core(s) total",
        online, online + 1
    );

    // PE-3: BSP 코어 특성 감지 (CPUID leaf 0x1A)
    let hybrid = cpu_is_hybrid();
    let core_type = detect_core_type();
    crate::serial_println!(
        "[smp-PE3] BSP core type: {} (hybrid_cpu={})",
        core_type.name(), hybrid,
    );
    if !hybrid {
        crate::serial_println!(
            "[smp-PE3]   (QEMU/비-hybrid CPU — P-core/E-core 구분 미지원, 구조만 검증됨)"
        );
    }
}

/// A-2: AP에 작업 함수 할당.
///
/// `ap_idx`: 0-based AP 인덱스 (BSP 제외).
/// `f`: 실행할 함수 (절대 반환하지 않아야 함 — `-> !`).
///
/// AP가 idle poll 중일 때 다음 사이클에 즉시 실행 시작.
/// 이미 실행 중이면 완료 후 덮어쓰임 — 호출자가 완료 시점을 AtomicU8 플래그로 확인할 것.
pub fn assign_ap_work(ap_idx: usize, f: unsafe fn() -> !) {
    if ap_idx < MAX_CPUS {
        AP_WORK_FN[ap_idx].store(f as u64, Ordering::Release);
    }
}

/// 총 CPU 코어 수 (BSP + 온라인 AP)
pub fn cpu_count() -> usize {
    CPU_TOTAL.load(Ordering::Relaxed)
}

/// 온라인 AP 수
pub fn ap_count() -> usize {
    AP_ONLINE.load(Ordering::SeqCst)
}

// ── BETA-X 5: Core Affinity ───────────────────────────────────────────────────

/// PID → 선호 CPU 코어 매핑 (BETA-X 5)
static AFFINITY: Mutex<BTreeMap<Pid, u8>> = Mutex::new(BTreeMap::new());

/// 현재 실행 중인 CPU 코어 ID (CPUID leaf1 Initial APIC ID).
///
/// BSP = 보통 0, AP = LAPIC ID (1, 2, ...).
/// APs가 HLT 루프 중인 현재는 항상 BSP(0)를 반환.
///
/// rbx는 LLVM 예약 레지스터이므로 push/pop으로 보호 후 CPUID 실행.
pub fn current_cpu_id() -> u8 {
    let ebx_val: u64;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "mov {out}, rbx",
            "pop rbx",
            out = out(reg) ebx_val,
            inout("eax") 1u32 => _,
            out("ecx") _,
            out("edx") _,
        );
    }
    ((ebx_val >> 24) & 0xFF) as u8
}

// ── PE-3: 코어 특성 감지 (Intel P-core/E-core) ────────────────────────────────

/// 코어 종류 — Intel Hybrid 아키텍처(Alder Lake+) 기준.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum CoreType {
    /// Performance core (Core) — 고성능, IO바운드/지연 민감 작업에 적합
    PCore,
    /// Efficiency core (Atom) — 저전력, CPU바운드 배치 작업에 적합
    ECore,
    /// Hybrid 미지원 CPU (QEMU TCG 포함) — 모든 코어 동일 취급
    Unknown,
}

impl CoreType {
    pub fn name(self) -> &'static str {
        match self {
            CoreType::PCore   => "P-core",
            CoreType::ECore   => "E-core",
            CoreType::Unknown => "동일(비-hybrid)",
        }
    }
}

/// CPUID.07H.0:EDX 비트 15 — Hybrid 아키텍처 지원 여부.
fn cpu_is_hybrid() -> bool {
    let edx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 7u32 => _,
            inout("ecx") 0u32 => _,
            out("edx") edx,
        );
    }
    (edx >> 15) & 1 == 1
}

/// CPUID.1AH.0:EAX 비트 [31:24] — Native Model ID / Core Type.
/// 0x40 = Atom(E-core), 0x20 = Core(P-core).
///
/// 현재 실행 중인 논리 코어 기준으로 판정하므로, 호출한 코어에서
/// 즉시 읽어야 한다(마이그레이션 시 재호출 필요).
pub fn detect_core_type() -> CoreType {
    if !cpu_is_hybrid() {
        return CoreType::Unknown;
    }
    let eax: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 0x1Au32 => eax,
            out("ecx") _,
            out("edx") _,
        );
    }
    match (eax >> 24) & 0xFF {
        0x40 => CoreType::ECore,
        0x20 => CoreType::PCore,
        _    => CoreType::Unknown,
    }
}

/// 우선순위 → 권고 코어 타입.
///
/// I/O바운드(High)  → P-core (지연 민감, 빠른 응답)
/// CPU바운드(Low)   → E-core (처리량 위주, 저전력)
/// Normal          → 무관(Unknown 취급, 스케줄러 자유 배치)
pub fn recommend_core_for_priority(pri: Priority) -> CoreType {
    match pri {
        Priority::High => CoreType::PCore,
        Priority::Low  => CoreType::ECore,
        _              => CoreType::Unknown,
    }
}

/// 특정 PID를 지정 코어에 고정.
pub fn pin_to_cpu(pid: Pid, cpu: u8) {
    AFFINITY.lock().insert(pid, cpu);
}

/// PID의 선호 코어 조회. None = 미설정 (어느 코어든 실행 가능).
pub fn get_affinity(pid: Pid) -> Option<u8> {
    AFFINITY.lock().get(&pid).copied()
}

/// Hot IPC 쌍을 같은 코어에 자동 고정 (BETA-X 5 핵심 로직).
///
/// 이미 고정된 쪽이 있으면 그 코어로 합류.
/// 둘 다 미설정이면 부하 최소 코어(현재는 단순 0)에 배정.
pub fn pin_pair(from: Pid, to: Pid) {
    let from_cpu = get_affinity(from);
    let to_cpu   = get_affinity(to);

    let target = match (from_cpu, to_cpu) {
        (Some(c), _) => c,
        (_, Some(c)) => c,
        _            => least_loaded_cpu(),
    };

    // 이미 같은 코어면 중복 로그 방지
    if from_cpu == Some(target) && to_cpu == Some(target) { return; }

    pin_to_cpu(from, target);
    pin_to_cpu(to, target);
    crate::tracer::pin_pair_event(from as u32, to as u32, target);

    crate::serial_println!(
        "[affinity] hot pair pid{}↔pid{} → core{} (캐시 재사용 최적화)",
        from, to, target,
    );
}

/// 부하 최소 코어 선택 — 현재는 0(BSP) 반환.
/// SMP 스케줄러 완성 시: 각 코어의 실행 큐 길이를 비교해 최소 선택.
fn least_loaded_cpu() -> u8 {
    // TODO(BETA-X 5+): 코어별 Ready 프로세스 수 비교
    0
}

/// BETA-X 6: PID의 core affinity 핀 제거 (채널 회수 시 호출).
pub fn unpin(pid: Pid) {
    AFFINITY.lock().remove(&pid);
}

/// 현재 등록된 affinity 항목 수 (디버그용)
pub fn affinity_count() -> usize {
    AFFINITY.lock().len()
}
