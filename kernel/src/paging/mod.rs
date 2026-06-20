//! 페이징 서브시스템 (Paging Subsystem)
//!
//! ## x86_64 4단계 페이지 테이블 구조
//!
//! 가상 주소 48비트 분해:
//! ```
//! [63:48] 부호 확장 (반드시 비트47의 복사본)
//! [47:39] PML4 인덱스 (9비트, 0-511)
//! [38:30] PDPT  인덱스 (9비트, 0-511)
//! [29:21] PD    인덱스 (9비트, 0-511)
//! [20:12] PT    인덱스 (9비트, 0-511)
//! [11:0]  페이지 내 오프셋 (12비트 = 4KB)
//! ```
//!
//! 각 테이블: 512 × 8바이트 = 4096바이트 (= 1 프레임)
//! PML4 → PDPT → PD → PT → 4KB 물리 프레임
//!
//! ## 주소 공간 분할
//!
//! ```
//! 0x0000_0000_0000_0000  ← 유저 공간 시작
//! ...
//! 0x0000_7FFF_FFFF_FFFF  ← 유저 공간 끝 (PML4[0..255])
//! [canonical hole]
//! 0xFFFF_8000_0000_0000  ← 커널 공간 시작 (PML4[256..511])
//! ...                       HHDM, 커널 코드/데이터
//! 0xFFFF_FFFF_FFFF_FFFF
//! ```
//!
//! ## Milestone 3.6 구현 범위
//!
//! - 커널 PML4: limine PML4의 상위 절반(인덱스 256-511) 복사 후 CR3 전환
//! - 유저 PML4: 커널 절반 공유 + 유저 코드/스택 매핑
//! - ring3 진입: IRETQ로 CPL 0 → CPL 3 전환
//! - Syscall:    `int 0x80` → ring3 → ring0 → IRETQ ring3 복귀
//! - Longjmp:    3회 syscall 후 커널 메인으로 복귀

use core::arch::asm;
use core::sync::atomic::{AtomicU32, Ordering};
use crate::memory::{frame, hhdm_offset};
use crate::interrupts::gdt;

// ── 페이지 테이블 엔트리 플래그 ──────────────────────────────────────────────
const PTE_PRESENT:  u64 = 1 << 0; // 페이지가 물리 메모리에 존재
const PTE_WRITABLE: u64 = 1 << 1; // 쓰기 허용
const PTE_USER:     u64 = 1 << 2; // ring3 접근 허용 (U/S 비트)
// bit 7 = PS(Page Size): PD에서 1이면 2MB 거대 페이지 (현재 미사용)

// ── 유저 프로세스 가상 주소 레이아웃 ─────────────────────────────────────────
/// 유저 코드가 매핑되는 가상 주소 (64KB)
const USER_CODE_VADDR: u64 = 0x0000_0000_1_0000;
/// 유저 스택 최상단 주소 (1MB, 스택은 아래 방향으로 성장)
const USER_STACK_TOP:  u64 = 0x0000_0000_10_0000;
/// 유저 스택에 할당할 4KB 프레임 수 (= 16KB 스택)
const USER_STACK_PAGES: usize = 4;

// ── 전역 상태 ─────────────────────────────────────────────────────────────
/// 커널 전용 페이지 테이블의 CR3 값 (유저 모드 복귀 시 CR3 복원에 사용)
pub static mut KERNEL_CR3: u64 = 0;

/// enter_user_demo() 진입 직전에 저장한 커널 메인 스택 포인터
///
/// syscall 핸들러가 3회 호출 후 이 값으로 RSP를 복원 (longjmp).
pub static mut KERNEL_MAIN_RSP: u64 = 0;

/// ring3 → ring0 인터럽트/예외 진입 시 CPU가 사용할 전용 스택
///
/// CPU는 ring3에서 인터럽트 발생 시 TSS.RSP0에 설정된 주소로 스택을 전환함.
/// 이 버퍼의 최상단(+8192)이 TSS.RSP0에 설정됨.
static mut USER_KERNEL_STACK: [u8; 8192] = [0; 8192];

// ── 페이지 테이블 타입 ────────────────────────────────────────────────────
/// x86_64 페이지 테이블 (PML4 / PDPT / PD / PT 모두 동일 구조)
///
/// `#[repr(C, align(4096))]`: 4KB 경계 정렬 필수 (CR3 로드 요구사항).
/// 할당 후 반드시 0으로 초기화해야 함 (비어있음 = Not Present = 0).
#[repr(C, align(4096))]
struct PageTable([u64; 512]);

