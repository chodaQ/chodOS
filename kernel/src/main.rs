//! 커널 진입점 (Kernel Entry Point)
//
//! ## 부팅 순서 (Milestone ALPHA)
//! 1. limine 부트로더 → _start() 호출
//! 2. 시리얼 포트 초기화
//! 3. 메모리 서브시스템 초기화 (프레임 할당자 + 힙)
//! 4. 인터럽트 서브시스템 초기화 (GDT + IDT + PIC + STI)
//! 5. 페이징 초기화 (커널 PML4 빌드, CR3 전환, TSS.RSP0 설정)
//! 6. Policy Engine 초기화 (ALPHA M3)
//! 7. IPC 데모 — 선점형 스케줄러 + 협력적 yield (ALPHA M1)
//! 8. Zero-copy Capability IPC 데모 (ALPHA M2)
//! 9. 선점형 스케줄러 데모 — yield 없는 태스크가 타이머에 의해 강제 전환
//! 10. VFS 데모: tmpfs 마운트 → 파일/디렉토리 생성/읽기/열거 (M3.7)
//! 11. ring3 데모: IRETQ 진입 → 유저 코드 `int 0x80` × 3 → longjmp 복귀 (M3.6)

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![allow(dead_code)]

extern crate alloc;

mod dynlink;       // BETA 19: ELF .so 파서 & 재배치 엔진 (Phase D)
mod tracer;        // BETA-X-2 1: Event Tracer — 자율 결정 블랙박스
mod bench_a1;     // BETA-X 검증 A-1: 페이로드 크기 스윕
mod bench_a2;     // BETA-X 검증 A-2: -smp 4 멀티코어 레이턴시 비교
mod bench_ipc;    // BETA-X 7: IPC 레이턴시 A/B 벤치마크
mod bench_pe4;    // PE-4: Policy Engine on/off A/B 벤치마크
mod power;        // Policy B-2: 전력 관리 보조 모듈
mod elf;
mod fb;
mod gfx_ipc;     // BETA-X 3: WM ↔ GFX 드라이버 IPC 채널 시연
mod input_direct; // BETA-X 4: 입력 경로 직통화
mod interrupts;
mod kbd;
mod mouse;
mod memory;
mod pkg;
mod net;
pub mod paging;
mod pci;
mod policy;
mod process;
mod serial;
mod signal;   // BETA 10: 시그널 서브시스템
mod smp;
mod syscall;
mod term;
mod vfs;
mod virtio;
mod wm;

use core::sync::atomic::{AtomicU64, Ordering};
use limine::request::{BootloaderInfoRequest, FramebufferRequest, HhdmRequest, MemmapRequest, MpRequest};
use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};

// ==================== Limine 요청 ====================

#[used] #[link_section = ".requests_start_marker"]
static _REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();
#[used] #[link_section = ".requests_end_marker"]
static _REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();
#[used] #[link_section = ".requests"]
static BASE_REVISION: BaseRevision = BaseRevision::with_revision(0);
#[used] #[link_section = ".requests"]
static BOOTLOADER_INFO: BootloaderInfoRequest = BootloaderInfoRequest::new();
#[used] #[link_section = ".requests"]
static HHDM: HhdmRequest = HhdmRequest::new();
#[used] #[link_section = ".requests"]
static MEMMAP: MemmapRequest = MemmapRequest::new();
#[used] #[link_section = ".requests"]
static FB_REQ: FramebufferRequest = FramebufferRequest::new();
#[used] #[link_section = ".requests"]
static MP_REQ: MpRequest = MpRequest::new(0); // flags=0: xAPIC 모드 (x2APIC 비활성)

// ==================== IPC 데모 프로세스 ====================

const PID_RECEIVER: process::Pid = 2;

fn proc_sender() -> ! {
    serial_println!("[sender pid=1] started");
    let mut counter: u64 = 0;
    loop {
        if counter < 10 {
            process::ipc::send_u64(PID_RECEIVER, counter);
            counter += 1;
        }
        // yield_now() = int 0x40 → isr64 → voluntary_yield (ALPHA M1)
        process::scheduler::yield_now();
    }
}

fn proc_receiver() -> ! {
    serial_println!("[receiver pid=2] started");
    loop {
        while let Some(msg) = process::ipc::recv() {
            let value = process::ipc::msg_as_u64(&msg);
            serial_println!("[receiver] <- pid={} | counter={}", msg.sender, value);
        }
        process::scheduler::yield_now();
    }
}

// ==================== 실험 31: sleep_ticks(n) 기반 IPC 데모 ====================
//
// proc_sender/proc_receiver와 동일한 로직이지만 yield_now()(협조적 전환,
// 여전히 Ready 상태 유지) 대신 sleep_ticks(4)(진짜 Blocked, 후보 탐색에서
// 제외)를 쓴다. 실험 30에서 "IPC-only인데 idle_pct가 0%"였던 원인이
// yield_now()의 한계였음을 검증하기 위한 대조군.
fn proc_sender_sleepy() -> ! {
    serial_println!("[sender3(sleepy) pid={}] started", process::scheduler::current_pid());
    let mut counter: u64 = 0;
    loop {
        if counter < 10 {
            process::ipc::send_u64(PID_RECEIVER, counter);
            counter += 1;
        }
        process::scheduler::sleep_ticks(4);
    }
}

fn proc_receiver_sleepy() -> ! {
    serial_println!("[receiver3(sleepy) pid={}] started", process::scheduler::current_pid());
    loop {
        while let Some(msg) = process::ipc::recv() {
            let value = process::ipc::msg_as_u64(&msg);
            serial_println!("[receiver3(sleepy)] <- pid={} | counter={}", msg.sender, value);
        }
        process::scheduler::sleep_ticks(4);
    }
}

// ==================== ALPHA M1: 선점형 스케줄러 데모 태스크 ====================
//
// 이 태스크들은 yield_now()를 호출하지 않는다.
// 타이머 IRQ(TIME_SLICE=3틱 ≈ 165ms)가 강제로 컨텍스트 스위치를 일으킨다.
// 타이머 ISR이 ALL 레지스터를 스택에 저장 → preempt_rsp 교환 → iretq로 복귀.

fn preempt_task_a() -> ! {
    serial_println!("[task_a] started — no yield, preempted by timer");
    let mut i: u64 = 0;
    loop {
        i += 1;
        // QEMU(에뮬레이션) 속도 기준 ~5백만 반복 ≈ 타임슬라이스 1-2회
        if i % 5_000_000 == 0 {
            serial_println!("[task_a] running... (loop #{})", i / 5_000_000);
        }
    }
}

fn preempt_task_b() -> ! {
    serial_println!("[task_b] started — no yield, preempted by timer");
    let mut i: u64 = 0;
    loop {
        i += 1;
        if i % 5_000_000 == 0 {
            serial_println!("[task_b] running... (loop #{})", i / 5_000_000);
        }
    }
}

// ── ST-3 검증(실험 25): CPU바운드 전용 워크로드 ────────────────────────────
// task_a/task_b와 동일 패턴(yield 없음)을 task_c/task_d로 복제해 IPC/yield
// 프로세스가 전혀 없는 순수 CPU바운드 4개 워크로드를 구성한다. 목적은
// ST-3(TIME_SLICE 범위 동적화)의 "빌드모드로 좁히기" 경로를 실제로
// 트리거시켜 검증하는 것 — 실험 24에서는 죽은 프로세스의 stale 상태가
// cnt_high를 계속 부풀려 이 경로가 한 번도 발동하지 않았다(policy::unregister
// 버그 수정으로 해결).
fn preempt_task_c() -> ! {
    serial_println!("[task_c] started — no yield, preempted by timer");
    let mut i: u64 = 0;
    loop {
        i += 1;
        if i % 5_000_000 == 0 {
            serial_println!("[task_c] running... (loop #{})", i / 5_000_000);
        }
    }
}

fn preempt_task_d() -> ! {
    serial_println!("[task_d] started — no yield, preempted by timer");
    let mut i: u64 = 0;
    loop {
        i += 1;
        if i % 5_000_000 == 0 {
            serial_println!("[task_d] running... (loop #{})", i / 5_000_000);
        }
    }
}

// ==================== BETA-X 6: 채널 회수(Decay) 데모 ====================

static DECAY_RECV_PID: AtomicU64 = AtomicU64::new(0);

fn decay_recv_task() -> ! {
    DECAY_RECV_PID.store(process::scheduler::current_pid(), Ordering::Relaxed);
    loop {
        while let Some(_) = process::ipc::recv() {}
        process::scheduler::yield_now();
    }
}

fn decay_send_task() -> ! {
    // 수신자 PID 대기
    let recv = loop {
        let p = DECAY_RECV_PID.load(Ordering::Relaxed);
        if p != 0 { break p; }
        process::scheduler::yield_now();
    };
    // 130회 버스트 → IPC_HOT_THRESHOLD(100) 초과 → fast channel 생성 트리거
    for i in 0u8..130 {
        process::ipc::send(recv, &[i]);
    }
    serial_println!("[decay-send] 130회 송신 완료 → 이후 침묵 (cold 카운트 시작)");
    // 이후 메시지 없음 → Policy Engine이 cold 창 감지 → 채널 회수
    loop { process::scheduler::yield_now(); }
}

// ==================== Policy B: 메모리 압력 + 전력 신호 ====================

fn mem_stress_task() -> ! {
    let pid = process::scheduler::current_pid();
    serial_println!("[mem-stress] 시작 (pid={})", pid);
    let mut n: u64 = 0;
    loop {
        // mmap 압력 시뮬레이션 — Policy Engine에 직접 알림
        // (실제 시스템에서는 sys_mmap 훅이 자동으로 이 역할을 함)
        policy::observe_mmap(pid);
        n += 1;
        if n % 20 == 0 {
            process::scheduler::yield_now();
        }
    }
}

fn cpu_stress_task() -> ! {
    serial_println!("[cpu-stress] 시작 — busy loop (타이머 선점 전용)");
    // 선점형 스케줄러에 의해 강제 전환됨 → forced_preempts 증가 → idle_pct 감소
    let mut x: u64 = 0xDEAD_BEEF;
    loop {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        // yield 없음 — 타이머에 의해만 전환
        if x == 0 { break; } // 절대 실행 안 됨 (컴파일러 최적화 방지)
    }
    unreachable!()
}

// ==================== ALPHA 14: 내장 테스트 ELF 바이너리 ====================
//
// 손으로 조립한 최소 ELF64 실행 파일 (181 = 0xB5 bytes)
//
// ## 구조
//
// ```
// Offset  Size  Content
// ──────────────────────────────
// 0x00    64    ELF64 헤더
//               e_entry = 0x400078
//               e_phoff = 0x40  (프로그램 헤더 시작)
//               e_phnum = 1
// 0x40    56    PT_LOAD 프로그램 헤더
//               p_offset = 0     (파일 처음부터 매핑)
//               p_vaddr  = 0x400000
//               p_filesz = 0xB5  (181 bytes)
//               p_memsz  = 0x1000 (1 페이지)
//               p_align  = 0x1000
// 0x78    61    코드 + 데이터 (vaddr 0x400078)
// ──────────────────────────────
// ```
//
// ## 코드 (vaddr 0x400078):
//
// ```asm
// 0x400078:  EB 10          jmp +16   ; 메시지 건너뛰기
// 0x40007A:  "Hello from ELF!\n"  ; 16 bytes (0x40007A..0x400089)
// 0x40008A:  B8 01 00 00 00  mov eax, 1      ; SYS_write
//            BF 01 00 00 00  mov edi, 1      ; fd = stdout
//            48 8D 35 DF FF FF FF  lea rsi, [rip-0x21]   ; → 0x40007A (메시지)
//              ; after lea: RIP = 0x40009B, disp = 0x40007A - 0x40009B = -0x21 ✓
//            BA 10 00 00 00  mov edx, 16     ; length
//            CD 80           int 0x80        ; → sys_write
//            B8 27 00 00 00  mov eax, 39     ; SYS_getpid
//            CD 80           int 0x80        ; → RAX = pid
//            B8 3C 00 00 00  mov eax, 60     ; SYS_exit
//            BF 00 00 00 00  mov edi, 0      ; exit code
//            CD 80           int 0x80        ; → longjmp
// ```
// BETA 2-1: musl-static hello 바이너리로 교체 — Linux ABI 호환성 검증
pub static ELF_TEST: &[u8] = include_bytes!(
    concat!(env!("CARGO_MANIFEST_DIR"), "/../build/musl_hello.elf")
);

// ==================== ALPHA 15: 내장 MuShell ELF ====================
//
// user/mushell/ 크레이트가 build/mushell.elf를 생성.
// (Makefile의 mushell 타겟이 먼저 빌드)
// handlers.rs의 after_user_demo Phase 1이 이 바이너리를 ring3에서 실행.
pub static MUSHELL_ELF: &[u8] = include_bytes!(
    concat!(env!("CARGO_MANIFEST_DIR"), "/../build/mushell.elf")
);

// ==================== 데모 워크로드 크기 ====================
//
// quick-demo 기능이 켜지면 데모용 대기 루프를 짧게 잡는다. 이 루프들은
// Policy Engine이 리포트를 몇 번 돌릴 시간을 벌기 위한 것이라, 리포트
// 주기(8~36틱)보다 충분히 길기만 하면 관측 목적은 유지된다.
// (자세한 배경은 kernel/Cargo.toml의 [features] 주석 참고)
#[cfg(feature = "quick-demo")]
const DEMO_WAIT_TICKS: u64 = 120;
#[cfg(not(feature = "quick-demo"))]
const DEMO_WAIT_TICKS: u64 = 300;

