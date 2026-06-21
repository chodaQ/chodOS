//! muls — 디렉토리 목록 출력 (BETA 7)
//!
//! 사용법: muls [경로]   (경로 생략 시 "/" 목록)
//! 디렉토리는 이름 뒤에 "/" 표시.
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

#[inline(always)]
unsafe fn sys_open(path: *const u8, flags: i64) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 2i64 => ret,
        in("rdi") path, in("rsi") flags,
        options(nostack));
    ret
}

#[inline(always)]
unsafe fn sys_close(fd: i64) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 3i64 => ret,
        in("rdi") fd,
        options(nostack));
    ret
}

// SYS_GETDENTS64 = 217: 디렉토리 엔트리 읽기
#[inline(always)]
unsafe fn sys_getdents64(fd: i64, buf: *mut u8, count: usize) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 217i64 => ret,
        in("rdi") fd, in("rsi") buf, in("rdx") count,
        options(nostack));
    ret
}

// SYS_PKG_GETARGS = 401: 패키지 실행 시 전달된 args 읽기
#[inline(always)]
unsafe fn sys_pkg_getargs(buf: *mut u8, len: usize) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 401i64 => ret,
        in("rdi") buf, in("rsi") len,
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

// O_DIRECTORY = 0x10000: 디렉토리로 열기
const O_DIRECTORY: i64 = 0x10000;
// d_type 상수
const DT_DIR: u8 = 4;
// linux_dirent64 고정 헤더 크기: d_ino(8) + d_off(8) + d_reclen(2) + d_type(1) = 19
const DENT_HDR: usize = 19;

#[no_mangle]
pub extern "C" fn pkg_main() {
    // 경로 인자 읽기 (없으면 "/")
    let mut path_buf = [0u8; 256];
    let path_len = unsafe { sys_pkg_getargs(path_buf.as_mut_ptr(), 255) };
    let path_len = if path_len <= 0 { 0 } else { path_len as usize };
    if path_len == 0 {
        path_buf[0] = b'/';
        path_buf[1] = 0;
    } else {
        path_buf[path_len] = 0;
    }

    // 디렉토리 open (핸들 테이블에 DirResource 등록)
    let fd = unsafe { sys_open(path_buf.as_ptr(), O_DIRECTORY) };
    if fd < 0 {
        print(b"muls: cannot open directory\n");
        return;
    }

    // getdents64로 엔트리 읽기
    let mut buf = [0u8; 4096];
    let n = unsafe { sys_getdents64(fd, buf.as_mut_ptr(), 4096) };
    unsafe { sys_close(fd) };

    if n <= 0 {
        print(b"(empty)\n");
        return;
    }

    // linux_dirent64 파싱 및 출력
    // 구조: d_ino(8) d_off(8) d_reclen(2) d_type(1) d_name(\0 종료)
    let mut pos = 0usize;
    let total = n as usize;
    while pos + DENT_HDR <= total {
        let d_reclen = u16::from_le_bytes([buf[pos + 16], buf[pos + 17]]) as usize;
        if d_reclen == 0 { break; }
        let d_type   = buf[pos + 18];
        let name_start = pos + DENT_HDR;
        let name_limit = (pos + d_reclen).min(total);
        let mut name_end = name_start;
        while name_end < name_limit && buf[name_end] != 0 {
            name_end += 1;
        }
        if name_end > name_start {
            print(&buf[name_start..name_end]);
            if d_type == DT_DIR { print(b"/"); }
            print(b"\n");
        }
        pos += d_reclen;
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); } }
}