/// 물리 프레임을 할당하고 0으로 초기화한 PageTable 포인터 반환
///
/// 반환값: 가상 주소(HHDM) 기반 포인터 (물리 주소 + HHDM 오프셋)
fn alloc_table() -> *mut PageTable {
    let phys = frame::alloc_frame().expect("OOM: page table frame");
    let virt = phys + hhdm_offset();
    unsafe {
        // 새 페이지 테이블은 반드시 0으로 채워야 함
        // (0 = Present 비트 클리어 = 해당 엔트리 미사용)
        core::ptr::write_bytes(virt as *mut u8, 0, 4096);
        virt as *mut PageTable
    }
}

/// 물리 주소 → 가상 주소(HHDM) 기반 PageTable 가변 참조
unsafe fn table_at(phys: u64) -> &'static mut PageTable {
    &mut *((phys + hhdm_offset()) as *mut PageTable)
}

/// 가상 주소를 4KB 물리 페이지에 매핑
///
/// PML4 → PDPT → PD → PT 계층을 순서대로 탐색.
/// 중간 테이블이 없으면 새 프레임을 할당해 생성.
///
/// # 인수
/// - `pml4`: 루트 페이지 테이블 (가상 주소 포인터)
/// - `vaddr`: 매핑할 가상 주소 (4KB 정렬이어야 함)
/// - `paddr`: 매핑할 물리 주소 (4KB 정렬이어야 함)
/// - `flags`: `PTE_WRITABLE`, `PTE_USER` 등 (PTE_PRESENT 자동 추가)
pub unsafe fn map_4k(pml4: *mut PageTable, vaddr: u64, paddr: u64, flags: u64) {
    // 가상 주소를 각 레벨의 9비트 인덱스로 분해
    let i4 = ((vaddr >> 39) & 0x1FF) as usize; // PML4 인덱스
    let i3 = ((vaddr >> 30) & 0x1FF) as usize; // PDPT 인덱스
    let i2 = ((vaddr >> 21) & 0x1FF) as usize; // PD   인덱스
    let i1 = ((vaddr >> 12) & 0x1FF) as usize; // PT   인덱스

    // 중간 테이블 엔트리에는 PTE_USER를 전파해야 함.
    // U/S 비트는 계층 전체에 AND로 적용되기 때문에
    // 상위 테이블 엔트리에 U/S=0이 있으면 하위 레벨이 U/S=1이어도 supervisor만 접근.
    let mid_flags = PTE_PRESENT | PTE_WRITABLE | PTE_USER;

    let pdpt = get_or_create(&mut (*pml4).0[i4], mid_flags);
    let pd   = get_or_create(&mut (*pdpt).0[i3], mid_flags);
    let pt   = get_or_create(&mut (*pd).0[i2],   mid_flags);

    // 최종 4KB 페이지 매핑 (PTE_PRESENT 자동 추가)
    (*pt).0[i1] = (paddr & !0xFFF) | flags | PTE_PRESENT;
}

/// 페이지 테이블 엔트리가 없으면 새 테이블 할당 후 엔트리에 기록
unsafe fn get_or_create(entry: &mut u64, flags: u64) -> *mut PageTable {
    if *entry & PTE_PRESENT == 0 {
        let new_tbl = alloc_table();
        // 물리 주소 = 가상 주소 - HHDM 오프셋
        let phys = new_tbl as u64 - hhdm_offset();
        *entry = phys | flags;
    }
    // 엔트리 상위 52비트 = 물리 주소, 하위 12비트 = 플래그
    table_at(*entry & !0xFFF)
}

// ── 초기화 ───────────────────────────────────────────────────────────────────

