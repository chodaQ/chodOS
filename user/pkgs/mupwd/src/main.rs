//! mupwd — 현재 작업 디렉토리 출력 (BETA 7)
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
    core::arch::asm!("int 0x80",
        inlateout("rax") 1i64 => ret,
        in("rdi") fd, in("rsi") buf, in("rdx") len,
        options(nostack));
    ret
}

// SYS_GETCWD = 79: 현재 작업 디렉토리 → buf에 기록
#[inline(always)]
unsafe fn sys_getcwd(buf: *mut u8, size: usize) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 79i64 => ret,
        in("rdi") buf, in("rsi") size,
        options(nostack));
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
    let mut buf = [0u8; 256];
    let r = unsafe { sys_getcwd(buf.as_mut_ptr(), 255) };
    if r <= 0 {
        print(b"/\n");
        return;
    }
    let mut len = 0;
    while len < 255 && buf[len] != 0 { len += 1; }
    print(&buf[..len]);
    print(b"\n");
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); } }
}
