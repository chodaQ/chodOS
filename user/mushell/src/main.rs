//! MuShell — MuKernel ALPHA 15/16 인터랙티브 셸 + 패키지 관리자
//!
//! `no_std` Rust 바이너리, `x86_64-unknown-none` 타겟.
//! 커널의 `int 0x80` Linux ABI를 통해 syscall 수행.
//!
//! ## 지원 명령어
//! ```
//! echo [text]            텍스트 출력
//! uname                  OS/커널 정보
//! whoami                 현재 사용자 (항상 root)
//! help                   도움말
//! mukg list              패키지 목록
//! mukg install <pkg>     패키지 설치 확인
//! mukg run <pkg> [args]  패키지 실행
//! mukg remove <pkg>      패키지 제거
//! <pkg> [args]           패키지 직접 실행
//! exit / quit            셸 종료
//! ```

#![no_std]
#![no_main]

use core::arch::global_asm;

// ── 진입점 (_start) ───────────────────────────────────────────────────────────

global_asm!(
    ".section .text.entry",
    ".global _start",
    "_start:",
    "    xor ebp, ebp",
    "    call mukernel_main",
    "    mov eax, 60",
    "    xor edi, edi",
    "    int 0x80",
);

// ── int 0x80 syscall ABI ─────────────────────────────────────────────────────

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
unsafe fn sys_read(fd: i64, buf: *mut u8, len: usize) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "int 0x80",
        inlateout("rax") 0i64 => ret,
        in("rdi") fd,
        in("rsi") buf,
        in("rdx") len,
        options(nostack),
    );
    ret
}

#[inline(always)]
unsafe fn sys_exit(code: i64) -> ! {
    core::arch::asm!(
        "int 0x80",
        in("rax") 60i64,
        in("rdi") code,
        options(noreturn, nostack),
    );
}

// ── BETA 7 syscall 추가 ───────────────────────────────────────────────────────

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

#[inline(always)]
unsafe fn sys_getdents64(fd: i64, buf: *mut u8, count: usize) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 217i64 => ret,
        in("rdi") fd, in("rsi") buf, in("rdx") count,
        options(nostack));
    ret
}

#[inline(always)]
unsafe fn sys_getcwd(buf: *mut u8, size: usize) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 79i64 => ret,
        in("rdi") buf, in("rsi") size,
        options(nostack));
    ret
}

#[inline(always)]
unsafe fn sys_mkdir(path: *const u8, mode: i64) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 83i64 => ret,
        in("rdi") path, in("rsi") mode,
        options(nostack));
    ret
}

/// ALPHA 16: 패키지를 이름으로 exec.
/// 성공 시 noreturn (longjmp → 패키지 실행 → mushell 재시작).
/// 실패 시 ENOENT (-2) 반환.
#[inline(always)]
unsafe fn sys_execve(path: *const u8, args: *const u8) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "int 0x80",
        inlateout("rax") 59i64 => ret,
        in("rdi") path,
        in("rsi") args,
        in("rdx") 0u64,
        options(nostack),
    );
    ret
}

/// ALPHA 16: 패키지 목록 → buf에 기록. 반환: 기록한 바이트 수.
#[inline(always)]
unsafe fn sys_pkg_list(buf: *mut u8, max_len: usize) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "int 0x80",
        inlateout("rax") 400i64 => ret,
        in("rdi") buf,
        in("rsi") max_len,
        options(nostack),
    );
    ret
}

// BETA 8: 패키지 관리자 고도화 syscall

#[inline(always)]
unsafe fn sys_pkg_info(name: *const u8, buf: *mut u8, max_len: usize) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 403i64 => ret,
        in("rdi") name, in("rsi") buf, in("rdx") max_len,
        options(nostack));
    ret
}

#[inline(always)]
unsafe fn sys_pkg_install(name: *const u8) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 404i64 => ret,
        in("rdi") name,
        options(nostack));
    ret
}