/// 페이징 서브시스템 초기화
///
/// ## 동작
/// 1. 현재 CR3(limine의 PML4)에서 커널 절반(인덱스 256-511) 복사
/// 2. 새 커널 PML4로 CR3 전환
/// 3. TSS.RSP0 = USER_KERNEL_STACK 최상단
///    (ring3 → ring0 전환 시 CPU가 이 주소로 스택을 자동 교체)
///
/// ## 왜 새 PML4를 만드는가?
/// limine의 PML4를 그대로 쓰면 우리가 유저 공간(인덱스 0-255)을
/// 제어할 수 없음. 새 PML4를 만들어야 프로세스별 유저 매핑을 독립적으로 관리 가능.
pub fn init() {
    unsafe {
        // 현재 CR3 읽기 (limine이 설정한 PML4의 물리 주소)
        let old_cr3: u64;
        asm!("mov {}, cr3", out(reg) old_cr3,
            options(nomem, nostack, preserves_flags));

        // 새 커널 PML4 할당 (유저 절반 0-255는 0으로 비워둠)
        let new_pml4 = alloc_table();
        let new_cr3  = new_pml4 as u64 - hhdm_offset();

        // limine PML4의 커널 절반(256-511)을 새 PML4에 복사.
        // 이 범위에 HHDM, 커널 코드/데이터, 스택 매핑이 있음.
        let old_pml4 = table_at(old_cr3 & !0xFFF);
        for i in 256..512usize {
            (*new_pml4).0[i] = old_pml4.0[i];
        }

        // CR3 전환 (= TLB 전체 플러시)
        asm!("mov cr3, {}", in(reg) new_cr3,
            options(nomem, nostack, preserves_flags));

        KERNEL_CR3 = new_cr3;

        // TSS.RSP0 설정: ring3에서 인터럽트 발생 시 CPU가 사용할 스택
        // 스택은 높은 주소에서 낮은 방향으로 성장하므로 버퍼 최상단 설정
        let rsp0 = USER_KERNEL_STACK.as_ptr().add(8192) as u64;
        gdt::set_tss_rsp0(rsp0);
    }

    crate::serial_println!("[paging] Kernel PML4 built and CR3 switched.");
}

// ── ring3 진입 ───────────────────────────────────────────────────────────────

