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
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;
use limine::mp::{MpGotoFunction, MpInfo, MpRespData};

use crate::process::Pid;

// ── 상수 ─────────────────────────────────────────────────────────────────────

pub const MAX_CPUS: usize = 8;
const AP_STACK_SIZE: usize = 16 * 1024; // AP당 16 KB 스택

// ── 전역 상태 ─────────────────────────────────────────────────────────────────

/// 온라인 AP 수 (BSP 제외)
static AP_ONLINE: AtomicUsize = AtomicUsize::new(0);

/// 전체 CPU 수 (BSP 포함)
static CPU_TOTAL: AtomicUsize = AtomicUsize::new(1);

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

    // 정적 스택으로 RSP 전환 (Limine 제공 임시 스택 대신)
    let stack_top = AP_STACKS[ap_idx].0.as_ptr().add(AP_STACK_SIZE) as u64;
    core::arch::asm!(
        "mov rsp, {rsp}",
        "push 0",          // 16-byte alignment: ret addr placeholder
        rsp = in(reg) stack_top,
        options(nostack),
    );

    // GDT/IDT 로드 + STI (BSP가 초기화한 테이블 재사용)
    crate::interrupts::ap_init();

    // AP 온라인 등록
    AP_ONLINE.fetch_add(1, Ordering::SeqCst);

    let lapic_id  = info.lapic_id;
    let proc_id   = info.processor_id;
    crate::serial_println!(
        "[smp] AP #{} online  (lapic_id={}, processor_id={})",
        ap_idx, lapic_id, proc_id
    );

    // AP 유휴 루프 — 향후 SMP 스케줄러와 연결 예정
    loop {
        core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
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

    let mut ap_idx = 0usize;
    for cpu in cpus {
        if cpu.lapic_id == bsp_lapic {
            crate::serial_println!(
                "[smp] CPU proc_id={} lapic_id={} → BSP (skip)",
                cpu.processor_id, cpu.lapic_id
            );
            continue;
        }
        if ap_idx >= MAX_CPUS {
            crate::serial_println!("[smp] MAX_CPUS={} 초과 — 나머지 AP 건너뜀", MAX_CPUS);
            break;
        }
        crate::serial_println!(
            "[smp] starting AP #{} (lapic_id={}, proc_id={})...",
            ap_idx, cpu.lapic_id, cpu.processor_id
        );
        cpu.bootstrap(ap_entry as MpGotoFunction, ap_idx as u64);
        ap_idx += 1;
    }

    // 모든 AP가 온라인이 될 때까지 스핀 대기 (최대 ~100ms)
    let expected = ap_idx;
    let mut spins = 0u64;
    while AP_ONLINE.load(Ordering::SeqCst) < expected {
        core::hint::spin_loop();
        spins += 1;
        if spins > 50_000_000 {
            crate::serial_println!(
                "[smp] timeout — {}/{} APs responded",
                AP_ONLINE.load(Ordering::SeqCst), expected
            );
            break;
        }
    }

    let online = AP_ONLINE.load(Ordering::SeqCst);
    crate::serial_println!(
        "[smp] init done — BSP + {} AP(s) = {} core(s) total",
        online, online + 1
    );
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