#[inline(always)]
unsafe fn sys_pkg_remove(name: *const u8) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 405i64 => ret,
        in("rdi") name,
        options(nostack));
    ret
}

#[inline(always)]
unsafe fn sys_pkg_installed(buf: *mut u8, max_len: usize) -> i64 {
    let ret: i64;
    core::arch::asm!("int 0x80",
        inlateout("rax") 406i64 => ret,
        in("rdi") buf, in("rsi") max_len,
        options(nostack));
    ret
}

// ── I/O 헬퍼 ─────────────────────────────────────────────────────────────────

fn print(s: &[u8]) {
    let mut off = 0;
    while off < s.len() {
        let n = unsafe { sys_write(1, s[off..].as_ptr(), s.len() - off) };
        if n > 0 { off += n as usize; } else { break; }
    }
}

fn read_byte() -> u8 {
    let mut b = 0u8;
    loop {
        if unsafe { sys_read(0, &mut b, 1) } == 1 { return b; }
    }
}

fn read_line(buf: &mut [u8]) -> usize {
    let mut n = 0usize;
    loop {
        let c = read_byte();
        match c {
            b'\n' | b'\r' => {
                print(b"\n");
                break;
            }
            3 => {
                print(b"^C\n");
                return 0;
            }
            4 => {
                print(b"\n");
                let exit = b"exit";
                buf[..4].copy_from_slice(exit);
                return 4;
            }
            127 | 8 => {
                // 백스페이스: 버퍼에서 제거 + 화면에서 지우기 (\b space \b)
                if n > 0 {
                    n -= 1;
                    print(b"\x08 \x08");
                }
            }
            32..=126 => {
                if n + 1 < buf.len() {
                    buf[n] = c;
                    n += 1;
                    // 에코: 입력 글자를 즉시 화면에 출력
                    unsafe { sys_write(1, &c as *const u8, 1); }
                }
            }
            _ => {}
        }
    }
    n
}

// ── 문자열 유틸 ──────────────────────────────────────────────────────────────

fn trim(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&c| c != b' ').unwrap_or(s.len());
    let end   = s.iter().rposition(|&c| c != b' ').map(|i| i + 1).unwrap_or(0);
    if start >= end { b"" } else { &s[start..end] }
}

/// 첫 번째 공백에서 (이름, 나머지) 분리
fn split_first_word(s: &[u8]) -> (&[u8], &[u8]) {
    if let Some(i) = s.iter().position(|&b| b == b' ') {
        (&s[..i], trim(&s[i+1..]))
    } else {
        (s, b"")
    }
}

// ── 패키지 실행 ──────────────────────────────────────────────────────────────

// ── BETA 7 내장 명령어 구현 ──────────────────────────────────────────────────

const O_DIRECTORY: i64 = 0x10000;
const DT_DIR: u8 = 4;
const DENT_HDR: usize = 19; // d_ino(8)+d_off(8)+d_reclen(2)+d_type(1)

fn cmd_ls(path_arg: &[u8]) {
    // 경로 결정: 인자 없으면 "/"
    let mut path_buf = [0u8; 256];
    let path_len = if path_arg.is_empty() {
        path_buf[0] = b'/';
        1
    } else {
        let l = path_arg.len().min(254);
        path_buf[..l].copy_from_slice(&path_arg[..l]);
        l
    };
    path_buf[path_len] = 0;

    let fd = unsafe { sys_open(path_buf.as_ptr(), O_DIRECTORY) };
    if fd < 0 {
        print(b"ls: cannot open directory\n");
        return;
    }
    let mut buf = [0u8; 4096];
    let n = unsafe { sys_getdents64(fd, buf.as_mut_ptr(), 4096) };
    unsafe { sys_close(fd) };
    if n <= 0 { return; }

    let total = n as usize;
    let mut pos = 0usize;
    while pos + DENT_HDR <= total {
        let d_reclen = u16::from_le_bytes([buf[pos + 16], buf[pos + 17]]) as usize;
        if d_reclen == 0 { break; }
        let d_type   = buf[pos + 18];
        let name_start = pos + DENT_HDR;
        let name_limit = (pos + d_reclen).min(total);
        let mut name_end = name_start;
        while name_end < name_limit && buf[name_end] != 0 { name_end += 1; }
        if name_end > name_start {
            print(&buf[name_start..name_end]);
            if d_type == DT_DIR { print(b"/"); }
            print(b"\n");
        }
        pos += d_reclen;
    }
}