/// BETA-X 3/4 데모 태스크의 진행 로그 출력 간격에 곱하는 배수.
///
/// 이 두 데모는 yield 루프를 매우 빠르게 돌아 프레임/이벤트 수가 수십만까지
/// 올라간다. 촘촘히 로그를 찍으면 시리얼 출력 자체가 병목이 된다 — 실측상
/// 전체 로그 106,404줄 중 104,545줄(4.7MB)이 이 두 데모에서 나왔고, UART가
/// 바이트당 수십 µs를 바쁜 대기로 소모하므로 출력에만 수 분이 걸렸다.
/// (guest time만 보면 이 구간은 짧아서, 처음 부팅 시간을 분석할 때
///  벤치마크만 범인으로 지목했다가 놓쳤던 두 번째 병목이다.)
///
/// 배수로 둔 이유: full 모드에서는 1이 되어 기존 로그 간격(15/30/20건)이
/// 그대로 유지된다. 로그량이 타이밍에 영향을 줄 수 있어, 기존 실험을 돌렸던
/// 조건을 바꾸지 않기 위함이다.
#[cfg(feature = "quick-demo")]
pub const DEMO_LOG_SCALE: u64 = 100;
#[cfg(not(feature = "quick-demo"))]
pub const DEMO_LOG_SCALE: u64 = 1;

