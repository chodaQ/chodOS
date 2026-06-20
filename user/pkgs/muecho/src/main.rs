//! muecho — 인자를 stdout에 출력하는 패키지 (ALPHA 16)
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

// SYS_PKG_GETARGS = 201: 현재 패키지에 전달된 args 문자열 읽기
#[inline(always)]
unsafe fn sys_pkg_getargs(buf: *mut u8, len: usize) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "int 0x80",
        inlateout("rax") 201i64 => ret,
        in("rdi") buf,
        in("rsi") len,
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

#[no_mangle]
pub extern "C" fn pkg_main() {
    let mut buf = [0u8; 512];
    let n = unsafe { sys_pkg_getargs(buf.as_mut_ptr(), 512) };
    if n > 0 {
        print(&buf[..n as usize]);
    }
    print(b"\n");
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); } }
}