/// 유저 프로세스 주소 공간 생성 후 ring3으로 진입 (반환하지 않음)
///
/// ## 동작 순서
/// 1. 유저 PML4 생성 (커널 절반 공유)
/// 2. 유저 코드 프레임 할당 → `code` 바이트 복사 → `USER_CODE_VADDR`에 매핑
/// 3. 유저 스택 프레임 할당 → `USER_STACK_TOP` 아래에 매핑
/// 4. 커널 메인 RSP 저장 (longjmp 복귀 지점)
/// 5. CR3 → 유저 PML4, IRETQ → ring3
///
/// ## 유저 코드 규약
/// - 진입점: `USER_CODE_VADDR` (코드 바이트의 오프셋 0)
/// - 스택: `USER_STACK_TOP`에서 시작, 아래로 성장
/// - Syscall: `int 0x80` 사용
///
/// ## 반환
/// 이 함수는 `options(noreturn)` asm으로 끝나며 절대 반환하지 않음.
/// `syscall_handler()`가 3번째 호출 시 longjmp로 `after_user_demo()`에 도달.
/// ELF64 바이너리를 유저 주소 공간에 로드한 뒤 ring3으로 진입 (ALPHA 14)
///
/// ## 동작 순서
/// 1. 유저 PML4 생성 (커널 절반 공유)
/// 2. ELF PT_LOAD 세그먼트별로 물리 프레임 할당 → 파일 내용 복사 → 가상 주소 매핑
/// 3. 유저 스택 할당 (USER_STACK_TOP 아래)
/// 4. KERNEL_MAIN_RSP 저장 (longjmp 복귀 지점)
/// 5. IRETQ → ring3, 진입점 = ELF e_entry
pub unsafe fn enter_elf(elf_data: &[u8]) -> ! {
    use crate::elf::Elf64;

    let elf = Elf64::parse(elf_data).expect("[elf] invalid ELF64 binary");
    let entry = elf.entry();

    crate::serial_println!("[elf] ELF64 parsed: entry={:#x}", entry);

    // ── 유저 PML4 ────────────────────────────────────────────────────────
    let user_pml4 = alloc_table();
    let kpml4 = table_at(KERNEL_CR3 & !0xFFF);
    for i in 256..512usize {
        (*user_pml4).0[i] = kpml4.0[i];
    }
    let user_cr3 = user_pml4 as u64 - hhdm_offset();

    // ── PT_LOAD 세그먼트 매핑 ────────────────────────────────────────────
    for seg in elf.load_segments() {
        crate::serial_println!(
            "[elf] LOAD vaddr={:#x}  filesz={:#x}  memsz={:#x}  flags={:#x}",
            seg.vaddr, seg.filesz, seg.memsz, seg.flags
        );

        // 세그먼트가 차지할 페이지 범위 (4KB 정렬)
        let page_start = seg.vaddr & !0xFFF;
        let page_end   = (seg.vaddr + seg.memsz as u64 + 0xFFF) & !0xFFF;

        // 세그먼트 플래그 PF_W(2) 있으면 쓰기 허용
        let pte_flags = PTE_USER | PTE_WRITABLE;

        let mut vpage = page_start;
        while vpage < page_end {
            let phys = frame::alloc_frame().expect("OOM: ELF segment page");
            let frame_virt = (phys + hhdm_offset()) as *mut u8;
            // 페이지 전체를 0으로 초기화 (BSS 영역 처리)
            core::ptr::write_bytes(frame_virt, 0, 4096);

            // 이 페이지에 해당하는 파일 데이터 복사
            // page_off_in_seg: 세그먼트 시작(vaddr)에서 현재 페이지의 상대 오프셋
            let page_off_in_seg = (vpage.saturating_sub(page_start)) as usize;
            let file_src_start  = seg.offset + page_off_in_seg;
            let file_src_end    = file_src_start + 4096;
            let copy_end        = file_src_end.min(seg.offset + seg.filesz).min(elf_data.len());

            if file_src_start < copy_end {
                let n = copy_end - file_src_start;
                // 세그먼트 vaddr이 페이지 경계와 다를 경우 페이지 내 오프셋 조정
                let dst_off = if vpage < seg.vaddr {
                    (seg.vaddr - vpage) as usize
                } else {
                    0
                };
                core::ptr::copy_nonoverlapping(
                    elf_data.as_ptr().add(file_src_start),
                    frame_virt.add(dst_off),
                    n,
                );
            }

            map_4k(user_pml4, vpage, phys, pte_flags);
            vpage += 4096;
        }
    }

    // ── 유저 스택 ─────────────────────────────────────────────────────────
    for i in 0..USER_STACK_PAGES {
        let phys  = frame::alloc_frame().expect("OOM: ELF stack page");
        let vaddr = USER_STACK_TOP - ((i + 1) as u64) * 4096;
        map_4k(user_pml4, vaddr, phys, PTE_WRITABLE | PTE_USER);
    }

    crate::serial_println!("[elf] address space ready. entering ring3 at {:#x}...", entry);

    // ── KERNEL_MAIN_RSP 저장 ──────────────────────────────────────────────
    asm!(
        "mov [{save}], rsp",
        save = in(reg) &raw mut KERNEL_MAIN_RSP,
        options(nostack, preserves_flags),
    );

    // ── IRETQ: ring0 → ring3 ─────────────────────────────────────────────
    asm!(
        "mov cr3, {user_cr3}",
        "push {ss}",
        "push {user_sp}",
        "push {flags}",
        "push {cs}",
        "push {user_ip}",
        "iretq",
        user_cr3 = in(reg) user_cr3,
        ss       = in(reg) gdt::USER_SS_RPL3 as u64,
        user_sp  = in(reg) USER_STACK_TOP,
        flags    = in(reg) 0x202u64,
        cs       = in(reg) gdt::USER_CS_RPL3 as u64,
        user_ip  = in(reg) entry,
        options(noreturn),
    );
}