// ==================== 커널 진입점 ====================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    assert!(BASE_REVISION.is_supported(), "limine revision not supported");

    // ── 1. 시리얼 ────────────────────────────────────────────────────────
    serial::init();
    serial_println!("===========================================");
    serial_println!("  MuKernel v0.1.0 - Milestone ALPHA");
    serial_println!("  Preemptive Sched + ZeroCopy IPC + Policy");
    serial_println!("===========================================");
    // 벤치마크 수치를 EXPERIMENTS.md의 기록과 혼동하지 않도록 모드를 명시한다.
    #[cfg(feature = "quick-demo")]
    serial_println!(
        "[demo] QUICK 모드 — 벤치마크 워크로드 축소됨. \
         측정 수치는 EXPERIMENTS.md 기록과 비교 불가 (전체: make run-full)"
    );
    #[cfg(not(feature = "quick-demo"))]
    serial_println!("[demo] FULL 모드 — 전체 벤치마크 워크로드 (약 41분 소요)");
    if let Some(info) = BOOTLOADER_INFO.response() {
        serial_println!("[boot] {} v{}", info.name(), info.version());
    }

    // ── 2. 메모리 ────────────────────────────────────────────────────────
    let hhdm_offset = HHDM.response().expect("HHDM response missing").offset;
    let memmap = MEMMAP.response().expect("memmap response missing").entries();
    memory::init(hhdm_offset, memmap);

    // ── 3. 인터럽트 (GDT→IDT→PIC→STI) ─────────────────────────────────
    interrupts::init();

    // ── 3.5. SMP: AP 시작 (BETA 5) ──────────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  BETA 5: SMP (Symmetric Multi-Processing)");
    serial_println!("===========================================");
    if let Some(mp_resp) = MP_REQ.response() {
        smp::init(mp_resp);
    } else {
        serial_println!("[smp] MpRequest not answered — single-core mode");
    }
    serial_println!("--- SMP init done ({} core(s)) ---\n", smp::cpu_count());

    // ── 4. 페이징 ─────────────────────────────────────────────────────────
    paging::init();

    // ── 5. Policy Engine 초기화 (ALPHA M3) ───────────────────────────────
    policy::init();

    // ── 6. 선점형 스케줄러 + IPC 데모 ────────────────────────────────────
    serial_println!("\n--- IPC Demo (선점형 스케줄러 적용) ---");
    process::scheduler::init();

    let sid = process::scheduler::alloc_pid(); // 1
    process::scheduler::spawn(process::Process::new(sid, "sender", proc_sender));
    let rid = process::scheduler::alloc_pid(); // 2
    process::scheduler::spawn(process::Process::new(rid, "receiver", proc_receiver));

    // yield_now()이 이제 int 0x40 → isr64 → voluntary_yield 경로를 탐
    for _ in 0..30 {
        process::scheduler::yield_now();
    }

    // IPC 데모 프로세스 종료 (Dead 표시 → 스케줄러가 건너뜀)
    process::scheduler::kill_pid(sid);
    process::scheduler::kill_pid(rid);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("--- IPC Demo complete ---\n");

    // ── 7. Zero-copy Capability IPC 데모 (ALPHA M2) ──────────────────────
    serial_println!("===========================================");
    serial_println!("  ALPHA M2: Zero-copy Capability IPC");
    serial_println!("===========================================");

    // 송신자: 공유 버퍼 할당 → 데이터 기록 → CapId 전송
    {
        use alloc::vec::Vec;
        let sender_pid: process::Pid = 0; // kernel_main이 sender 역할
        let _receiver_pid: process::Pid = 99; // 가상 수신자 (데모용)

        // 4KB 데이터를 공유 버퍼에 등록 (복사 없이 참조 전달)
        let mut payload: Vec<u8> = Vec::with_capacity(4096);
        for i in 0u8..=255 {
            payload.extend_from_slice(&[i; 16]); // 256 × 16 = 4096 bytes
        }
        let data_size = payload.len();
        let cap = process::ipc_cap::alloc_shared(sender_pid, payload);
        serial_println!("[cap-ipc] alloc_shared: cap_id={}, size={} bytes", cap, data_size);

        // append: 추가 데이터도 복사 없이 버퍼에 직접 추가
        process::ipc_cap::append_shared(cap, b"[appended metadata]");
        serial_println!("[cap-ipc] append_shared: +19 bytes");

        // read: 수신자 입장 — cap_id로 데이터에 직접 접근 (복사 없음)
        process::ipc_cap::read_shared(cap, |data| {
            serial_println!("[cap-ipc] read_shared: {} bytes total", data.len());
            serial_println!("[cap-ipc]   data[0]   = {:#04x}", data[0]);
            serial_println!("[cap-ipc]   data[4095] = {:#04x}", data[4095]);
            serial_println!("[cap-ipc]   tail = {:?}",
                core::str::from_utf8(&data[data.len()-19..]).unwrap_or("?"));
        });

        // capability 소멸: 버퍼 해제 + 이후 접근 불가
        let freed = process::ipc_cap::drop_shared(cap);
        serial_println!("[cap-ipc] drop_shared: cap_id={} freed={}", cap, freed);
        serial_println!("[cap-ipc] is_valid after drop: {}", process::ipc_cap::is_valid(cap));
    }
    serial_println!("--- Zero-copy IPC demo complete ---\n");

    // ── 8. 선점형 스케줄러 데모 (ALPHA M1 핵심) ──────────────────────────
    //
    // task_a / task_b: yield_now() 호출 없이 무한 루프.
    // 타이머 IRQ0이 ~165ms마다 강제로 컨텍스트 스위치를 일으킴.
    // Policy Engine이 CPU 점유율을 실시간 추적 후 주기 리포트.
    serial_println!("===========================================");
    serial_println!("  ALPHA M1: 선점형 스케줄러 데모");
    serial_println!("  (yield 없는 태스크가 타이머에 의해 강제 전환)");
    serial_println!("===========================================");

    let pa = process::scheduler::alloc_pid(); // 3
    process::scheduler::spawn(process::Process::new(pa, "task_a", preempt_task_a));
    let pb = process::scheduler::alloc_pid(); // 4
    process::scheduler::spawn(process::Process::new(pb, "task_b", preempt_task_b));

    // kernel_main은 HLT 대기 루프로 ~3초 대기.
    // 이 동안 타이머 IRQ가 task_a / task_b / kernel_main을 라운드로빈으로 전환.
    // HLT: 다음 인터럽트까지 CPU 슬립 → 타이머가 깨우면 틱 확인 후 재슬립.
    let start_tick = interrupts::handlers::TICK.load(Ordering::Relaxed);
    while interrupts::handlers::TICK.load(Ordering::Relaxed) - start_tick < 54 {
        // ~3초 (54틱 × ~55ms/틱) 대기
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }

    // 태스크 종료 (Dead 표시)
    process::scheduler::kill_pid(pa);
    process::scheduler::kill_pid(pb);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("[sched] task_a / task_b killed. Continuing...\n");

    // ── 8-1. ST-3 검증(실험 25): CPU바운드 전용 워크로드 ──────────────────
    // sender/receiver(High 우선순위, IPC/yield) 없이 순수 CPU바운드 프로세스
    // 4개만 오래 돌려서, TIME_SLICE 범위가 실제로 빌드모드(2~3틱)까지
    // 좁혀지는지 관찰한다. task_a/task_b는 위에서 이미 kill_pid()로 죽었고
    // (policy::unregister_pid 수정 덕분에 이제 stats 슬롯도 비활성화됨),
    // 여기서는 완전히 새로운 4개 프로세스로 다시 시작한다.
    serial_println!("===========================================");
    serial_println!("  ST-3 검증: CPU바운드 전용 워크로드 (실험 25)");
    serial_println!("  (IPC/yield 프로세스 없음 — 빌드모드 트리거 관찰)");
    serial_println!("===========================================");

    let pc = process::scheduler::alloc_pid();
    process::scheduler::spawn(process::Process::new(pc, "task_c", preempt_task_c));
    let pd = process::scheduler::alloc_pid();
    process::scheduler::spawn(process::Process::new(pd, "task_d", preempt_task_d));
    let pe = process::scheduler::alloc_pid();
    process::scheduler::spawn(process::Process::new(pe, "task_e", preempt_task_a));
    let pf = process::scheduler::alloc_pid();
    process::scheduler::spawn(process::Process::new(pf, "task_f", preempt_task_b));

    // 300틱(≈16.5초) 대기 — report_interval이 8틱까지 줄어들 여유를 주고
    // ST-3의 Hysteresis(창당 ±1단계)가 baseline(게임모드)에서 빌드모드까지
    // 2단계 이동할 시간을 확보한다.
    let st3_start_tick = interrupts::handlers::TICK.load(Ordering::Relaxed);
    while interrupts::handlers::TICK.load(Ordering::Relaxed) - st3_start_tick < DEMO_WAIT_TICKS {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }

    process::scheduler::kill_pid(pc);
    process::scheduler::kill_pid(pd);
    process::scheduler::kill_pid(pe);
    process::scheduler::kill_pid(pf);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("[sched] ST-3 검증 워크로드 종료. Continuing...\n");

    // ── 8-2. PE-2 게임 프로파일 경계값 검증(실험 29 후속) ──────────────────
    // 실험 29는 빌드 프로파일 경계(유휴 64%→bias+15→79%→MWAIT)만 실측했고,
    // 게임 프로파일 쪽 경계(유휴 70%+ & 게임)는 이번까지 두 벤치마크
    // (task_a/b 동시구간, GUI 렌더링 구간) 모두 CPU를 계속 점유하고 있어서
    // 관측되지 않았다. sender/receiver(IPC + voluntary yield, CPU 홀드
    // 시간이 짧음)만 단독으로 오래 돌려 "게임 실행 중이지만 대부분 대기
    // 상태"인 상황을 재현한다 — CPU바운드 프로세스가 전혀 없으므로
    // High/Low 분류표에는 IPC 프로세스만 잡혀 워크로드 프로파일이 게임
    // 쪽(baseline)에 머무는 동안 실제 유휴율이 얼마나 올라가는지 관찰한다.
    serial_println!("===========================================");
    serial_println!("  PE-2 게임 프로파일 경계값 검증 (실험 29 후속)");
    serial_println!("  (CPU바운드 프로세스 없음 — IPC만 단독 실행)");
    serial_println!("===========================================");

    let sid2 = process::scheduler::alloc_pid();
    process::scheduler::spawn(process::Process::new(sid2, "sender2", proc_sender));
    let rid2 = process::scheduler::alloc_pid();
    process::scheduler::spawn(process::Process::new(rid2, "receiver2", proc_receiver));

    // 실험 31에서 이 구간(순수 yield_now() busy-loop인 sender2/receiver2)이
    // kernel_main을 무기한 굶기는 starvation 버그를 발견했었다(aging이 한
    // 단계만 올라 Low/Normal이 영원히 Ready&High를 못 넘던 문제). 실험 32에서
    // scheduler.rs의 FAIRNESS_FLOOR_TICKS(200틱 이상 대기 시 무조건 High로
    // 강제 승격)로 수정 완료 — 이 구간도 이제 정상 종료된다.
    let pe2_start_tick = interrupts::handlers::TICK.load(Ordering::Relaxed);
    while interrupts::handlers::TICK.load(Ordering::Relaxed) - pe2_start_tick < DEMO_WAIT_TICKS {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }

    process::scheduler::kill_pid(sid2);
    process::scheduler::kill_pid(rid2);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("[sched] PE-2 게임 프로파일 경계값 검증 종료. Continuing...\n");

    // ── 8-3. sleep_ticks(n) 검증 + PE-2 게임 경계값 재도전(실험 31) ─────────
    // 실험 30 결론: yield_now()만으로는 idle_pct를 낮출 수 없다(협조적 전환일
    // 뿐 Ready 상태 유지). 여기서는 동일한 IPC 워크로드를 sleep_ticks(4)로
    // 바꿔서 진짜 Blocked 상태를 만든 뒤, 유휴율이 이번에는 올라가는지 —
    // 그리고 유휴율 70%대에서 게임 프로파일이 MWAIT를 억제하는지 관찰한다.
    serial_println!("===========================================");
    serial_println!("  실험 31: sleep_ticks(n) 기반 게임 경계값 재도전");
    serial_println!("===========================================");

    let sid3 = process::scheduler::alloc_pid();
    process::scheduler::spawn(process::Process::new(sid3, "sender3", proc_sender_sleepy));
    let rid3 = process::scheduler::alloc_pid();
    process::scheduler::spawn(process::Process::new(rid3, "receiver3", proc_receiver_sleepy));

    let e31_start_tick = interrupts::handlers::TICK.load(Ordering::Relaxed);
    while interrupts::handlers::TICK.load(Ordering::Relaxed) - e31_start_tick < DEMO_WAIT_TICKS {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }

    process::scheduler::kill_pid(sid3);
    process::scheduler::kill_pid(rid3);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("[sched] 실험 31 워크로드 종료. Continuing...\n");

    // ── 9. ext4 데모 (ALPHA 8) ────────────────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  ALPHA 8: ext4 Disk Image");
    serial_println!("===========================================");

    // 커널 바이너리에 embed된 ext4 이미지 (빌드 시 Makefile이 생성)
    static EXT4_IMAGE: &[u8] = include_bytes!(
        concat!(env!("CARGO_MANIFEST_DIR"), "/../build/rootfs.ext4")
    );

    if vfs::mount_ext4(EXT4_IMAGE) {
        // /etc/os-release 읽기
        if let Some(data) = vfs::ext4_read_file("/etc/os-release") {
            serial_println!("[ext4] /etc/os-release ({} bytes):", data.len());
            for line in core::str::from_utf8(&data).unwrap_or("").lines() {
                serial_println!("       {}", line);
            }
        }

        // /etc/motd 읽기
        if let Some(data) = vfs::ext4_read_file("/etc/motd") {
            serial_println!("[ext4] /etc/motd: {:?}",
                core::str::from_utf8(&data).unwrap_or("").trim_end());
        }

        // /var/log/boot.log 읽기
        if let Some(data) = vfs::ext4_read_file("/var/log/boot.log") {
            serial_println!("[ext4] /var/log/boot.log ({} bytes):", data.len());
            for line in core::str::from_utf8(&data).unwrap_or("").lines() {
                serial_println!("       {}", line);
            }
        }

        // / 디렉토리 열거
        serial_println!("[ext4] ls /:");
        for entry in vfs::ext4_list_dir("/") {
            serial_println!("       {}{}", entry.name,
                if entry.is_dir { "/" } else { "" });
        }

        // /etc 디렉토리 열거
        serial_println!("[ext4] ls /etc:");
        for entry in vfs::ext4_list_dir("/etc") {
            serial_println!("       {}{}", entry.name,
                if entry.is_dir { "/" } else { "" });
        }
    }
    serial_println!("--- ext4 demo complete ---\n");

    // ── ALPHA 9: Capability Handle Table ─────────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  ALPHA 9: Capability Handle Table");
    serial_println!("  (Linux fd <-> Handle <-> Capability<T>)");
    serial_println!("===========================================");
    {
        use process::handle::{Capability, Rights};

        // 1. 읽기 전용 버퍼 Capability 등록
        let buf_ro: alloc::vec::Vec<u8> = b"hello from capability".to_vec();
        let h_ro = process::scheduler::insert_capability(
            Capability::new(buf_ro, Rights::READ),
        );
        serial_println!("[cap9] insert fd={} rights={:?}", h_ro.id, h_ro.rights);

        // 2. 읽기+쓰기 Capability 등록
        let buf_rw: alloc::vec::Vec<u8> = b"writable buffer".to_vec();
        let h_rw = process::scheduler::insert_capability(
            Capability::new(buf_rw, Rights::READ | Rights::WRITE),
        );
        serial_println!("[cap9] insert fd={} rights={:?}", h_rw.id, h_rw.rights);

        // 3. 읽기 조회 (성공)
        if let Some(data) = process::scheduler::get_capability::<alloc::vec::Vec<u8>>(
            h_ro.id, Rights::READ,
        ) {
            serial_println!("[cap9] read fd={}: {:?}",
                h_ro.id, core::str::from_utf8(&data).unwrap_or("?"));
        }

        // 4. 읽기 전용 핸들에 쓰기 시도 (권한 거부)
        let denied = process::scheduler::get_capability::<alloc::vec::Vec<u8>>(
            h_ro.id, Rights::WRITE,
        ).is_none();
        serial_println!("[cap9] write on read-only fd={}: {}",
            h_ro.id, if denied { "denied ✓" } else { "BUG: should be denied!" });

        // 5. 타입 불일치 조회 (거부)
        let type_mismatch = process::scheduler::get_capability::<u64>(
            h_ro.id, Rights::READ,
        ).is_none();
        serial_println!("[cap9] wrong-type get fd={}: {}",
            h_ro.id, if type_mismatch { "denied ✓" } else { "BUG!" });

        // 6. 핸들 목록 출력
        for (id, rights) in process::scheduler::list_handles() {
            serial_println!("[cap9]   open fd={} rights={:?}", id, rights);
        }
        serial_println!("[cap9] open handles: {}", process::scheduler::handle_count());

        // 7. 핸들 닫기
        process::scheduler::close_handle(h_ro.id);
        process::scheduler::close_handle(h_rw.id);
        serial_println!("[cap9] after close: {} handles remain",
            process::scheduler::handle_count());
    }
    serial_println!("--- ALPHA 9 demo complete ---\n");

    // ── ALPHA 10: VirtIO Block 드라이버 ──────────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  ALPHA 10: VirtIO Block Driver");
    serial_println!("===========================================");
    if let Some(mut blk) = virtio::VirtioBlk::init() {
        // 섹터 0 읽기 (MBR / 부트 섹터)
        let mut sector0 = [0u8; 512];
        if blk.read_sector(0, &mut sector0) {
            serial_println!("[virtio-blk] sector 0 read OK ({} bytes)", sector0.len());
            // 처음 64바이트를 16진수로 출력
            for row in 0..4 {
                let off = row * 16;
                serial_println!(
                    "[virtio-blk]   {:04x}: {:02x} {:02x} {:02x} {:02x}  {:02x} {:02x} {:02x} {:02x}  {:02x} {:02x} {:02x} {:02x}  {:02x} {:02x} {:02x} {:02x}",
                    off,
                    sector0[off+0],  sector0[off+1],  sector0[off+2],  sector0[off+3],
                    sector0[off+4],  sector0[off+5],  sector0[off+6],  sector0[off+7],
                    sector0[off+8],  sector0[off+9],  sector0[off+10], sector0[off+11],
                    sector0[off+12], sector0[off+13], sector0[off+14], sector0[off+15],
                );
            }
            // ASCII 헤더 출력 (텍스트 포함 시)
            let text = core::str::from_utf8(&sector0[..64]).unwrap_or("");
            let printable: alloc::string::String = text.chars()
                .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '.' })
                .collect();
            serial_println!("[virtio-blk]   ascii: {}", &printable[..printable.len().min(48)]);
        } else {
            serial_println!("[virtio-blk] sector 0 read FAILED");
        }

        // 섹터 1 쓰기 후 다시 읽기 (왕복 검증)
        let mut write_buf = [0u8; 512];
        let magic = b"MuKernel VirtIO Block OK";
        write_buf[..magic.len()].copy_from_slice(magic);
        write_buf[510] = 0xAA;
        write_buf[511] = 0x55;
        if blk.write_sector(1, &write_buf) {
            let mut read_buf = [0u8; 512];
            if blk.read_sector(1, &mut read_buf) && read_buf[..magic.len()] == *magic {
                serial_println!("[virtio-blk] write→read roundtrip OK: {:?}",
                    core::str::from_utf8(&read_buf[..magic.len()]).unwrap_or("?"));
            } else {
                serial_println!("[virtio-blk] roundtrip MISMATCH");
            }
        }
    } else {
        serial_println!("[virtio-blk] device not found — QEMU disk 없음");
        serial_println!("  (Makefile의 DISK_IMG 타겟과 run 타겟에 -device virtio-blk-pci 필요)");
    }
    serial_println!("--- ALPHA 10 demo complete ---\n");

    // ── ALPHA 11: VirtIO Net 드라이버 ────────────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  ALPHA 11: VirtIO Net Driver");
    serial_println!("  (ARP request to QEMU SLIRP gateway)");
    serial_println!("===========================================");

    if let Some(mut nic) = virtio::VirtioNet::init() {
        let mac = nic.mac;
        serial_println!(
            "[virtio-net] MAC = {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );

        // ARP request: who has 10.0.2.2? tell 10.0.2.15
        let src_ip: [u8; 4] = [10, 0, 2, 15];
        let dst_ip: [u8; 4] = [10, 0, 2,  2];
        let arp_frame = virtio::net::build_arp_request(mac, src_ip, dst_ip);

        serial_println!(
            "[virtio-net] Sending ARP request: who has {}.{}.{}.{}? tell {}.{}.{}.{}",
            dst_ip[0], dst_ip[1], dst_ip[2], dst_ip[3],
            src_ip[0], src_ip[1], src_ip[2], src_ip[3],
        );

        if nic.send(&arp_frame) {
            serial_println!("[virtio-net] ARP frame sent OK");
        } else {
            serial_println!("[virtio-net] ARP send timeout");
        }

        // RX 폴링: ~1초 안에 ARP reply 기다림
        let poll_start = interrupts::handlers::TICK.load(Ordering::Relaxed);
        let mut got_reply = false;
        while interrupts::handlers::TICK.load(Ordering::Relaxed) - poll_start < 18 {
            if let Some(frame) = nic.try_recv() {
                let etype = virtio::net::ethertype(&frame);
                serial_println!("[virtio-net] RX {} bytes, ethertype=0x{:04x}", frame.len(), etype);

                if let Some((reply_mac, reply_ip)) = virtio::net::parse_arp_reply(&frame) {
                    serial_println!(
                        "[virtio-net] ARP reply: {}.{}.{}.{} is at {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                        reply_ip[0], reply_ip[1], reply_ip[2], reply_ip[3],
                        reply_mac[0], reply_mac[1], reply_mac[2], reply_mac[3], reply_mac[4], reply_mac[5],
                    );
                    got_reply = true;
                    break;
                }
            }
            unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
        }

        if !got_reply {
            serial_println!("[virtio-net] no ARP reply received (timeout ~1s)");
        }
    } else {
        serial_println!("[virtio-net] device not found — QEMU NIC 없음");
        serial_println!("  (Makefile run 타겟에 -netdev user + -device virtio-net-pci 필요)");
    }
    serial_println!("--- ALPHA 11 demo complete ---\n");

    // ── ALPHA 12: IPv4 네트워크 스택 (ARP/ICMP/UDP) ──────────────────────────
    serial_println!("===========================================");
    serial_println!("  ALPHA 12: IPv4 Network Stack (ARP/ICMP/UDP)");
    serial_println!("  (ARP table + IPv4 + ICMP ping + UDP)");
    serial_println!("===========================================");

    if let Some(nic) = virtio::VirtioNet::init() {
        let guest_ip = [10u8, 0, 2, 15];
        let gateway  = [10u8, 0, 2,  2];
        let dns      = [10u8, 0, 2,  3];

        let mut stack = net::NetworkStack::new(nic, guest_ip);
        serial_println!(
            "[net] stack up: IP={}.{}.{}.{}  MAC={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            guest_ip[0], guest_ip[1], guest_ip[2], guest_ip[3],
            stack.mac[0], stack.mac[1], stack.mac[2],
            stack.mac[3], stack.mac[4], stack.mac[5],
        );

        // 1. ARP resolve gateway
        serial_println!("[net] ARP resolve {}.{}.{}.{}...", gateway[0], gateway[1], gateway[2], gateway[3]);
        if let Some(gw_mac) = stack.arp_resolve(gateway) {
            serial_println!(
                "[net] gateway MAC = {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                gw_mac[0], gw_mac[1], gw_mac[2], gw_mac[3], gw_mac[4], gw_mac[5],
            );
        } else {
            serial_println!("[net] ARP resolve timeout");
        }

        // 2. ICMP ping gateway (3회)
        for i in 0..3u16 {
            let t0 = interrupts::handlers::TICK.load(Ordering::Relaxed);
            match stack.ping(gateway) {
                Some(seq) => {
                    let rtt = interrupts::handlers::TICK.load(Ordering::Relaxed) - t0;
                    serial_println!(
                        "[net] ping {}.{}.{}.{}: reply seq={} rtt=~{}ms",
                        gateway[0], gateway[1], gateway[2], gateway[3],
                        seq, rtt * 55
                    );
                }
                None => serial_println!("[net] ping seq={} timeout", i),
            }
        }

        // 3. UDP 송신 (DNS query to 10.0.2.3:53 — "mukernel\x00" A record lookup)
        //
        // 최소 DNS query 패킷 (www.example.com A record 요청 형식)
        // SLIRP가 응답하지 않아도 TX 성공이 목표.
        let dns_query: &[u8] = &[
            0x00, 0x01,  // Transaction ID
            0x01, 0x00,  // Flags: standard query
            0x00, 0x01,  // QDCOUNT = 1
            0x00, 0x00,  // ANCOUNT = 0
            0x00, 0x00,  // NSCOUNT = 0
            0x00, 0x00,  // ARCOUNT = 0
            // QNAME: "mukernel\x00"
            0x08, b'm', b'u', b'k', b'e', b'r', b'n', b'e', b'l',
            0x00,        // root label
            0x00, 0x01,  // QTYPE = A
            0x00, 0x01,  // QCLASS = IN
        ];
        let sent = stack.udp_send(dns, 53, 1024, dns_query);
        serial_println!("[net] UDP DNS query to {}.{}.{}.{}:53 — tx={}",
            dns[0], dns[1], dns[2], dns[3], if sent { "OK" } else { "FAIL" });

        // 4. RX poll ~500ms: SLIRP DNS 응답 기다리기
        let poll_end = interrupts::handlers::TICK.load(Ordering::Relaxed) + 9;
        while interrupts::handlers::TICK.load(Ordering::Relaxed) < poll_end {
            if let Some(pkt) = stack.poll() {
                match pkt {
                    net::Packet::Udp { src, src_port, dst_port, data } => {
                        serial_println!(
                            "[net] UDP RX from {}.{}.{}.{}:{} → port {} ({} bytes)",
                            src[0], src[1], src[2], src[3], src_port, dst_port, data.len()
                        );
                    }
                    net::Packet::IcmpEchoReply { src, id, seq } => {
                        serial_println!(
                            "[net] ICMP reply from {}.{}.{}.{} id={:#x} seq={}",
                            src[0], src[1], src[2], src[3], id, seq
                        );
                    }
                }
            }
        }
    } else {
        serial_println!("[net] VirtioNet not available");
    }
    serial_println!("--- ALPHA 12 demo complete ---\n");

    // ── ALPHA 17: GUI / Window System ─────────────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  ALPHA 17: GUI / Window System");
    serial_println!("  (Limine Framebuffer + 8x8 Font + MuWM)");
    serial_println!("===========================================");

    if let Some(fb_resp) = FB_REQ.response() {
        let fbs = fb_resp.framebuffers();
        if let Some(fb) = fbs.first() {
            let addr   = fb.address() as *mut u8 as usize;
            let width  = fb.width  as u32;
            let height = fb.height as u32;
            let pitch  = fb.pitch  as u32;
            let r_sh   = fb.red_mask_shift;
            let g_sh   = fb.green_mask_shift;
            let b_sh   = fb.blue_mask_shift;
            serial_println!("[fb] {}x{} pitch={} bpp={} r_sh={} g_sh={} b_sh={}",
                width, height, pitch, fb.bpp, r_sh, g_sh, b_sh);
            fb::init(addr, width, height, pitch, r_sh, g_sh, b_sh);
            wm::render_desktop();
            serial_println!("[fb] desktop rendered — {} windows + taskbar", 3);

            // PS/2 마우스 초기화 + 초기 커서 렌더링
            mouse::init();
            let (mx, my) = (
                mouse::MOUSE_X.load(core::sync::atomic::Ordering::Relaxed),
                mouse::MOUSE_Y.load(core::sync::atomic::Ordering::Relaxed),
            );
            fb::draw_cursor(mx, my);
        } else {
            serial_println!("[fb] no framebuffer in response");
        }
    } else {
        serial_println!("[fb] FramebufferRequest not responded");
        serial_println!("     GUI requires display: use 'make run-gui'");
    }
    serial_println!("--- ALPHA 17 demo complete ---\n");

    // ── 10. VFS 데모 (Milestone 3.7) ─────────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  Milestone 3.7: VFS + tmpfs");
    serial_println!("===========================================");

    vfs::init();
    vfs::mkdir("/etc").expect("mkdir /etc");
    vfs::mkdir("/var").expect("mkdir /var");
    vfs::mkdir("/var/log").expect("mkdir /var/log");
    vfs::create_file("/etc/hostname").expect("create /etc/hostname");
    vfs::write_file("/etc/hostname", b"mukernel\n");
    vfs::create_file("/etc/version").expect("create /etc/version");
    vfs::write_file("/etc/version", b"MuKernel v0.1.0-alpha\n");
    vfs::create_file("/var/log/kernel.log").expect("create /var/log/kernel.log");
    vfs::write_file("/var/log/kernel.log", b"[boot] kernel started\n");
    vfs::append_file("/var/log/kernel.log", b"[alpha] preemptive scheduler active\n");
    vfs::append_file("/var/log/kernel.log", b"[alpha] zero-copy IPC ready\n");

    let hostname = vfs::read_file("/etc/hostname").expect("read hostname");
    serial_println!("[vfs] /etc/hostname = {:?}",
        core::str::from_utf8(&hostname).unwrap_or("").trim_end());

    let log = vfs::read_file("/var/log/kernel.log").expect("read kernel.log");
    serial_println!("[vfs] /var/log/kernel.log ({} bytes):", log.len());
    for line in core::str::from_utf8(&log).unwrap_or("").lines() {
        serial_println!("       {}", line);
    }

    serial_println!("[vfs] ls /:");
    for entry in vfs::list_dir("/") {
        serial_println!("       {}{}", entry.name, if entry.is_dir { "/" } else { "" });
    }
    serial_println!("--- VFS demo complete ---\n");

    // ── BETA 16/17: mprotect + Demand Paging 인커널 검증 ────────────────────
    {
        serial_println!("===========================================");
        serial_println!("  BETA 16: mprotect  /  BETA 17: Demand Paging");
        serial_println!("===========================================");

        use process::vma;

        // 검증 1: lazy VMA 등록 + try_demand_page 직접 호출
        let test_va: u64 = 0x0000_7FFF_1000_0000;
        vma::insert(test_va, 2, vma::PROT_READ | vma::PROT_WRITE, true);

        let cr3: u64;
        unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }

        let ok0 = vma::try_demand_page(test_va,        cr3);
        let ok1 = vma::try_demand_page(test_va + 4096, cr3);
        serial_println!(
            "[beta17] demand alloc: page0={} page1={}  {}",
            ok0, ok1,
            if ok0 && ok1 { "✓" } else { "✗ FAIL" },
        );

        // 검증 2: HHDM을 통한 실제 R/W 왕복
        if ok0 {
            if let Some(phys) = unsafe { crate::paging::virt_to_phys(cr3, test_va) } {
                let hhdm_va = phys + crate::paging::get_hhdm_offset();
                unsafe {
                    let ptr = hhdm_va as *mut u64;
                    *ptr = 0xDEAD_BEEF_1234_5678;
                    let read = *ptr;
                    serial_println!(
                        "[beta17] R/W 왕복: write={:#x} read={:#x}  {}",
                        0xDEAD_BEEF_1234_5678u64, read,
                        if read == 0xDEAD_BEEF_1234_5678 { "✓" } else { "✗ FAIL" },
                    );
                }
            }
        }

        // 검증 3: mprotect RO — PTE_WRITABLE 제거
        // sys_mprotect는 current_cr3() 사용 → 커널 컨텍스트에서 0 반환
        // 직접 set_page_prot으로 PTE 변경을 검증
        vma::update_prot(test_va, 4096, vma::PROT_READ);
        unsafe { crate::paging::set_page_prot(cr3, test_va, 1, vma::PROT_READ); }
        if let Some(pte) = crate::paging::get_user_pte(cr3, test_va) {
            let writable = pte & (1 << 1) != 0;
            serial_println!(
                "[beta16] mprotect RO: PTE_WRITABLE={}  {}",
                writable as u8,
                if !writable { "✓" } else { "✗ FAIL" },
            );
        }

        // 검증 4: mprotect RW 복구 — PTE_WRITABLE 재설정
        vma::update_prot(test_va, 4096, vma::PROT_READ | vma::PROT_WRITE);
        unsafe { crate::paging::set_page_prot(cr3, test_va, 1, vma::PROT_READ | vma::PROT_WRITE); }
        if let Some(pte) = crate::paging::get_user_pte(cr3, test_va) {
            let writable = pte & (1 << 1) != 0;
            serial_println!(
                "[beta16] mprotect RW 복구: PTE_WRITABLE={}  {}",
                writable as u8,
                if writable { "✓" } else { "✗ FAIL" },
            );
        }

        serial_println!("--- BETA 16/17 complete ---\n");
    }

    // ── BETA-X-2 2/3: 비대칭 권한 매핑 + 격리 검증 ──────────────────────────
    serial_println!("===========================================");
    serial_println!("  BETA-X-2 2/3: 비대칭 권한 매핑 + 격리 검증");
    serial_println!("  write_va(HHDM): PTE_WRITABLE=1  (송신자 전용)");
    serial_println!("  read_va(RO_WIN): PTE_WRITABLE=0  (수신자 읽기 전용)");
    serial_println!("===========================================");
    {
        let (write_va, read_va, phys) = crate::paging::alloc_channel_frame();

        let wpte_ok = crate::paging::get_kernel_pte(write_va)
            .map(|p| p & 2 != 0)
            .unwrap_or(false);
        serial_println!("[beta-x2-2] write_va={:#x}  PTE_WRITABLE={}  {}",
            write_va, wpte_ok as u8, if wpte_ok { "✓" } else { "✗ FAIL" });

        let rpte_ok = crate::paging::get_kernel_pte(read_va as u64)
            .map(|p| p & 2 == 0 && p & 1 != 0)
            .unwrap_or(false);
        serial_println!("[beta-x2-3] read_va={:#x}   PTE_WRITABLE=0  {}",
            read_va as u64, if rpte_ok { "✓" } else { "✗ FAIL" });

        const ASYM_PATTERN: u64 = 0x5A5A_DEAD_5A5A_BEEF;
        unsafe { (write_va as *mut u64).write_volatile(ASYM_PATTERN); }
        let rb = unsafe { (read_va as *const u64).read_volatile() };
        serial_println!("[beta-x2-3] W→R 왕복: write={:#x} read={:#x}  {}",
            ASYM_PATTERN, rb, if rb == ASYM_PATTERN { "✓" } else { "✗ FAIL" });

        crate::paging::free_channel_frame(phys);
    }
    serial_println!("--- BETA-X-2 2/3 complete ---\n");

    // ── BETA-X-2 5: Switchless 직접 통신 검증 ────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  BETA-X-2 5: Switchless 직접 통신 검증");
    serial_println!("  도어벨 기반 lock-free send→poll 왕복");
    serial_println!("===========================================");
    {
        const SW_FROM: u64 = 252;
        const SW_TO:   u64 = 253;
        let sw_cap = process::ipc_fast::ensure_channel_cap(SW_FROM, SW_TO, 64);

        let test_data = b"switchless-ok";
        let sent = process::ipc_cap::send_switchless(sw_cap, test_data);
        let doorbell = process::ipc_cap::doorbell_pending(sw_cap);

        let mut recv_buf = [0u8; 64];
        let recv_len = process::ipc_cap::poll_switchless(sw_cap, &mut recv_buf);

        let data_ok = recv_len == Some(test_data.len())
            && recv_buf[..test_data.len()] == *test_data;

        serial_println!(
            "[beta-x2-5] send={}  doorbell={}  recv={:?}B  data={}  {}",
            sent, doorbell, recv_len,
            if data_ok { "일치" } else { "불일치" },
            if sent && data_ok { "✓" } else { "✗ FAIL" },
        );
        process::ipc_fast::drop_channel(SW_FROM, SW_TO);
    }
    serial_println!("--- BETA-X-2 5 complete ---\n");

    // ── BETA-X-2 4: 이상 탐지 + 강제 회수 ───────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  BETA-X-2 4: 이상 탐지 + 강제 회수");
    serial_println!("  가상 pid254→255: 3창 기준선(15/창) → spike(300/창)");
    serial_println!("  EMA 20× 초과 → ANOMALY + 채널 강제 회수");
    serial_println!("===========================================");
    {
        const ANOM_FROM: u64 = 254;
        const ANOM_TO:   u64 = 255;

        for w in 0u64..3 {
            crate::policy::observe_ipc(ANOM_FROM, ANOM_TO, (w + 1) * 15, 0);
            serial_println!("[beta-x2-4] 기준선 창 {}: count={}", w + 1, (w + 1) * 15);
            let t0 = interrupts::handlers::TICK.load(core::sync::atomic::Ordering::Relaxed);
            while interrupts::handlers::TICK.load(core::sync::atomic::Ordering::Relaxed)
                .wrapping_sub(t0) < 40
            {
                unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
            }
        }

        // spike: count 300 추가 → EMA 대비 20× 초과
        crate::policy::observe_ipc(ANOM_FROM, ANOM_TO, 3 * 15 + 300, 0);
        serial_println!("[beta-x2-4] spike 창: count=345 (+300) → 이상 탐지 대기...");
        let t0 = interrupts::handlers::TICK.load(core::sync::atomic::Ordering::Relaxed);
        while interrupts::handlers::TICK.load(core::sync::atomic::Ordering::Relaxed)
            .wrapping_sub(t0) < 40
        {
            unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
        }
    }
    serial_println!("--- BETA-X-2 4 complete ---\n");

    // ── ML 비교 벤치마크: Baseline / ML1 / ML2 / ML3 ─────────────────────────
    // 4개 분류기를 동일한 합성 IPC 트레이스에 독립 실행, 정확도 비교.
    {
        serial_println!("===========================================");
        serial_println!("  ML 분류기 비교 벤치마크");
        serial_println!("  Baseline vs ML1(Bayesian) vs ML2(GBDT) vs ML3(RL)");
        serial_println!("===========================================");

        // ── 내부 헬퍼 함수 (no_std, 힙 없음) ─────────────────────────────────

        // bench_ml2: ml2_gbdt의 standalone 사본 (policy 모듈 의존 없음)
        fn bench_ml2(f_alpha: i64, f_beta: i64, f_rate: i64, f_cold: i64, f_trend: i64) -> i64 {
            let t1: i64 = if f_alpha >= 10 {
                if f_cold == 0 { 200 } else if f_alpha >= 20 { 140 } else { 80 }
            } else if f_rate >= 20 { if f_alpha >= 5 { 60 } else { 20 } } else { 0 };
            let t2: i64 = if f_rate >= 15 {
                if f_alpha >= 6 { 160 } else if f_trend == 1 { 80 } else { 30 }
            } else if f_alpha >= 8 { 60 } else { 0 };
            let t3: i64 = if f_cold >= 2 { -80 }
                else if f_cold >= 1 { if f_alpha >= 12 { 40 } else { -20 } }
                else if f_rate >= 30 { 120 } else { 40 };
            let t4: i64 = if f_beta <= 5 { if f_alpha >= 8 { 100 } else { 30 } }
                else if f_alpha >= 25 { 50 }
                else if f_beta >= 10 { -40 } else { -10 };
            t1 + t2 + t3 + t4
        }

        // print_bench_row: 분류기 결과와 정답 레이블로 H/. 패턴 + 지표 출력
        fn print_bench_row(
            label: &str,
            result:  &[bool; 10],
            truth:   &[bool; 10],
        ) {
            let mut tp = 0u32; let mut tn = 0u32;
            let mut fp = 0u32; let mut fn_ = 0u32;
            let mut first: i32 = -1;
            let mut pat = [b'.'; 10];
            for w in 0..10 {
                let r = result[w]; let t = truth[w];
                if r { pat[w] = b'H'; }
                match (r, t) {
                    (true,  true)  => { tp += 1; if first < 0 { first = w as i32; } }
                    (false, false) => { tn += 1; }
                    (true,  false) => { fp += 1; }
                    (false, true)  => { fn_ += 1; }
                }
            }
            let s = core::str::from_utf8(&pat).unwrap_or("??????????");
            let total = (tp + tn + fp + fn_) as u32;
            let acc = if total > 0 { (tp + tn) * 100 / total } else { 0 };
            crate::serial_println!(
                "  {:12} [{}]  TP={} TN={} FP={} FN={}  Acc={}%  첫HOT={}",
                label, s, tp, tn, fp, fn_, acc,
                if first >= 0 { first } else { -1 },
            );
        }

        // ── 시나리오 정의 ──────────────────────────────────────────────────────
        // ground truth: 이 정도면 HOT이 맞다는 도메인 기준
        //   A 지속: 4창 이상 충분한 IPC → w4부터 HOT
        //   B 스파이크: 갑작스러운 폭증 후 침묵 → 절대 HOT 아님
        //   C HOT→COLD: 4창 활성 후 소멸 → w0-3만 HOT
        //   D 느린성장: 점점 가속 → w7부터 HOT

        let sc_names:  [&str; 4] = [
            "A: 지속HOT  ",
            "B: 스파이크 ",
            "C: HOT→COLD ",
            "D: 느린성장 ",
        ];
        let sc_deltas: [[u64; 10]; 4] = [
            [10, 15, 20, 25, 30, 35, 40, 45, 50, 55],  // A
            [ 0,  0,  0,300,  0,  0,  0,  0,  0,  0],  // B
            [60, 70, 80, 90,  0,  0,  0,  0,  0,  0],  // C
            [ 3,  5,  8, 12, 18, 28, 40, 60, 90,130],  // D
        ];
        let sc_truth: [[bool; 10]; 4] = [
            [false,false,false,false,true,true,true,true,true,true],   // A: w4+
            [false,false,false,false,false,false,false,false,false,false], // B: 없음
            [true,true,true,true,false,false,false,false,false,false], // C: w0-3
            [false,false,false,false,false,false,false,true,true,true],// D: w7+
        ];

        // 시나리오별 집계용 (5분류기 × 4시나리오 Acc 저장: Baseline/ML1/ML2/ML3/Ensemble)
        let mut summary_acc = [[0u32; 5]; 4]; // [scenario][classifier]

        for sc in 0..4usize {
            let deltas = &sc_deltas[sc];
            let truth  = &sc_truth[sc];
            serial_println!("[ml-bench] ── {} deltas={:?}", sc_names[sc], deltas);

            let mut results = [[false; 10]; 5]; // [classifier][window]: 0=Base,1=ML1,2=ML2,3=ML3,4=Ens

            // ── [0] Baseline: 누적 count >= 100 ────────────────────────────────
            {
                let mut cum: u64 = 0;
                for w in 0..10 { cum += deltas[w]; results[0][w] = cum >= 100; }
            }

            // ── [1] ML1 Bayesian: score=α*1000/(α+β+1) >= 650 ─────────────────
            // ML1 튜닝: GAIN_CAP=5(spike 억제), COLD_DECAY=3(빠른 망각)
            {
                let (mut a, mut b) = (1u32, 4u32);
                for w in 0..10 {
                    let d = deltas[w];
                    if d > 0 {
                        a = a.saturating_add(((d / 10) as u32).max(2).min(5)).min(500);
                        b = b.saturating_sub(1).max(4);
                    } else {
                        b = b.saturating_add(1).min(500);
                        a = a.saturating_sub(3).max(1);
                    }
                    let score = (a as u64) * 1000 / (a as u64 + b as u64 + 1);
                    results[1][w] = score >= 650;
                }
            }

            // ── [2] ML2 GBDT: bench_ml2(...) >= 300 ───────────────────────────
            {
                let (mut a, mut b) = (1u32, 4u32);
                let mut rate_ema: u64 = 0;
                let mut cold: u8 = 0;
                let mut dprev: u64 = 0;
                for w in 0..10 {
                    let d = deltas[w];
                    if d > 0 {
                        // 튜닝된 Bayesian 업데이트 (α feature → ML2 입력)
                        a = a.saturating_add(((d / 10) as u32).max(2).min(5)).min(500);
                        b = b.saturating_sub(1).max(4);
                        cold = 0;
                    } else {
                        b = b.saturating_add(1).min(500);
                        a = a.saturating_sub(3).max(1);
                        cold = cold.saturating_add(1);
                    }
                    rate_ema = (3 * d * 10 + 7 * rate_ema) / 10;
                    let trend: i64 = if dprev * 10 > rate_ema { 1 } else { 0 };
                    dprev = d;
                    let score = bench_ml2(
                        a as i64, b as i64, (rate_ema / 10) as i64, cold as i64, trend,
                    );
                    results[2][w] = score >= 300;
                }
            }

            // ── [3] ML3 Q-learning: ε-greedy, 신선한 Q-table ──────────────────
            {
                let (mut a, mut b) = (1u32, 4u32);
                let mut rate_ema: u64 = 0;
                let mut cold: u8 = 0;
                let mut dprev: u64 = 0;
                let mut q: [[i32; 4]; 36] = [[10, 0, 0, 0]; 36]; // NOOP bias
                let mut prev_s: usize = 0;
                let mut action: u8 = 0; // NOOP
                let mut rng: u64 = 0xdeadbeef_12345678u64
                    .wrapping_add(sc as u64 * 0x9e3779b9);
                const EPS: u8 = 20; // 20% 탐색

                for w in 0..10 {
                    let d = deltas[w];
                    if d > 0 {
                        // 튜닝된 Bayesian 업데이트 (α/β → ML3 state feature)
                        a = a.saturating_add(((d / 10) as u32).max(2).min(5)).min(500);
                        b = b.saturating_sub(1).max(4);
                        cold = 0;
                    } else {
                        b = b.saturating_add(1).min(500);
                        a = a.saturating_sub(3).max(1);
                        cold = cold.saturating_add(1);
                    }
                    rate_ema = (3 * d * 10 + 7 * rate_ema) / 10;
                    let trend: i64 = if dprev * 10 > rate_ema { 1 } else { 0 };
                    dprev = d;

                    // ML2 score → action으로 임계값 조정
                    let ml2 = bench_ml2(
                        a as i64, b as i64, (rate_ema / 10) as i64, cold as i64, trend,
                    );
                    let eff_thr: i64 = match action {
                        1 => 220, // ENCOURAGE
                        2 => 380, // DISCOURAGE
                        _ => 300,
                    };
                    results[3][w] = ml2 >= eff_thr || action == 3; // 3=PIN_PUSH

                    // state bucket
                    let ab = if a >= 25 { 3 } else if a >= 10 { 2 } else if a >= 3 { 1 } else { 0 };
                    let rb = if rate_ema / 10 >= 21 { 2 } else if rate_ema / 10 >= 6 { 1 } else { 0 };
                    let cb = if cold >= 2 { 2 } else { cold as usize };
                    let next_s = ab * 9 + rb * 3 + cb;

                    // reward
                    let reward: i32 = if d == 0 {
                        if action == 1 { -4 } else { -1 }
                    } else if d >= 20 {
                        match action { 1 | 3 => 12, 0 => 5, _ => -2 }
                    } else { 2 };

                    // Bellman update
                    let max_n = { let mut m=q[next_s][0]; for k in 1..4 { if q[next_s][k]>m{m=q[next_s][k];} } m };
                    let td = reward * 100 + (9 * max_n) / 10 - q[prev_s][action as usize];
                    q[prev_s][action as usize] += td / 10;

                    // ε-greedy next action
                    let r1 = { let mut x=rng; x^=x<<13; x^=x>>7; x^=x<<17; rng=x; x % 100 };
                    action = if r1 < EPS as u64 {
                        let r2 = { let mut x=rng; x^=x<<13; x^=x>>7; x^=x<<17; rng=x; x };
                        (r2 % 4) as u8
                    } else {
                        let mut best=0u8; let mut bq=q[next_s][0];
                        for k in 1..4usize { if q[next_s][k]>bq{bq=q[next_s][k];best=k as u8;} }
                        best
                    };
                    prev_s = next_s;
                }
            }

            // ── [4] ML4 Ensemble: ML1 AND ML2 동시 동의 시에만 HOT ──────────────
            // 두 분류기가 모두 HOT이어야 채택 → FP 감소, recall 유지
            for w in 0..10 {
                results[4][w] = results[1][w] && results[2][w];
            }

            // 결과 출력 + 집계
            let clf_names = ["Baseline", "ML1 Bay ", "ML2 GBDT", "ML3 RL  ", "ML4 Ens "];
            for c in 0..5 {
                // accuracy 계산
                let r = &results[c]; let t = truth;
                let mut correct = 0u32;
                for w in 0..10 { if r[w] == t[w] { correct += 1; } }
                summary_acc[sc][c] = correct * 100 / 10;
                print_bench_row(clf_names[c], r, t);
            }
            serial_println!("");
        }

        // ── 전체 요약 ──────────────────────────────────────────────────────────
        serial_println!("[ml-bench] ── 전체 정확도 요약 (각 시나리오 10창 기준) ──");
        serial_println!("  분류기    │  A지속  B스파  C전환  D성장 │  평균");
        for c in 0..5 {
            let clf_names = ["Baseline", "ML1 Bay ", "ML2 GBDT", "ML3 RL  ", "ML4 Ens "];
            let a = summary_acc[0][c];
            let b = summary_acc[1][c];
            let cc = summary_acc[2][c];
            let d = summary_acc[3][c];
            let avg = (a + b + cc + d) / 4;
            serial_println!("  {} │  {:3}%  {:3}%  {:3}%  {:3}% │  {:3}%",
                clf_names[c], a, b, cc, d, avg);
        }
        serial_println!("[ml-bench] complete ---\n");

        // ── ML3 장기 수렴 실험: 100창 × 4시나리오, 25창 단위 정확도 추적 ─────
        // 10창 평가에서 "과소학습"으로 결론 보류된 ML3 Q-learning을
        // 100창(동일 패턴 10회 반복)으로 재평가. Q-table이 수렴하는지 확인.
        {
            serial_println!("[ml3-conv] ── ML3 장기 수렴 실험 (4시나리오 × 100창) ──");
            serial_println!("[ml3-conv]   시나리오    │  w0-24  w25-49 w50-74 w75-99 │ 전체");

            let sc_disp: [&str; 4] = ["A 지속HOT ", "B 스파이크", "C HOT→COLD", "D 느린성장"];

            for sc in 0..4usize {
                let deltas = &sc_deltas[sc];
                let truth  = &sc_truth[sc];

                // 시나리오별 독립 Q-table (학습 전부터 시작)
                let mut q: [[i32; 4]; 36] = [[10, 0, 0, 0]; 36];
                let mut prev_s: usize = 0;
                let mut action: u8 = 0;
                let mut rng: u64 = 0xfeed_dead_cafe_beefu64.wrapping_add(sc as u64 * 7919);
                const EPS_CONV: u8 = 15; // 15% 탐색 (초반 학습용)

                let (mut a, mut b) = (1u32, 4u32);
                let mut rate_ema: u64 = 0;
                let mut cold: u8 = 0;
                let mut dprev: u64 = 0;

                let mut blk_ok = [0u32; 4]; // 25창 블록별 정답 수

                for w in 0..100usize {
                    let d = deltas[w % 10];

                    // Bayesian 업데이트 (튜닝 파라미터)
                    if d > 0 {
                        a = a.saturating_add(((d / 10) as u32).max(2).min(5)).min(500);
                        b = b.saturating_sub(1).max(4);
                        cold = 0;
                    } else {
                        b = b.saturating_add(1).min(500);
                        a = a.saturating_sub(3).max(1);
                        cold = cold.saturating_add(1);
                    }
                    rate_ema = (3 * d * 10 + 7 * rate_ema) / 10;
                    let trend: i64 = if dprev * 10 > rate_ema { 1 } else { 0 };
                    dprev = d;

                    // ML2 score
                    let ml2_s = bench_ml2(
                        a as i64, b as i64, (rate_ema / 10) as i64, cold as i64, trend,
                    );
                    let eff: i64 = match action { 1 => 280, 3 => 0, _ => 300 };
                    let hot = ml2_s >= eff || action == 3;

                    if hot == truth[w % 10] { blk_ok[w / 25] += 1; }

                    // RL state
                    let ab = if a>=25{3}else if a>=10{2}else if a>=3{1}else{0};
                    let rb = if rate_ema/10>=21{2}else if rate_ema/10>=6{1}else{0};
                    let cb = if cold>=2{2}else{cold as usize};
                    let ns = ab * 9 + rb * 3 + cb;

                    // reward
                    let rew: i32 = if d == 0 {
                        if action == 1 { -4 } else { -1 }
                    } else if d >= 20 {
                        match action { 1|3=>12, 0=>5, _=>-2 }
                    } else { 2 };

                    // Bellman update
                    let mx = {let mut m=q[ns][0];for k in 1..4{if q[ns][k]>m{m=q[ns][k];}}m};
                    let td = rew*100 + (9*mx)/10 - q[prev_s][action as usize];
                    q[prev_s][action as usize] += td / 10;

                    // ε-greedy
                    let r1={let mut x=rng;x^=x<<13;x^=x>>7;x^=x<<17;rng=x;x%100};
                    action = if r1 < EPS_CONV as u64 {
                        let r2={let mut x=rng;x^=x<<13;x^=x>>7;x^=x<<17;rng=x;x};
                        (r2%4) as u8
                    } else {
                        let mut best=0u8;let mut bq=q[ns][0];
                        for k in 1..4usize{if q[ns][k]>bq{bq=q[ns][k];best=k as u8;}}
                        best
                    };
                    prev_s = ns;
                }

                let a0=blk_ok[0]*100/25; let a1=blk_ok[1]*100/25;
                let a2=blk_ok[2]*100/25; let a3=blk_ok[3]*100/25;
                let tot=blk_ok[0]+blk_ok[1]+blk_ok[2]+blk_ok[3];
                serial_println!("[ml3-conv]   {} │  {:3}%  {:3}%  {:3}%  {:3}% │ {:3}%",
                    sc_disp[sc], a0, a1, a2, a3, tot);
            }
            serial_println!("[ml3-conv] complete ---\n");
        }

        // ── ML3 공정 재평가: 매 사이클 Bayesian 리셋, Q-table 누적 ──────────────
        // 실험 10에서 발견된 "상태 누적 오염" 제거.
        // 10창 사이클 × 10회 반복 = 100창. 각 사이클 시작마다 α/β/EMA 리셋.
        // Q-table만 사이클 간 유지 → 순수한 RL 수렴 측정.
        {
            serial_println!("[ml3-fair] ── ML3 공정 재평가 (Bayesian 리셋/Q-table 누적, 100창) ──");
            serial_println!("[ml3-fair]   시나리오    │  w0-24  w25-49 w50-74 w75-99 │ 전체");

            let sc_disp2: [&str; 4] = ["A 지속HOT ", "B 스파이크", "C HOT→COLD", "D 느린성장"];

            for sc in 0..4usize {
                let deltas = &sc_deltas[sc];
                let truth  = &sc_truth[sc];

                // Q-table은 시나리오별 독립, 100창 내내 누적
                let mut q: [[i32; 4]; 36] = [[10, 0, 0, 0]; 36];
                let mut rng: u64 = 0xabcd_1234_ef56_7890u64.wrapping_add(sc as u64 * 6271);
                const EPS_FAIR: u8 = 20;

                let mut blk_ok = [0u32; 4];

                for cycle in 0..10usize {
                    // 매 사이클 Bayesian·EMA 리셋 (Q-table은 유지)
                    let (mut a, mut b) = (1u32, 4u32);
                    let mut rate_ema: u64 = 0;
                    let mut cold: u8 = 0;
                    let mut dprev: u64 = 0;
                    let mut prev_s: usize = 0;
                    let mut action: u8 = 0;

                    for w in 0..10usize {
                        let d = deltas[w];

                        // Bayesian 업데이트 (튜닝 파라미터)
                        if d > 0 {
                            a = a.saturating_add(((d/10) as u32).max(2).min(5)).min(500);
                            b = b.saturating_sub(1).max(4);
                            cold = 0;
                        } else {
                            b = b.saturating_add(1).min(500);
                            a = a.saturating_sub(3).max(1);
                            cold = cold.saturating_add(1);
                        }
                        rate_ema = (3 * d * 10 + 7 * rate_ema) / 10;
                        let trend: i64 = if dprev * 10 > rate_ema { 1 } else { 0 };
                        dprev = d;

                        // ML2 + ML3 action
                        let ml2_s = bench_ml2(
                            a as i64, b as i64, (rate_ema/10) as i64, cold as i64, trend,
                        );
                        let eff: i64 = match action { 1=>280, 3=>0, _=>300 };
                        let hot = ml2_s >= eff || action == 3;

                        let gw = cycle * 10 + w;
                        if hot == truth[w] { blk_ok[gw / 25] += 1; }

                        // RL state bucket
                        let ab = if a>=25{3}else if a>=10{2}else if a>=3{1}else{0};
                        let rb = if rate_ema/10>=21{2}else if rate_ema/10>=6{1}else{0};
                        let cb = if cold>=2{2}else{cold as usize};
                        let ns = ab*9 + rb*3 + cb;

                        // reward
                        let rew: i32 = if d == 0 {
                            if action == 1 { -4 } else { -1 }
                        } else if d >= 20 {
                            match action { 1|3=>12, 0=>5, _=>-2 }
                        } else { 2 };

                        // Bellman update
                        let mx={let mut m=q[ns][0];for k in 1..4{if q[ns][k]>m{m=q[ns][k];}}m};
                        let td = rew*100 + (9*mx)/10 - q[prev_s][action as usize];
                        q[prev_s][action as usize] += td/10;

                        // ε-greedy
                        let r1={let mut x=rng;x^=x<<13;x^=x>>7;x^=x<<17;rng=x;x%100};
                        action = if r1 < EPS_FAIR as u64 {
                            let r2={let mut x=rng;x^=x<<13;x^=x>>7;x^=x<<17;rng=x;x};
                            (r2%4) as u8
                        } else {
                            let mut best=0u8; let mut bq=q[ns][0];
                            for k in 1..4usize{if q[ns][k]>bq{bq=q[ns][k];best=k as u8;}}
                            best
                        };
                        prev_s = ns;
                    }
                }

                let a0=blk_ok[0]*100/25; let a1=blk_ok[1]*100/25;
                let a2=blk_ok[2]*100/25; let a3=blk_ok[3]*100/25;
                let tot = blk_ok[0]+blk_ok[1]+blk_ok[2]+blk_ok[3];
                serial_println!("[ml3-fair]   {} │  {:3}%  {:3}%  {:3}%  {:3}% │ {:3}%",
                    sc_disp2[sc], a0, a1, a2, a3, tot);
            }
            serial_println!("[ml3-fair] complete ---\n");
        }

        // ── ML3 보상 함수 수정 + 공정 재평가 ──────────────────────────────────
        // 실험 12 발견: 기존 보상(d≥20→+12)이 spike에서 ENCOURAGE를 학습시킴.
        // 수정: prev_delta < 5 && d >= 50이면 spike → DISCOURAGE 보상(+10).
        {
            serial_println!("[ml3-fix] ── ML3 보상 재설계 + 공정 재평가 ──");
            serial_println!("[ml3-fix]   시나리오    │  w0-24  w25-49 w50-74 w75-99 │ 전체");

            let sc_disp3: [&str; 4] = ["A 지속HOT ", "B 스파이크", "C HOT→COLD", "D 느린성장"];

            for sc in 0..4usize {
                let deltas = &sc_deltas[sc];
                let truth  = &sc_truth[sc];

                let mut q: [[i32; 4]; 36] = [[10, 0, 0, 0]; 36];
                let mut rng: u64 = 0x1234_abcd_5678_ef90u64.wrapping_add(sc as u64 * 5381);
                const EPS_FIX: u8 = 10; // 낮춰서 PIN_PUSH 랜덤 탐색 FP 감소 확인

                let mut blk_ok = [0u32; 4];

                for cycle in 0..10usize {
                    // 매 사이클 Bayesian·EMA 리셋
                    let (mut a, mut b) = (1u32, 4u32);
                    let mut rate_ema: u64 = 0;
                    let mut cold: u8 = 0;
                    let mut dprev: u64 = 0; // 진짜 이전 창 delta (spike 판별용)
                    let mut prev_s: usize = 0;
                    let mut action: u8 = 0;

                    for w in 0..10usize {
                        let d = deltas[w];
                        let prev_d = dprev; // 이전 창 delta 저장

                        if d > 0 {
                            a = a.saturating_add(((d/10) as u32).max(2).min(5)).min(500);
                            b = b.saturating_sub(1).max(4);
                            cold = 0;
                        } else {
                            b = b.saturating_add(1).min(500);
                            a = a.saturating_sub(3).max(1);
                            cold = cold.saturating_add(1);
                        }
                        rate_ema = (3 * d * 10 + 7 * rate_ema) / 10;
                        let trend: i64 = if prev_d * 10 > rate_ema { 1 } else { 0 };
                        dprev = d;

                        let ml2_s = bench_ml2(
                            a as i64, b as i64, (rate_ema/10) as i64, cold as i64, trend,
                        );
                        // DISCOURAGE: eff=9999로 올려서 스파이크 창에서 HOT 차단
                        // 기존에는 DISCOURAGE도 eff=300이라 ML2=370인 스파이크를 막지 못했음
                        let eff: i64 = match action { 1=>280, 2=>9999, 3=>0, _=>300 };
                        let hot = ml2_s >= eff || action == 3;

                        let gw = cycle * 10 + w;
                        if hot == truth[w] { blk_ok[gw / 25] += 1; }

                        let ab = if a>=25{3}else if a>=10{2}else if a>=3{1}else{0};
                        let rb = if rate_ema/10>=21{2}else if rate_ema/10>=6{1}else{0};
                        let cb = if cold>=2{2}else{cold as usize};
                        let ns = ab*9 + rb*3 + cb;

                        // 수정된 보상 함수 (spike-aware)
                        // NOOP=+1→-3: 양수이면 NOOP이 계속 강화돼서 DISCOURAGE로 못 넘어감
                        let is_spike = d >= 20 && prev_d < 5 && d >= 50;
                        let rew: i32 = if d == 0 {
                            if action == 1 { -4 } else { -1 }
                        } else if is_spike {
                            match action { 2=>10, 0=>-3, _=>-8 } // DISCOURAGE=2, NOOP=-3
                        } else if d >= 20 {
                            match action { 1|3=>12, 0=>5, _=>-2 }
                        } else { 2 };

                        let mx={let mut m=q[ns][0];for k in 1..4{if q[ns][k]>m{m=q[ns][k];}}m};
                        let td = rew*100 + (9*mx)/10 - q[prev_s][action as usize];
                        q[prev_s][action as usize] += td/10;

                        let r1={let mut x=rng;x^=x<<13;x^=x>>7;x^=x<<17;rng=x;x%100};
                        action = if r1 < EPS_FIX as u64 {
                            let r2={let mut x=rng;x^=x<<13;x^=x>>7;x^=x<<17;rng=x;x};
                            (r2%4) as u8
                        } else {
                            let mut best=0u8; let mut bq=q[ns][0];
                            for k in 1..4usize{if q[ns][k]>bq{bq=q[ns][k];best=k as u8;}}
                            best
                        };
                        prev_s = ns;
                    }
                }

                let a0=blk_ok[0]*100/25; let a1=blk_ok[1]*100/25;
                let a2=blk_ok[2]*100/25; let a3=blk_ok[3]*100/25;
                let tot = blk_ok[0]+blk_ok[1]+blk_ok[2]+blk_ok[3];
                serial_println!("[ml3-fix]   {} │  {:3}%  {:3}%  {:3}%  {:3}% │ {:3}%",
                    sc_disp3[sc], a0, a1, a2, a3, tot);
            }
            serial_println!("[ml3-fix] complete ---\n");

            // ── [ml3-conv2] 학습/평가 분리: 50사이클 학습(ε=20%) → 50사이클 평가(ε=0) ──
            // 탐색 노이즈 없는 수렴된 Q-table의 진짜 성능 측정
            serial_println!("[ml3-conv2] ── ML3 train(50cy)/eval(50cy) 분리 실험 ──");
            serial_println!("[ml3-conv2]   시나리오    │  eval평균 │ 비고");
            {
                let sc_disp4: [&str; 4] = ["A 지속HOT ", "B 스파이크", "C HOT→COLD", "D 느린성장"];
                for sc in 0..4usize {
                    let deltas = &sc_deltas[sc];
                    let truth  = &sc_truth[sc];
                    let mut q: [[i32; 4]; 36] = [[10, 0, 0, 0]; 36];
                    let mut rng: u64 = 0x1234_abcd_5678_ef90u64.wrapping_add(sc as u64 * 5381);

                    // Phase 1: 학습 (50사이클, ε=20%)
                    for _cy in 0..50usize {
                        let (mut a, mut b) = (1u32, 4u32);
                        let mut rate_ema: u64 = 0;
                        let mut cold: u8 = 0;
                        let mut dprev: u64 = 0;
                        let mut prev_s: usize = 0;
                        let mut action: u8 = 0;
                        for w in 0..10usize {
                            let d = deltas[w];
                            let prev_d = dprev;
                            if d > 0 {
                                a = a.saturating_add(((d/10) as u32).max(2).min(5)).min(500);
                                b = b.saturating_sub(1).max(4);
                                cold = 0;
                            } else {
                                b = b.saturating_add(1).min(500);
                                a = a.saturating_sub(3).max(1);
                                cold = cold.saturating_add(1);
                            }
                            rate_ema = (3*d*10 + 7*rate_ema)/10;
                            dprev = d;
                            let ab = if a>=25{3}else if a>=10{2}else if a>=3{1}else{0};
                            let rb = if rate_ema/10>=21{2}else if rate_ema/10>=6{1}else{0};
                            let cb = if cold>=2{2}else{cold as usize};
                            let ns = ab*9 + rb*3 + cb;
                            let is_spike = d>=20 && prev_d<5 && d>=50;
                            let rew: i32 = if d==0 {
                                if action==1{-4}else{-1}
                            } else if is_spike {
                                match action{2=>10,0=>-3,_=>-8}
                            } else if d>=20 {
                                match action{1|3=>12,0=>5,_=>-2}
                            } else {2};
                            let mx={let mut m=q[ns][0];for k in 1..4{if q[ns][k]>m{m=q[ns][k];}}m};
                            let td=rew*100+(9*mx)/10-q[prev_s][action as usize];
                            q[prev_s][action as usize]+=td/10;
                            let r1={let mut x=rng;x^=x<<13;x^=x>>7;x^=x<<17;rng=x;x%100};
                            action = if r1<20 {
                                let r2={let mut x=rng;x^=x<<13;x^=x>>7;x^=x<<17;rng=x;x};
                                (r2%4) as u8
                            } else {
                                let mut best=0u8;let mut bq=q[ns][0];
                                for k in 1..4{if q[ns][k]>bq{bq=q[ns][k];best=k as u8;}}
                                best
                            };
                            prev_s = ns;
                        }
                    }

                    // Phase 2: 평가 (50사이클, ε=0 exploit-only)
                    let mut eval_ok = 0u32;
                    for _cy in 0..50usize {
                        let (mut a, mut b) = (1u32, 4u32);
                        let mut rate_ema: u64 = 0;
                        let mut cold: u8 = 0;
                        let mut dprev2: u64 = 0;
                        let mut action: u8 = 0;
                        for w in 0..10usize {
                            let d = deltas[w];
                            let prev_d2 = dprev2;
                            if d > 0 {
                                a = a.saturating_add(((d/10) as u32).max(2).min(5)).min(500);
                                b = b.saturating_sub(1).max(4);
                                cold = 0;
                            } else {
                                b = b.saturating_add(1).min(500);
                                a = a.saturating_sub(3).max(1);
                                cold = cold.saturating_add(1);
                            }
                            rate_ema = (3*d*10 + 7*rate_ema)/10;
                            dprev2 = d;
                            let trend2: i64 = if prev_d2*10 > rate_ema {1} else {0};
                            let ml2_s = bench_ml2(
                                a as i64, b as i64, (rate_ema/10) as i64, cold as i64, trend2,
                            );
                            let eff: i64 = match action { 1=>280, 2=>9999, 3=>0, _=>300 };
                            let hot = ml2_s >= eff || action == 3;
                            if hot == truth[w] { eval_ok += 1; }
                            let ab = if a>=25{3}else if a>=10{2}else if a>=3{1}else{0};
                            let rb = if rate_ema/10>=21{2}else if rate_ema/10>=6{1}else{0};
                            let cb = if cold>=2{2}else{cold as usize};
                            let ns = ab*9 + rb*3 + cb;
                            // exploit-only (ε=0)
                            let mut best=0u8;let mut bq=q[ns][0];
                            for k in 1..4{if q[ns][k]>bq{bq=q[ns][k];best=k as u8;}}
                            action = best;
                        }
                    }
                    let eval_pct = eval_ok * 100 / 500;
                    serial_println!("[ml3-conv2]   {} │    {:3}%    │ 50cy 학습 후 ε=0",
                        sc_disp4[sc], eval_pct);
                }
            }
            serial_println!("[ml3-conv2] complete ---\n");
        }
    }

    // ── BETA-X 3: WM ↔ GFX 드라이버 동적 IPC 채널 시연 ─────────────────────
    serial_println!("===========================================");
    serial_println!("  BETA-X 3: WM <-> GFX 드라이버 fast channel");
    serial_println!("  Phase1: 일반 IPC 120회 → 임계값 초과");
    serial_println!("  Phase2: Policy Engine 자동 채널 생성 → fast path");
    serial_println!("===========================================");

    // GFX 태스크 먼저 스폰 (PID를 GFX_PID 전역에 등록)
    let gfx_pid_val = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(gfx_pid_val, "gfx_drv", gfx_ipc::gfx_task)
    );
    // WM 태스크 스폰 (GFX_PID 전역 읽어 gfx_pid 확인 후 전송 시작)
    let wm_pid_val = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(wm_pid_val, "wm_task", gfx_ipc::wm_task)
    );

    // Policy Engine 리포트 주기(36틱 ≈ 2초)의 두 배 대기
    // → adapt_and_report 최소 1회 이상 실행 보장 → fast channel 생성 확인
    let betax3_start = interrupts::handlers::TICK.load(Ordering::Relaxed);
    while interrupts::handlers::TICK.load(Ordering::Relaxed) - betax3_start < 72 {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }

    let ch_count = process::ipc_fast::channel_count();
    serial_println!(
        "[beta-x3] 완료: fast channels={} ({})",
        ch_count,
        if ch_count > 0 { "fast path 활성화 확인 ✓" } else { "Policy Engine 리포트 미발생" },
    );
    process::scheduler::kill_pid(gfx_pid_val);
    process::scheduler::kill_pid(wm_pid_val);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("--- BETA-X 3 demo complete ---\n");

    // ── BETA-X 4: 마우스/키보드 → 포그라운드 앱 입력 경로 직통화 ─────────────
    serial_println!("===========================================");
    serial_println!("  BETA-X 4: 입력 직통 경로 (Input Direct Path)");
    serial_println!("  IRQ → AtomicRing → input_drv → fast channel → foreground");
    serial_println!("===========================================");

    // 1) 포그라운드 앱 (수신자) 먼저 스폰 → FOREGROUND_PID 등록
    let inp_consumer = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(inp_consumer, "input_app", input_direct::input_consumer_task)
    );
    // 2) 입력 드라이버 태스크 스폰 → INPUT_DRV_PID 등록
    let inp_drv = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(inp_drv, "input_drv", input_direct::input_drv_task)
    );
    // 3) 합성 이벤트 생성기 스폰 (실제 키 입력 없이도 threshold 돌파)
    let key_gen = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(key_gen, "key_gen", input_direct::key_gen_task)
    );

    // Policy Engine adapt_and_report × 2회 이상 실행 대기 (72틱 ≈ 4초)
    let betax4_start = interrupts::handlers::TICK.load(Ordering::Relaxed);
    while interrupts::handlers::TICK.load(Ordering::Relaxed) - betax4_start < 72 {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }

    let inp_channels = process::ipc_fast::channel_count();
    serial_println!(
        "[beta-x4] 완료: fast channels={} ({})",
        inp_channels,
        if inp_channels > 0 { "입력 직통 fast path 확인 ✓" } else { "Policy Engine 리포트 미발생" },
    );
    process::scheduler::kill_pid(inp_consumer);
    process::scheduler::kill_pid(inp_drv);
    process::scheduler::kill_pid(key_gen);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("--- BETA-X 4 demo complete ---\n");

    // ── BETA-X 6: 채널 회수(Decay) ────────────────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  BETA-X 6: 채널 회수 (Decay)");
    serial_println!("  130회 버스트 → fast channel 생성");
    serial_println!("  이후 침묵 → cold {} 창 → 자동 채널 해제", 2u8);
    serial_println!("===========================================");

    DECAY_RECV_PID.store(0, Ordering::Relaxed);

    let decay_recv = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(decay_recv, "decay_recv", decay_recv_task)
    );
    let decay_send = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(decay_send, "decay_send", decay_send_task)
    );

    // 1창(36틱) 대기 → Policy Engine이 hot pair 감지 → fast channel 생성
    let betax6_ch_wait = interrupts::handlers::TICK.load(Ordering::Relaxed);
    while interrupts::handlers::TICK.load(Ordering::Relaxed) - betax6_ch_wait < 40 {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }
    serial_println!(
        "[beta-x6] 채널 생성 후: fast_channels={} affinity={}",
        process::ipc_fast::channel_count(),
        smp::affinity_count(),
    );

    // 2창(72틱) 추가 대기 → DECAY_COLD_WINDOWS=2 충족 → 채널 회수
    let betax6_decay_wait = interrupts::handlers::TICK.load(Ordering::Relaxed);
    while interrupts::handlers::TICK.load(Ordering::Relaxed) - betax6_decay_wait < 80 {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }
    serial_println!(
        "[beta-x6] 채널 회수 후: fast_channels={} affinity={}",
        process::ipc_fast::channel_count(),
        smp::affinity_count(),
    );

    process::scheduler::kill_pid(decay_recv);
    process::scheduler::kill_pid(decay_send);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("--- BETA-X 6 demo complete ---\n");

    // ── BETA-X 7: IPC 레이턴시 A/B 벤치마크 ─────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  BETA-X 7: IPC 레이턴시 A/B 벤치마크");
    serial_println!("  Phase A: 일반 IPC  Phase B: fast channel");
    serial_println!("  rdtsc(send)→rdtsc(recv) 사이클 측정 n={}", bench_ipc::BENCH_N);
    serial_println!("===========================================");

    let b7_recv = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(b7_recv, "bench_recv", bench_ipc::bench_receiver_task)
    );
    let b7_send = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(b7_send, "bench_send", bench_ipc::bench_sender_task)
    );

    // Phase 3(완료) + 전체 샘플 수집 대기
    loop {
        let phase = bench_ipc::BENCH_PHASE.load(Ordering::Relaxed);
        let idx   = bench_ipc::BENCH_IDX.load(Ordering::Relaxed);
        if phase >= 3 && idx >= bench_ipc::BENCH_N * 2 { break; }
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }

    bench_ipc::report();

    process::scheduler::kill_pid(b7_recv);
    process::scheduler::kill_pid(b7_send);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("--- BETA-X 7 demo complete ---\n");

    // ── Policy B: 메모리 압력 + 전력 신호 관찰 ──────────────────────────────
    serial_println!("===========================================");
    serial_println!("  Policy B: 메모리·전력 관찰");
    serial_println!("  B-1: mmap 압력 계측 → 우선순위 자동 조정");
    serial_println!("  B-2: CPU 유휴율 + APERF/MPERF 주파수 추적");
    serial_println!("===========================================");

    // Phase 1: mem_stress + cpu_stress 동시 실행 → "고부하 + 메모리 압박" 창
    let pb_mem = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(pb_mem, "mem_stress", mem_stress_task)
    );
    let pb_cpu = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(pb_cpu, "cpu_stress", cpu_stress_task)
    );

    serial_println!("[policy-B] Phase 1: 고부하 + 메모리 압박 관찰 중...");
    let pb_start = interrupts::handlers::TICK.load(Ordering::Relaxed);
    while interrupts::handlers::TICK.load(Ordering::Relaxed) - pb_start < 40 {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }
    serial_println!("[policy-B] Phase 1 완료 (Policy Engine 리포트 1회 이상 실행됨)\n");

    // Phase 2: cpu_stress 종료 → 유휴율 상승 관찰
    process::scheduler::kill_pid(pb_cpu);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("[policy-B] Phase 2: cpu_stress 종료 → 유휴율 상승 관찰 중...");
    let pb2_start = interrupts::handlers::TICK.load(Ordering::Relaxed);
    while interrupts::handlers::TICK.load(Ordering::Relaxed) - pb2_start < 40 {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }
    serial_println!("[policy-B] Phase 2 완료 — 유휴율 변화 로그 확인\n");

    process::scheduler::kill_pid(pb_mem);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("--- Policy B demo complete ---\n");

    // ── BETA-X A-1: 페이로드 크기 스윕 ──────────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  BETA-X A-1: 페이로드 크기 스윕");
    serial_println!("  64B / 128B / 256B / 512B / 1024B");
    serial_println!("  Phase A: 일반 IPC  Phase B: fast channel");
    serial_println!("  각 크기당 n={} 샘플 (이상치 12.5% 제거)", bench_a1::N_PER);
    serial_println!("===========================================");

    bench_a1::A1_IDX.store(0, Ordering::Relaxed);
    bench_a1::A1_DONE.store(0, Ordering::Relaxed);
    bench_a1::A1_RECV_PID.store(0, Ordering::Relaxed);

    let a1_recv = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(a1_recv, "a1_recv", bench_a1::a1_receiver_task)
    );
    let a1_send = process::scheduler::alloc_pid();
    process::scheduler::spawn(
        process::Process::new(a1_send, "a1_send", bench_a1::a1_sender_task)
    );

    // 완료 대기 (크기 4개 × 2페이즈 × 32샘플 = 256)
    loop {
        if bench_a1::A1_DONE.load(Ordering::Relaxed) == 1
            && bench_a1::A1_IDX.load(Ordering::Relaxed) >= bench_a1::N_PER * 8
        {
            break;
        }
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }

    bench_a1::report();

    process::scheduler::kill_pid(a1_recv);
    process::scheduler::kill_pid(a1_send);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    serial_println!("--- BETA-X A-1 complete ---\n");

    // ── BETA-X A-2: 멀티코어 레이턴시 비교 ──────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  BETA-X A-2: 멀티코어 IPC 레이턴시 비교");
    serial_println!("  Phase A: AP1→BSP cross-core AtomicU64 ping-pong");
    serial_println!("  Phase B: BSP→BSP same-core yield ping-pong");
    serial_println!("  n={} samples each", bench_a2::N);
    serial_println!("===========================================");

    if smp::ap_count() >= 1 {
        // Phase A: AP #0은 bootstrap 시점부터 ap_sender_phase_a()를 실행 중.
        // assign_ap_work 불필요 — BSP가 receiver로 진입하면 A2_PONG=1로 시작 신호.
        bench_a2::A2_PING.store(0, Ordering::Relaxed);
        bench_a2::A2_PONG.store(0, Ordering::Relaxed);
        bench_a2::A2_IDX_A.store(0, Ordering::Relaxed);
        bench_a2::A2_DONE.store(1, Ordering::Relaxed);

        // BSP가 receiver로 동작 (블로킹 루프, AP1 ping 대기)
        bench_a2::bsp_receiver_phase_a();

        // Phase A 완료 대기 (A2_DONE=2 설정됨)
        while bench_a2::A2_DONE.load(Ordering::Relaxed) < 2 {
            unsafe { core::arch::asm!("pause", options(nomem, nostack, preserves_flags)); }
        }

        // Phase B: BSP에서 sender/receiver를 스케줄러 프로세스로 실행
        bench_a2::A2_PING.store(0, Ordering::Relaxed);
        bench_a2::A2_PONG.store(0, Ordering::Relaxed);
        bench_a2::A2_IDX_B.store(0, Ordering::Relaxed);

        let a2_send = process::scheduler::alloc_pid();
        process::scheduler::spawn(
            process::Process::new(a2_send, "a2_send", bench_a2::bsp_b_sender_task)
        );
        let a2_recv = process::scheduler::alloc_pid();
        process::scheduler::spawn(
            process::Process::new(a2_recv, "a2_recv", bench_a2::bsp_b_receiver_task)
        );

        // Phase B 완료 대기 (A2_DONE=3)
        loop {
            if bench_a2::A2_DONE.load(Ordering::Relaxed) >= 3 { break; }
            unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
        }

        process::scheduler::kill_pid(a2_send);
        process::scheduler::kill_pid(a2_recv);
        process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)

        bench_a2::report();
    } else {
        serial_println!("[a2] AP 없음 (단일코어 모드) — A-2 건너뜀");
        serial_println!("[a2] QEMU 실행 시 -smp 4 옵션 확인 (Makefile에 이미 포함)");
    }

    // ── PE-4: Policy Engine A/B 벤치마크 ─────────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  PE-4: Policy Engine on/off A/B 벤치마크");
    serial_println!("  워크로드: cpu_hog(배경) + kbd_task(전경, n={})", bench_pe4::N_SAMPLES);
    serial_println!("===========================================");

    policy::set_enabled(true);
    let pe_on = bench_pe4::run("PE=ON ");

    policy::set_enabled(false);
    let pe_off = bench_pe4::run("PE=OFF");
    policy::set_enabled(true); // 이후 데모에 영향 없도록 원복

    bench_pe4::report_ab(pe_on, pe_off);
    serial_println!("--- PE-4 complete ---\n");

    // ── CFS-1: 스케줄러 A/B 벤치마크 (실험 33) ───────────────────────────────
    // PE-5(실험 19)에서 "Linux CFS가 처리량 희생 없이 비슷한 반응성을 달성 —
    // MuKernel의 이진 High/Low 분류가 구조적 한계"라는 결론을 냈고, 실험
    // 31/32에서 기존 weighted round-robin+aging이 구조적으로 starvation에
    // 취약함을 실측 확인했다. vruntime 기반 CFS 모드가 별도 안전장치 없이도
    // 이 문제를 구조적으로 해결하는지, PE-4와 동일한 워크로드로 비교한다.
    // "이긴다"가 목표가 아니라 구조적 차이를 정직하게 기록하는 것이 목표.
    serial_println!("===========================================");
    serial_println!("  CFS-1: 스케줄러 A/B (WeightedPriority vs CFS, 실험 33)");
    serial_println!("  워크로드: cpu_hog(배경) + kbd_task(전경, n={})", bench_pe4::N_SAMPLES);
    serial_println!("===========================================");

    process::scheduler::set_mode(process::scheduler::SchedMode::WeightedPriority);
    let sched_wp = bench_pe4::run("WP    ");

    process::scheduler::set_mode(process::scheduler::SchedMode::Cfs);
    let sched_cfs = bench_pe4::run("CFS   ");
    process::scheduler::set_mode(process::scheduler::SchedMode::WeightedPriority); // 이후 데모 원복 (실험 45: CFS 기본값 시도 후 회귀 발견, 롤백)

    bench_pe4::report_ab_labeled(
        "CFS-1: 스케줄러 A/B 비교 결과 (WeightedPriority vs CFS)",
        "WP", sched_wp, "CFS", sched_cfs,
    );
    serial_println!("--- CFS-1 complete ---\n");

    // ── CFS-2: 반복 시행 + starvation 재현 (실험 34) ─────────────────────────
    // 실험 33(CFS-1)은 WP/CFS 각 1회씩만 돈 예비 비교였다. QEMU TCG 실행시간
    // 변동성이 커서(같은 지점 도달까지 500~1500초 편차 관측) 그 결과가
    // 노이즈인지 실제 패턴인지 확신할 수 없었다. 여기서는
    // (a) 각 모드 n=3회씩 반복해 min/avg/max를 기록하고,
    // (b) 실험 31/32 스타일 "Ready&High가 영원히 존재" starvation 워크로드를
    //     CFS 모드로 직접 재현해 FAIRNESS_FLOOR_TICKS 없이도 정말 안전한지
    //     검증한다 (CFS-1의 hog완료=1tick은 간접 증거였을 뿐, 이번이 정면
    //     재현).
    serial_println!("===========================================");
    serial_println!("  CFS-2: 반복 시행(n=3) + starvation 재현 (실험 34)");
    serial_println!("===========================================");

    // quick-demo에서는 반복 시행을 1회로 줄인다. 통계(min/avg/max)의 의미는
    // 사라지지만 두 스케줄러가 동작한다는 것 자체는 그대로 보여준다.
    #[cfg(feature = "quick-demo")]
    const CFS2_TRIALS: usize = 1;
    #[cfg(not(feature = "quick-demo"))]
    const CFS2_TRIALS: usize = 3;
    let mut wp_trials: [(u64, u64, u64); CFS2_TRIALS] = [(0, 0, 0); CFS2_TRIALS];
    let mut cfs_trials: [(u64, u64, u64); CFS2_TRIALS] = [(0, 0, 0); CFS2_TRIALS];

    for i in 0..CFS2_TRIALS {
        process::scheduler::set_mode(process::scheduler::SchedMode::WeightedPriority);
        wp_trials[i] = bench_pe4::run("WP    ");
    }
    for i in 0..CFS2_TRIALS {
        process::scheduler::set_mode(process::scheduler::SchedMode::Cfs);
        cfs_trials[i] = bench_pe4::run("CFS   ");
    }
    process::scheduler::set_mode(process::scheduler::SchedMode::WeightedPriority); // 이후 데모 원복 (실험 45: CFS 기본값 시도 후 회귀 발견, 롤백)

    fn trial_stats(trials: &[(u64, u64, u64)]) -> (u64, u64, u64, u64, u64, u64) {
        let (mut lat_min, mut lat_max, mut lat_sum) = (u64::MAX, 0u64, 0u64);
        let (mut hog_min, mut hog_max, mut hog_sum) = (u64::MAX, 0u64, 0u64);
        for &(lat, _sw, hog) in trials {
            lat_min = lat_min.min(lat); lat_max = lat_max.max(lat); lat_sum += lat;
            hog_min = hog_min.min(hog); hog_max = hog_max.max(hog); hog_sum += hog;
        }
        let n = trials.len() as u64;
        (lat_min, lat_max, lat_sum / n, hog_min, hog_max, hog_sum / n)
    }

    let (wp_lat_min, wp_lat_max, wp_lat_avg, wp_hog_min, wp_hog_max, wp_hog_avg) = trial_stats(&wp_trials);
    let (cfs_lat_min, cfs_lat_max, cfs_lat_avg, cfs_hog_min, cfs_hog_max, cfs_hog_avg) = trial_stats(&cfs_trials);

    serial_println!("[cfs2] ══════════════════════════════════════════════");
    serial_println!("[cfs2]  CFS-2: 반복 시행 통계 (n={})", CFS2_TRIALS);
    serial_println!("[cfs2] ──────────────────────────────────────────────");
    serial_println!(
        "[cfs2]  키입력 레이턴시(cy)  WP  min/avg/max = {}/{}/{}",
        wp_lat_min, wp_lat_avg, wp_lat_max,
    );
    serial_println!(
        "[cfs2]  키입력 레이턴시(cy)  CFS min/avg/max = {}/{}/{}",
        cfs_lat_min, cfs_lat_avg, cfs_lat_max,
    );
    serial_println!(
        "[cfs2]  hog 완료(tick)       WP  min/avg/max = {}/{}/{}",
        wp_hog_min, wp_hog_avg, wp_hog_max,
    );
    serial_println!(
        "[cfs2]  hog 완료(tick)       CFS min/avg/max = {}/{}/{}",
        cfs_hog_min, cfs_hog_avg, cfs_hog_max,
    );
    serial_println!("[cfs2] ══════════════════════════════════════════════");

    // ── CFS-2b: starvation 시나리오를 CFS로 직접 재현 ────────────────────────
    // 실험 31/32와 완전히 동일한 패턴(yield_now() busy-loop sender/receiver,
    // 절대 안 죽고 항상 Ready&High) — WeightedPriority였다면 FAIRNESS_FLOOR
    // 없이는 kernel_main의 이 300틱 대기 루프가 무기한 안 끝난다(실측: 실험
    // 31에서 8000+틱, 480초+ 동안 미종료). CFS는 이론상 안전장치 없이도
    // starvation이 구조적으로 불가능해야 한다 — 이 루프가 정상 종료되는지
    // 자체가 검증.
    serial_println!("-------------------------------------------");
    serial_println!("  CFS-2b: starvation 워크로드를 CFS로 재현 (실험 31/32와 동일 패턴)");
    serial_println!("-------------------------------------------");

    process::scheduler::set_mode(process::scheduler::SchedMode::Cfs);

    let sid4 = process::scheduler::alloc_pid();
    process::scheduler::spawn(process::Process::new(sid4, "sender4", proc_sender));
    let rid4 = process::scheduler::alloc_pid();
    process::scheduler::spawn(process::Process::new(rid4, "receiver4", proc_receiver));

    let cfs2b_start_tick = interrupts::handlers::TICK.load(Ordering::Relaxed);
    while interrupts::handlers::TICK.load(Ordering::Relaxed) - cfs2b_start_tick < DEMO_WAIT_TICKS {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }

    process::scheduler::kill_pid(sid4);
    process::scheduler::kill_pid(rid4);
    process::scheduler::reap_dead(); // 실험 35: 힙 자원 회수 (일반 컨텍스트)
    process::scheduler::set_mode(process::scheduler::SchedMode::WeightedPriority); // 이후 데모 원복 (실험 45: CFS 기본값 시도 후 회귀 발견, 롤백)

    serial_println!("[cfs2b] CFS 모드에서 starvation 워크로드 완료 — 300틱 대기 루프 정상 종료 (FAIRNESS_FLOOR 없이도 kernel_main이 굶지 않음)");
    serial_println!("--- CFS-2 complete ---\n");

    // ── BETA-X-2 1: Event Tracer dump ────────────────────────────────────────
    tracer::dump();
    serial_println!("--- BETA-X A-2 complete ---\n");

    // ── ALPHA 13 → 14 연속 데모 ──────────────────────────────────────────────
    //
    // ALPHA 13: raw 머신 코드를 ring3에서 실행 → syscall table 검증
    // ALPHA 14: 실제 ELF64 바이너리를 ring3에서 실행 → ELF 로더 검증
    //
    // 두 데모는 sys_exit longjmp → after_user_demo → enter_elf 체인으로 연결됨.
    serial_println!("===========================================");
    serial_println!("  ALPHA 13: Linux Compat Tier 1");
    serial_println!("  (write / getpid / mmap / exit via int 0x80)");
    serial_println!("  → after_user_demo → ALPHA 14 ELF Loader");
    serial_println!("===========================================");

    // ── ring3 머신 코드 레이아웃 ─────────────────────────────────────────────
    //
    // [0x00] EB 12         jmp short → [0x14]  (skip 18-byte message)
    // [0x02..0x13]         "Hello from ring3!\n"  (18 bytes)
    // [0x14] code:
    //   B8 01 00 00 00     mov eax, 1       (SYS_write)
    //   BF 01 00 00 00     mov edi, 1       (fd = stdout)
    //   48 8D 35 DD FF FF FF  lea rsi, [rip-0x23]  → [0x02] = 메시지
    //     (RIP 계산: 인스트럭션 뒤 RIP = 0x10000+0x25, 메시지 = 0x10000+0x02
    //      disp32 = 0x02 - 0x25 = -0x23 = 0xFFFFFFDD ✓)
    //   BA 12 00 00 00     mov edx, 18      (length)
    //   CD 80              int 0x80         → sys_write → "Hello from ring3!\n"
    //
    //   B8 27 00 00 00     mov eax, 39      (SYS_getpid)
    //   CD 80              int 0x80         → RAX = current PID
    //
    //   B8 09 00 00 00     mov eax, 9       (SYS_mmap)
    //   BF 00 00 00 00     mov edi, 0       (addr hint = NULL)
    //   BE 00 10 00 00     mov esi, 0x1000  (len = 4096)
    //   BA 03 00 00 00     mov edx, 3       (prot = PROT_READ|PROT_WRITE)
    //   41 BA 22 00 00 00  mov r10d, 0x22   (flags = MAP_PRIVATE|MAP_ANONYMOUS)
    //   45 31 C0           xor r8d, r8d     (fd = 0)  ← need -1: use 4D31C0+dec
    //   49 83 C8 FF        or r8, -1        (fd = -1)
    //   4D 31 C9           xor r9, r9       (off = 0)
    //   CD 80              int 0x80         → RAX = mapped address
    //
    //   B8 3C 00 00 00     mov eax, 60      (SYS_exit)
    //   BF 00 00 00 00     mov edi, 0       (exit code = 0)
    //   CD 80              int 0x80         → longjmp → after_user_demo

    let user_code: &[u8] = &[
        // [0x00] jmp to [0x14]
        0xEB, 0x12,
        // [0x02] "Hello from ring3!\n" (18 bytes)
        b'H', b'e', b'l', b'l', b'o', b' ',
        b'f', b'r', b'o', b'm', b' ',
        b'r', b'i', b'n', b'g', b'3', b'!', b'\n',
        // [0x14] --- code ---
        // sys_write(1, &msg, 18)
        0xB8, 0x01, 0x00, 0x00, 0x00,        // mov eax, 1
        0xBF, 0x01, 0x00, 0x00, 0x00,        // mov edi, 1
        0x48, 0x8D, 0x35, 0xDD, 0xFF, 0xFF, 0xFF, // lea rsi, [rip-0x23]
        0xBA, 0x12, 0x00, 0x00, 0x00,        // mov edx, 18
        0xCD, 0x80,                           // int 0x80
        // sys_getpid()
        0xB8, 0x27, 0x00, 0x00, 0x00,        // mov eax, 39
        0xCD, 0x80,                           // int 0x80  (RAX = pid)
        // sys_mmap(0, 0x1000, PROT_RW, MAP_ANON|MAP_PRIVATE, -1, 0)
        0xB8, 0x09, 0x00, 0x00, 0x00,        // mov eax, 9
        0xBF, 0x00, 0x00, 0x00, 0x00,        // mov edi, 0
        0xBE, 0x00, 0x10, 0x00, 0x00,        // mov esi, 0x1000
        0xBA, 0x03, 0x00, 0x00, 0x00,        // mov edx, 3
        0x41, 0xBA, 0x22, 0x00, 0x00, 0x00,  // mov r10d, 0x22
        0x49, 0x83, 0xC8, 0xFF,              // or r8, -1  (fd = -1)
        0x4D, 0x31, 0xC9,                    // xor r9, r9
        0xCD, 0x80,                           // int 0x80  (RAX = mapped ptr)
        // sys_exit(0)
        0xB8, 0x3C, 0x00, 0x00, 0x00,        // mov eax, 60
        0xBF, 0x00, 0x00, 0x00, 0x00,        // mov edi, 0
        0xCD, 0x80,                           // int 0x80  (noreturn)
    ];

    serial_println!("[alpha13] entering ring3 ({} bytes of user code)", user_code.len());
    unsafe { paging::enter_user_demo(user_code); }
}

// ==================== 패닉 핸들러 ====================

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial::init();
    serial_println!("\n!!! KERNEL PANIC !!!");
    serial_println!("{}", info);
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack, preserves_flags)); }
    }
}

#[alloc_error_handler]
fn alloc_error(layout: core::alloc::Layout) -> ! {
    panic!("OOM: size={} align={}", layout.size(), layout.align());
}