fn cmd_pwd() {
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

fn cmd_mkdir(path_arg: &[u8]) {
    if path_arg.is_empty() {
        print(b"mkdir: missing operand\nUsage: mkdir <path>\n");
        return;
    }
    let mut path_buf = [0u8; 256];
    let l = path_arg.len().min(254);
    path_buf[..l].copy_from_slice(&path_arg[..l]);
    path_buf[l] = 0;
    let r = unsafe { sys_mkdir(path_buf.as_ptr(), 0o755) };
    if r < 0 {
        print(b"mkdir: cannot create directory\n");
    }
}

/// `cmd`에서 패키지 이름과 args를 분리해 sys_execve 호출.
/// 성공 시 절대 반환하지 않음 (longjmp), 실패 시 음수 반환.
fn exec_pkg(cmd: &[u8]) -> i64 {
    let (name, args) = split_first_word(cmd);
    let mut name_buf = [0u8; 64];
    let nl = name.len().min(63);
    name_buf[..nl].copy_from_slice(&name[..nl]);
    // name_buf[nl] = 0 (이미 0으로 초기화됨)

    let mut args_buf = [0u8; 512];
    let al = args.len().min(511);
    args_buf[..al].copy_from_slice(&args[..al]);

    unsafe { sys_execve(name_buf.as_ptr(), args_buf.as_ptr()) }
}

// ── mukg 서브커맨드 ──────────────────────────────────────────────────────────

fn cmd_mukg(args: &[u8]) {
    let args = trim(args);

    if args.is_empty() || args == b"list" {
        let mut buf = [0u8; 2048];
        let n = unsafe { sys_pkg_list(buf.as_mut_ptr(), 2048) };
        if n > 0 {
            print(b"Available packages ([*]=installed  [ ]=not installed):\n");
            for line in buf[..n as usize].split(|&b| b == b'\n') {
                if !line.is_empty() {
                    print(b"  ");
                    print(line);
                    print(b"\n");
                }
            }
        } else {
            print(b"No packages available.\n");
        }

    } else if args.starts_with(b"info ") {
        let pkg = trim(&args[5..]);
        let mut name_buf = [0u8; 64];
        let nl = pkg.len().min(63);
        name_buf[..nl].copy_from_slice(&pkg[..nl]);
        let mut buf = [0u8; 512];
        let n = unsafe { sys_pkg_info(name_buf.as_ptr(), buf.as_mut_ptr(), 512) };
        if n < 0 {
            print(b"[mukg] error: package '");
            print(pkg);
            print(b"' not found.\n");
        } else {
            print(&buf[..n as usize]);
        }

    } else if args.starts_with(b"install ") {
        let pkg = trim(&args[8..]);
        let mut name_buf = [0u8; 64];
        let nl = pkg.len().min(63);
        name_buf[..nl].copy_from_slice(&pkg[..nl]);
        print(b"[mukg] Installing ");
        print(pkg);
        print(b"...\n");
        let r = unsafe { sys_pkg_install(name_buf.as_ptr()) };
        if r < 0 {
            print(b"[mukg] error: package '");
            print(pkg);
            print(b"' not found in registry.\n");
        } else {
            print(b"[mukg] Package '");
            print(pkg);
            print(b"' installed successfully.\n");
        }

    } else if args.starts_with(b"remove ") {
        let pkg = trim(&args[7..]);
        let mut name_buf = [0u8; 64];
        let nl = pkg.len().min(63);
        name_buf[..nl].copy_from_slice(&pkg[..nl]);
        let r = unsafe { sys_pkg_remove(name_buf.as_ptr()) };
        if r < 0 {
            print(b"[mukg] error: package '");
            print(pkg);
            print(b"' not found.\n");
        } else {
            print(b"[mukg] Package '");
            print(pkg);
            print(b"' removed.\n");
        }

    } else if args == b"installed" {
        let mut buf = [0u8; 2048];
        let n = unsafe { sys_pkg_installed(buf.as_mut_ptr(), 2048) };
        if n > 0 { print(&buf[..n as usize]); }

    } else if args.starts_with(b"run ") {
        let rest = trim(&args[4..]);
        let ret = exec_pkg(rest);
        if ret < 0 {
            print(b"[mukg] error: package not found: ");
            let (name, _) = split_first_word(rest);
            print(name);
            print(b"\n");
        }

    } else if args == b"upgrade" {
        // 설치된 패키지를 최신 버전으로 재표시 (정적 커널에서는 버전 고정)
        let mut buf = [0u8; 2048];
        let n = unsafe { sys_pkg_list(buf.as_mut_ptr(), 2048) }.max(0) as usize;
        print(b"[mukg] Checking for upgrades...\n");
        let mut any = false;
        for line in buf[..n].split(|&b| b == b'\n') {
            if line.starts_with(b"[*]") {
                print(b"  ");
                print(line);
                print(b" (up to date)\n");
                any = true;
            }
        }
        if !any { print(b"[mukg] No packages installed. Use: mukg install <pkg>\n"); }

    } else if args == b"search" || args.starts_with(b"search ") {
        let kw = if args.len() > 7 { trim(&args[7..]) } else { b"" };
        if kw.is_empty() {
            print(b"Usage: mukg search <keyword>\n");
            return;
        }
        let mut buf = [0u8; 2048];
        let n = unsafe { sys_pkg_list(buf.as_mut_ptr(), 2048) }.max(0) as usize;
        let mut found = false;
        for line in buf[..n].split(|&b| b == b'\n') {
            // 마커 4바이트 제거 후 키워드 검색 ("[ ] " or "[*] ")
            let text = if line.len() > 4 { &line[4..] } else { line };
            if contains(text, kw) {
                if !found { print(b"Search results:\n"); found = true; }
                print(b"  ");
                print(line);
                print(b"\n");
            }
        }
        if !found {
            print(b"[mukg] No packages matching '");
            print(kw);
            print(b"'.\n");
        }

    } else if args == b"help" {
        print(MUKG_HELP);

    } else {
        print(b"[mukg] unknown subcommand. Try: mukg help\n");
    }
}

/// 슬라이스에서 패턴 포함 여부 확인 (대소문자 구분)
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() { return true; }
    if needle.len() > haystack.len() { return false; }
    for i in 0..=(haystack.len() - needle.len()) {
        if &haystack[i..i + needle.len()] == needle { return true; }
    }
    false
}

