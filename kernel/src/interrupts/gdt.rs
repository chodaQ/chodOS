//! GDT (Global Descriptor Table) + TSS 설정
//!
//! ## GDT 레이아웃 (Milestone 3.6 기준)
//!
//! ```
//! [0x00] null             — ABI 요구 (첫 엔트리 항상 null)
//! [0x08] kernel code      — CS: L=1(64비트), DPL=0, execute/read
//! [0x10] kernel data      — DS/SS: DPL=0, read/write
//! [0x18] TSS low          — 16바이트 시스템 디스크립터 (두 슬롯)
//! [0x20] TSS high         ┘
//! [0x28] user code        — CS: L=1(64비트), DPL=3, execute/read
//! [0x30] user data        — DS/SS: DPL=3, read/write
//! ```
//!
//! ## 셀렉터 RPL(Request Privilege Level)
//!
//! IRETQ로 ring3로 진입할 때 CS/SS 셀렉터에 RPL=3을 OR해야 함.
//! CPU가 IRETQ 시 셀렉터의 RPL을 새 CPL(Current Privilege Level)로 설정.
//!
//! 예: user code selector = 0x28, RPL=3 → 0x2B (push할 때 0x2B 사용)

use core::{arch::asm, mem};

// ── 셀렉터 상수 ────────────────────────────────────────────────────────────
pub const KERNEL_CODE_SEL:      u16 = 0x08;
pub const KERNEL_DATA_SEL:      u16 = 0x10;
pub const TSS_SEL:              u16 = 0x18;
/// ring3 코드 세그먼트 (RPL=3 OR 전 베이스값)
pub const USER_CODE_SEL:        u16 = 0x28;
/// ring3 데이터 세그먼트 (RPL=3 OR 전 베이스값)
pub const USER_DATA_SEL:        u16 = 0x30;
/// IRETQ 프레임에 넣을 user CS (RPL=3 세트)
pub const USER_CS_RPL3:         u16 = USER_CODE_SEL | 3; // 0x2B
/// IRETQ 프레임에 넣을 user SS (RPL=3 세트)
pub const USER_SS_RPL3:         u16 = USER_DATA_SEL | 3; // 0x33

// ── TSS ────────────────────────────────────────────────────────────────────
//
// x86_64 TSS는 104바이트 고정 크기.
// 우리가 사용하는 필드:
//   rsp[0] = ring3→ring0 전환 시 CPU가 로드할 커널 스택 포인터
//            (int/exception이 ring3에서 발생하면 CPU가 자동으로 이 값을 RSP에 씀)
#[repr(C, packed)]
pub struct Tss {
    _reserved0: u32,
    pub rsp:        [u64; 3],   // RSP0-RSP2: 특권 레벨 전환용 스택
    _reserved1: u64,
    pub ist:        [u64; 7],   // IST1-IST7: 인터럽트 전용 스택 (현재 미사용)
    _reserved2: u64,
    _reserved3: u16,
    pub iomap_base: u16,         // IOPB 오프셋: sizeof(Tss) → IOPB 없음
}

// ── GDT 테이블 ─────────────────────────────────────────────────────────────
//
// ## 세그먼트 디스크립터 비트 레이아웃 (8바이트)
//
// access byte(bits 47:40):
//   bit 47 (P)   = 세그먼트 유효 여부 (1 = present)
//   bit 46:45(DPL)= 특권 레벨 (00=ring0, 11=ring3)
//   bit 44 (S)   = 0=시스템, 1=코드/데이터
//   bit 43:40(Type)= 세그먼트 타입
//     1010 = 코드, 실행/읽기
//     0010 = 데이터, 읽기/쓰기
//
// flags nibble(bits 55:52): G | D/B | L | AVL
//   L=1, D=0 → 64비트 코드 세그먼트
//   G=1       → 리밋을 4KB 단위로 (64비트 모드에서는 무시)
//
// 커널 코드 (0x9A access, 0xA flags):
//   access=0x9A: P=1, DPL=0, S=1, Type=0xA(코드 실행/읽기)
//   flags=0xA:   G=1, D=0, L=1(64비트 코드)
//   → 0x00af_9a00_0000_ffff
//
// 유저 코드 (0xFA access, 0xA flags):
//   access=0xFA: P=1, DPL=3, S=1, Type=0xA
//   → 0x00af_fa00_0000_ffff
//
// 커널/유저 데이터 (0x92/0xF2 access):
//   access=0x92: P=1, DPL=0, S=1, Type=0x2(데이터 읽기/쓰기)
//   access=0xF2: P=1, DPL=3, S=1, Type=0x2
//   64비트 모드에서 데이터 세그먼트 base/limit/flags는 대부분 무시됨