pub unsafe fn enter_user_demo(code: &[u8]) -> ! {
    assert!(!code.is_empty() && code.len() <= 4096, "user code must fit in one page");

    // ── 유저 PML4 생성 ───────────────────────────────────────────────────
    let user_pml4 = alloc_table();

    // 커널 절반 공유: ring3에서 발생한 인터럽트/예외 핸들러가
    // 커널 코드와 스택에 접근할 수 있어야 하므로 상위 절반 엔트리를 복사.
    let kpml4 = table_at(KERNEL_CR3 & !0xFFF);
    for i in 256..512usize {
        (*user_pml4).0[i] = kpml4.0[i];
    }

    let user_cr3 = user_pml4 as u64 - hhdm_offset();

    // ── 유저 코드 매핑 ───────────────────────────────────────────────────
    // 코드 페이지: USER(ring3 접근) + PRESENT, WRITABLE 없음(읽기/실행 전용)
    // x86_64에서 기본 실행 허용(NX 비트 미사용).
    let code_phys = frame::alloc_frame().expect("OOM: user code frame");
    let code_virt = (code_phys + hhdm_offset()) as *mut u8;
    core::ptr::copy_nonoverlapping(code.as_ptr(), code_virt, code.len());
    map_4k(user_pml4, USER_CODE_VADDR, code_phys, PTE_USER);
    // WRITABLE 없음 → 코드 영역은 읽기/실행 전용

    // ── 유저 스택 매핑 ───────────────────────────────────────────────────
    // 스택 페이지: USER + WRITABLE + PRESENT
    // 스택은 USER_STACK_TOP 바로 아래 프레임부터 시작.
    for i in 0..USER_STACK_PAGES {
        let phys  = frame::alloc_frame().expect("OOM: user stack frame");
        let vaddr = USER_STACK_TOP - ((i + 1) as u64) * 4096;
        map_4k(user_pml4, vaddr, phys, PTE_WRITABLE | PTE_USER);
    }

    crate::serial_println!("[paging] User address space:");
    crate::serial_println!("  code  → virt={:#x}  ({} bytes)", USER_CODE_VADDR, code.len());
    crate::serial_println!("  stack → virt={:#x}..{:#x}  ({} pages)",
        USER_STACK_TOP - (USER_STACK_PAGES as u64) * 4096,
        USER_STACK_TOP, USER_STACK_PAGES);

    // ── 커널 메인 RSP 저장 ───────────────────────────────────────────────
    // syscall_handler()가 3회 호출 후 이 RSP로 복원 후 after_user_demo()로 점프.
    // 저장 시점: 아래 IRETQ 프레임을 push하기 전이므로
    //            복원 시 RSP는 IRETQ 프레임 이전 상태 → 유효한 커널 스택.
    asm!(
        "mov [{save}], rsp",
        save = in(reg) &raw mut KERNEL_MAIN_RSP,
        options(nostack, preserves_flags),
    );

    crate::serial_println!("[paging] Entering ring3 via IRETQ...");
    crate::serial_println!("         code at virt={:#x}, stack top={:#x}",
        USER_CODE_VADDR, USER_STACK_TOP);

    // ── IRETQ: ring0 → ring3 ─────────────────────────────────────────────
    //
    // ring0에서 ring3으로 전환하려면 IRETQ 명령어 사용.
    // 스택에 다음 순서로 push (IRETQ가 반대 순서로 pop):
    //
    //   [낮은주소] RIP     ← IRETQ 후 ring3 코드가 시작할 주소
    //              CS      ← ring3 코드 세그먼트 셀렉터 (RPL=3 필수)
    //              RFLAGS  ← 새 RFLAGS (IF=1로 인터럽트 허용)
    //              RSP     ← ring3 스택 포인터
    //   [높은주소] SS      ← ring3 스택 세그먼트 셀렉터 (RPL=3 필수)
    //
    // CS/SS의 RPL(Request Privilege Level)=3 필수:
    //   RPL이 최종 CPL(Current Privilege Level)을 결정함.
    //   RPL < 3이면 ring3로 전환되지 않음.
    asm!(
        "mov cr3, {user_cr3}",      // 유저 페이지 테이블로 전환
        "push {ss}",                // SS  = 유저 데이터 세그먼트 (RPL=3)
        "push {user_sp}",           // RSP = 유저 스택 포인터
        "push {flags}",             // RFLAGS = IF=1 (인터럽트 허용)
        "push {cs}",                // CS  = 유저 코드 세그먼트 (RPL=3)
        "push {user_ip}",           // RIP = 유저 코드 진입점
        "iretq",                    // ring3 전환 (CPL: 0 → 3)
        user_cr3 = in(reg) user_cr3,
        ss       = in(reg) gdt::USER_SS_RPL3 as u64,
        user_sp  = in(reg) USER_STACK_TOP,
        flags    = in(reg) 0x202u64,  // RFLAGS: IF=1 + 예약비트(bit1)
        cs       = in(reg) gdt::USER_CS_RPL3 as u64,
        user_ip  = in(reg) USER_CODE_VADDR,
        options(noreturn),
        // noreturn: IRETQ 이후 ring3 실행, 이 asm 블록으로 돌아오지 않음
        // 복귀는 syscall_handler의 longjmp가 after_user_demo()로 점프
    );
}