// ── 셸 상수 ──────────────────────────────────────────────────────────────────

const BANNER: &[u8] = b"\n\
 __  __       _  __                    _ \n\
|  \\/  |_   _| |/ /___ _ __ _ __   ___| |\n\
| |\\/| | | | | ' // _ \\ '__| '_ \\ / _ \\ |\n\
| |  | | |_| | . \\  __/ |  | | | |  __/ |\n\
|_|  |_|\\__,_|_|\\_\\___|_|  |_| |_|\\___|_|\n\
\n\
 MuKernel Shell v0.1  --  ALPHA 15/16\n\
 Type 'help' for commands.  'mukg list' for packages.\n\
\n";

const HELP: &[u8] = b"Built-in commands:\n\
  echo [text]    print text\n\
  ls [path]      list directory (default: /)\n\
  pwd            print working directory\n\
  mkdir <path>   create directory\n\
  cat <file>     print file contents\n\
  uname          OS/kernel information\n\
  whoami         current user\n\
  help           show this help\n\
  exit           exit the shell\n\
\n\
Package manager (mukg):\n\
  mukg list              list all packages ([*]=installed)\n\
  mukg info <pkg>        show package details\n\
  mukg install <pkg>     install a package\n\
  mukg remove <pkg>      remove a package\n\
  mukg installed         list installed packages\n\
  mukg upgrade           check for upgrades\n\
  mukg search <kw>       search packages\n\
  mukg run <pkg> [args]  run a package\n\
  <pkg> [args]           run package directly\n\
\n\
Examples:\n\
  ls /etc\n\
  cat /etc/hostname\n\
  mkdir /tmp/test\n\
  sysinfo\n\
  muecho hello world\n";

