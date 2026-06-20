//! 커널 진입점 (Kernel Entry Point)
//!
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

extern crate alloc;

mod elf;
mod fb;
mod interrupts;
mod kbd;
mod mouse;
mod memory;
mod pkg;
mod net;
mod paging;
mod pci;
mod policy;
mod process;
mod serial;
mod syscall;
mod vfs;
mod virtio;
mod wm;

use core::sync::atomic::Ordering;
use limine::request::{BootloaderInfoRequest, FramebufferRequest, HhdmRequest, MemmapRequest};
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
pub static ELF_TEST: &[u8] = &[
    // ── ELF64 헤더 (64 bytes) ────────────────────────────────────────────
    // e_ident
    0x7F, b'E', b'L', b'F',     // magic
    2,                            // EI_CLASS  = ELFCLASS64
    1,                            // EI_DATA   = little-endian
    1,                            // EI_VERSION
    0,                            // EI_OSABI  = System V
    0, 0, 0, 0, 0, 0, 0, 0,     // padding
    // e_type = ET_EXEC (2)
    2, 0,
    // e_machine = EM_X86_64 (0x3E = 62)
    0x3E, 0,
    // e_version = 1
    1, 0, 0, 0,
    // e_entry = 0x400078
    0x78, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    // e_phoff = 0x40 (program header table at offset 64)
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // e_shoff = 0 (no section headers)
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // e_flags = 0
    0x00, 0x00, 0x00, 0x00,
    // e_ehsize = 64
    0x40, 0x00,
    // e_phentsize = 56
    0x38, 0x00,
    // e_phnum = 1
    0x01, 0x00,
    // e_shentsize = 64
    0x40, 0x00,
    // e_shnum = 0
    0x00, 0x00,
    // e_shstrndx = 0
    0x00, 0x00,

    // ── PT_LOAD 프로그램 헤더 (56 bytes, offset 0x40) ────────────────────
    // p_type = PT_LOAD (1)
    0x01, 0x00, 0x00, 0x00,
    // p_flags = PF_R|PF_X (5)
    0x05, 0x00, 0x00, 0x00,
    // p_offset = 0 (파일 처음부터 매핑)
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // p_vaddr = 0x400000
    0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    // p_paddr = 0x400000
    0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    // p_filesz = 181 = 0xB5
    0xB5, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // p_memsz = 0x1000 (1 페이지, BSS 포함)
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // p_align = 0x1000
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,

    // ── 코드 + 데이터 (61 bytes, file offset 0x78 = vaddr 0x400078) ──────
    // [0x400078] jmp short +16  → [0x40008A]
    0xEB, 0x10,
    // [0x40007A] "Hello from ELF!\n"  (16 bytes)
    b'H', b'e', b'l', b'l', b'o', b' ', b'f', b'r',
    b'o', b'm', b' ', b'E', b'L', b'F', b'!', b'\n',
    // [0x40008A] sys_write(1, 0x40007A, 16)
    0xB8, 0x01, 0x00, 0x00, 0x00,              // mov eax, 1
    0xBF, 0x01, 0x00, 0x00, 0x00,              // mov edi, 1
    0x48, 0x8D, 0x35, 0xDF, 0xFF, 0xFF, 0xFF,  // lea rsi, [rip-0x21]
    0xBA, 0x10, 0x00, 0x00, 0x00,              // mov edx, 16
    0xCD, 0x80,                                 // int 0x80
    // [0x4000A2] sys_getpid()
    0xB8, 0x27, 0x00, 0x00, 0x00,              // mov eax, 39
    0xCD, 0x80,                                 // int 0x80
    // [0x4000A9] sys_exit(0)
    0xB8, 0x3C, 0x00, 0x00, 0x00,              // mov eax, 60
    0xBF, 0x00, 0x00, 0x00, 0x00,              // mov edi, 0
    0xCD, 0x80,                                 // int 0x80
];

// ==================== ALPHA 15: 내장 MuShell ELF ====================
//
// user/mushell/ 크레이트가 build/mushell.elf를 생성.
// (Makefile의 mushell 타겟이 먼저 빌드)
// handlers.rs의 after_user_demo Phase 1이 이 바이너리를 ring3에서 실행.
pub static MUSHELL_ELF: &[u8] = include_bytes!(
    concat!(env!("CARGO_MANIFEST_DIR"), "/../build/mushell.elf")
);

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
    if let Some(info) = BOOTLOADER_INFO.response() {
        serial_println!("[boot] {} v{}", info.name(), info.version());
    }

    // ── 2. 메모리 ────────────────────────────────────────────────────────
    let hhdm_offset = HHDM.response().expect("HHDM response missing").offset;
    let memmap = MEMMAP.response().expect("memmap response missing").entries();
    memory::init(hhdm_offset, memmap);

    // ── 3. 인터럽트 (GDT→IDT→PIC→STI) ─────────────────────────────────
    interrupts::init();

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
    serial_println!("--- IPC Demo complete ---\n");

    // ── 7. Zero-copy Capability IPC 데모 (ALPHA M2) ──────────────────────
    serial_println!("===========================================");
    serial_println!("  ALPHA M2: Zero-copy Capability IPC");
    serial_println!("===========================================");

    // 송신자: 공유 버퍼 할당 → 데이터 기록 → CapId 전송
    {
        use alloc::vec::Vec;
        let sender_pid: process::Pid = 0; // kernel_main이 sender 역할
        let receiver_pid: process::Pid = 99; // 가상 수신자 (데모용)

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
    serial_println!("[sched] task_a / task_b killed. Continuing...\n");

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

    // ── ALPHA 12: TCP/IP 네트워크 스택 ───────────────────────────────────────
    serial_println!("===========================================");
    serial_println!("  ALPHA 12: TCP/IP Network Stack");
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