#[repr(C, align(8))]
struct Gdt([u64; 7]);

#[repr(C, packed)]
struct GdtPointer { limit: u16, base: u64 }

// ── NMI/#DF 전용 IST 스택 ────────────────────────────────────────────────
//
// NMI(벡터 2)는 x86에서 cli/IF=0로 절대 마스킹할 수 없는 유일한 인터럽트다.
// IST 없이(=현재 스택 그대로) 처리하면, 인터럽트 게이트로 IF=0을 걸어둔
// 임계 구간(예: isr32/isr64가 mov rsp,rax로 다음 프로세스 스택으로 전환한
// 직후~iretq 직전) 한복판에 NMI가 끼어들어도 그 순간의 현재 rsp에 자기
// 예외 프레임을 그대로 밀어넣는다 — 다음 프로세스의 저장된 레지스터
// 프레임(특히 iretq 직전 CS 슬롯)을 덮어쓸 수 있다는 뜻이다.
// (`#GP` 부팅 회귀 실험 36~40에서 "IF=0인데 뭔가 끼어든 것처럼 보인다"는
// 관측이 있었는데, IF로 막을 수 없는 NMI가 정확히 그 설명에 들어맞는
// 유일한 인터럽트라 실험 41에서 이 가설을 검증하기 위해 추가함.)
// #DF(벡터 8, 더블 폴트)도 관례상 전용 스택을 준다 — 더블 폴트가 난
// 시점엔 원래 스택 상태를 신뢰할 수 없기 때문(Linux/seL4 등 표준 관행).
const NMI_DF_IST_STACK_SIZE: usize = 8192;
static mut NMI_IST_STACK: [u8; NMI_DF_IST_STACK_SIZE] = [0; NMI_DF_IST_STACK_SIZE];
static mut DF_IST_STACK:  [u8; NMI_DF_IST_STACK_SIZE] = [0; NMI_DF_IST_STACK_SIZE];

static mut TSS: Tss = Tss {
    _reserved0: 0,
    rsp:        [0; 3],
    _reserved1: 0,
    ist:        [0; 7],
    _reserved2: 0,
    _reserved3: 0,
    iomap_base: mem::size_of::<Tss>() as u16,
};

static mut GDT: Gdt = Gdt([
    0x0000_0000_0000_0000, // [0] null
    0x00af_9a00_0000_ffff, // [1] kernel code  (DPL=0, L=1) → 0x08
    0x00cf_9200_0000_ffff, // [2] kernel data  (DPL=0)      → 0x10
    0x0000_0000_0000_0000, // [3] TSS low      (runtime)    → 0x18
    0x0000_0000_0000_0000, // [4] TSS high     (runtime)    → 0x20
    0x00af_fa00_0000_ffff, // [5] user code    (DPL=3, L=1) → 0x28
    0x00cf_f200_0000_ffff, // [6] user data    (DPL=3)      → 0x30
]);