const MUKG_HELP: &[u8] = b"mukg - MuKernel Package Manager v0.2.0 (BETA 8)\n\
Usage:\n\
  mukg list              list all packages ([*]=installed)\n\
  mukg info <pkg>        show package details\n\
  mukg install <pkg>     install a package (with deps)\n\
  mukg remove <pkg>      remove a package\n\
  mukg installed         list installed packages only\n\
  mukg upgrade           check for upgrades\n\
  mukg search <kw>       search packages by keyword\n\
  mukg run <pkg> [args]  run a package directly\n\
  mukg help              show this help\n";

// ── 셸 메인 루프 ─────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn mukernel_main() {
    print(BANNER);

    let mut buf = [0u8; 256];

    loop {
        print(b"mukernel$ ");

        let n   = read_line(&mut buf);
        let cmd = trim(&buf[..n]);

        if cmd.is_empty() {
            // 빈 줄: 다시 프롬프트
        } else if cmd == b"help" {
            print(HELP);
        } else if cmd == b"uname" {
            print(b"MuKernel 0.1.0-alpha x86_64 (ALPHA-16)\n");
        } else if cmd == b"whoami" {
            print(b"root\n");
        } else if cmd == b"echo" || cmd.starts_with(b"echo ") {
            if cmd.len() > 5 { print(&cmd[5..]); }
            print(b"\n");
        } else if cmd == b"mukg" || cmd.starts_with(b"mukg ") {
            cmd_mukg(if cmd.len() > 5 { &cmd[5..] } else { b"" });
        // ── BETA 7 내장 명령어 ──────────────────────────────────────────
        } else if cmd == b"ls" || cmd.starts_with(b"ls ") {
            let path = if cmd.len() > 3 { trim(&cmd[3..]) } else { b"" };
            cmd_ls(path);
        } else if cmd == b"pwd" {
            cmd_pwd();
        } else if cmd == b"mkdir" || cmd.starts_with(b"mkdir ") {
            let path = if cmd.len() > 6 { trim(&cmd[6..]) } else { b"" };
            cmd_mkdir(path);
        } else if cmd == b"cat" || cmd.starts_with(b"cat ") {
            // cat → mucat 패키지로 위임
            let args = if cmd.len() > 4 { trim(&cmd[4..]) } else { b"" };
            let mut mcmd = [0u8; 270];
            mcmd[..5].copy_from_slice(b"mucat");
            if !args.is_empty() {
                mcmd[5] = b' ';
                let al = args.len().min(263);
                mcmd[6..6 + al].copy_from_slice(&args[..al]);
                exec_pkg(&mcmd[..6 + al]);
            } else {
                print(b"cat: missing file operand\n");
            }
        } else if cmd == b"exit" || cmd == b"quit" {
            print(b"Goodbye!\n");
            unsafe { sys_exit(0); }
        } else {
            // 패키지로 실행 시도 (ALPHA 16)
            let ret = exec_pkg(cmd);
            if ret < 0 {
                print(b"mukernel: ");
                print(cmd);
                print(b": command not found\n");
            }
        }
    }
}

// ── 패닉 핸들러 ──────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    print(b"\nSHELL PANIC\n");
    unsafe { sys_exit(111); }
}
