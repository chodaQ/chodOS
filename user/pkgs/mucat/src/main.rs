//! mucat — ext4에서 파일을 읽어 출력하는 패키지 (ALPHA 16)
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

// SYS_PKG_GETARGS = 201: 현재 패키지에 전달된 args (파일 경로)
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

// SYS_EXT4_READ = 202: ext4 이미지에서 파일 읽기
// (path, buf, max_len) → 읽은 바이트 수 또는 -2(ENOENT)
#[inline(always)]
unsafe fn sys_ext4_read(path: *const u8, buf: *mut u8, len: usize) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "int 0x80",
        inlateout("rax") 202i64 => ret,
        in("rdi") path,
        in("rsi") buf,
        in("rdx") len,
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
    // args = 파일 경로 (예: "/etc/hostname")
    let mut path_buf = [0u8; 256];
    let path_len = unsafe { sys_pkg_getargs(path_buf.as_mut_ptr(), 255) };
    if path_len <= 0 {
        print(b"mucat: missing file operand\nUsage: mucat <file>\n");
        return;
    }
    // null-terminate
    path_buf[path_len as usize] = 0;

    let mut data = [0u8; 4096];
    let data_len = unsafe {
        sys_ext4_read(path_buf.as_ptr(), data.as_mut_ptr(), 4096)
    };
    if data_len < 0 {
        print(b"mucat: ");
        print(&path_buf[..path_len as usize]);
        print(b": No such file or directory\n");
        return;
    }
    print(&data[..data_len as usize]);
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); } }
}