pub fn init() {
    unsafe {
        // ── TSS 디스크립터 빌드 (16바이트 시스템 디스크립터) ───────────────
        //
        // 하위 8바이트:
        //   bits 0:15  = limit[15:0]
        //   bits 16:39 = base[23:0]
        //   bits 40:47 = access (0x89 = Present, Available 64-bit TSS)
        //   bits 48:51 = limit[19:16]
        //   bits 56:63 = base[31:24]
        // 상위 8바이트:
        //   bits 0:31  = base[63:32]
        let tss_ptr  = &raw const TSS as u64;
        let tss_size = (mem::size_of::<Tss>() - 1) as u64;

        let tss_low =
              (tss_size & 0xFFFF)
            | ((tss_ptr & 0x00FF_FFFF) << 16)
            | (0x89_u64 << 40)
            | (((tss_size >> 16) & 0xF) << 48)
            | (((tss_ptr >> 24) & 0xFF) << 56);

        GDT.0[3] = tss_low;
        GDT.0[4] = tss_ptr >> 32;

        // ── IST1 = NMI 전용 스택, IST2 = #DF 전용 스택 ─────────────────────
        // idt.rs에서 벡터 2(NMI)는 ist=1, 벡터 8(#DF)은 ist=2로 등록한다.
        let nmi_stack_top = (&raw const NMI_IST_STACK as u64) + NMI_DF_IST_STACK_SIZE as u64;
        let df_stack_top  = (&raw const DF_IST_STACK  as u64) + NMI_DF_IST_STACK_SIZE as u64;
        TSS.ist[0] = nmi_stack_top; // IST1
        TSS.ist[1] = df_stack_top;  // IST2

        // ── GDTR 로드 ──────────────────────────────────────────────────────
        let gdtr = GdtPointer {
            limit: (mem::size_of::<Gdt>() - 1) as u16,
            base:  core::ptr::addr_of!(GDT.0) as u64,
        };
        asm!("lgdt [{}]", in(reg) &gdtr, options(readonly, nostack, preserves_flags));

        // ── CS 재로드 (원거리 반환 트릭) ───────────────────────────────────
        // lgdt 이후 세그먼트 디스크립터 캐시를 갱신하려면 CS를 다시 로드해야 함.
        // 64비트에서 CS는 MOV 불가 → 스택에 [새 CS, 복귀 RIP] push 후 LRETQ.
        asm!(
            "pushq {sel}",
            "leaq 1f(%rip), %rax",
            "pushq %rax",
            "lretq",
            "1:",
            sel = in(reg) KERNEL_CODE_SEL as u64,
            out("rax") _,
            options(att_syntax),
        );

        // ── 데이터 세그먼트 재로드 ─────────────────────────────────────────
        asm!(
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            in("ax") KERNEL_DATA_SEL,
            options(nostack, nomem, preserves_flags),
        );
        asm!(
            "xor eax, eax",
            "mov fs, ax",
            "mov gs, ax",
            out("eax") _,
            options(nostack, nomem, preserves_flags),
        );

        // ── TSS 로드 ───────────────────────────────────────────────────────
        asm!("ltr ax", in("ax") TSS_SEL, options(nostack, nomem));
    }

    crate::serial_println!(
        "[gdt] GDT loaded: kernel(0x{:02x}/0x{:02x}) tss(0x{:02x}) user(0x{:02x}/0x{:02x})",
        KERNEL_CODE_SEL, KERNEL_DATA_SEL, TSS_SEL, USER_CODE_SEL, USER_DATA_SEL
    );
}

/// AP 전용: LGDT + CS/DS 재로드 (TSS 없음, 출력 없음)
pub fn ap_load() {
    unsafe {
        let gdtr = GdtPointer {
            limit: (mem::size_of::<Gdt>() - 1) as u16,
            base:  core::ptr::addr_of!(GDT.0) as u64,
        };
        asm!("lgdt [{}]", in(reg) &gdtr, options(readonly, nostack, preserves_flags));
        asm!(
            "pushq {sel}",
            "leaq 1f(%rip), %rax",
            "pushq %rax",
            "lretq",
            "1:",
            sel = in(reg) KERNEL_CODE_SEL as u64,
            out("rax") _,
            options(att_syntax),
        );
        asm!(
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            in("ax") KERNEL_DATA_SEL,
            options(nostack, nomem, preserves_flags),
        );
    }
}

/// TSS.RSP0 설정 — ring3 → ring0 전환 시 CPU가 사용할 커널 스택 포인터
///
/// int/exception이 ring3에서 발생할 때 CPU는 자동으로:
/// 1. RSP ← TSS.RSP0 (이 함수로 설정한 값)
/// 2. 기존 SS/RSP/RFLAGS/CS/RIP를 새 스택에 push
/// 3. 핸들러로 점프
///
/// paging::init()에서 전용 스택(USER_KERNEL_STACK)을 할당 후 여기에 설정.
pub fn set_tss_rsp0(rsp0: u64) {
    unsafe { TSS.rsp[0] = rsp0; }
    crate::serial_println!("[gdt] TSS.RSP0 = {:#x}", rsp0);
}
