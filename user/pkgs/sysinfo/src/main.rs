//! sysinfo — MuKernel 시스템 정보 패키지 (ALPHA 16)
#![no_std]
#![no_main]

use core::arch::global_asm;

global_asm!(
    ".section .text.entry",
    ".global _start",
    "_start:",
    "xor ebp, ebp",
    "call pkg_main",
    "mov eax, 60",
    "xor edi, edi",
    "int 0x80",
);

#[inline(always)]
unsafe fn sys_write(fd: i64, buf: *const u8, len: usize) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "int 0x80",
        inlateout("rax") 1i64 => ret,
        in("rdi") fd,
        in("rsi") buf,
        in("rdx") len,
        options(nostack),
    );
    ret
}

#[inline(always)]
unsafe fn sys_getpid() -> i64 {
    let ret: i64;
    core::arch::asm!(
        "int 0x80",
        inlateout("rax") 39i64 => ret,
        options(nostack),
    );
    ret
}

fn print(s: &[u8]) {
    let mut off = 0;
    while off < s.len() {
        let n = unsafe { sys_write(1, s[off..].as_ptr(), s.len() - off) };
        if n <= 0 { break; }
        off += n as usize;
    }
}

fn print_u64(mut n: u64) {
    if n == 0 { print(b"0"); return; }
    let mut buf = [0u8; 20];
    let mut i = 20usize;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    print(&buf[i..]);
}

#[no_mangle]
pub extern "C" fn pkg_main() {
    let pid = unsafe { sys_getpid() } as u64;
    print(b"=== MuKernel System Information ===\n");
    print(b"OS:      MuKernel 0.1.0-alpha\n");
    print(b"Arch:    x86_64 (bare metal, UEFI)\n");
    print(b"Kernel:  Preemptive + EMA Policy Engine\n");
    print(b"Shell:   MuShell v0.1 (ALPHA-15)\n");
    print(b"PkgMgr:  mukg v0.1.0 (ALPHA-16)\n");
    print(b"PID:     ");
    print_u64(pid);
    print(b"\n");
    print(b"Memory:  256 MB (QEMU)\n");
    print(b"Boot:    Limine UEFI v8.x\n");
    print(b"===================================\n");
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); } }
}
