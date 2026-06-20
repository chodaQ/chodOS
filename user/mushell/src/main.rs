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
        inlateout("rax") 200i64 => ret,
        in("rdi") buf,
        in("rsi") max_len,
        options(nostack),
    );
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
            b'\n' | b'\r' => break,
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
                if n > 0 { n -= 1; }
            }
            32..=126 => {
                if n + 1 < buf.len() {
                    buf[n] = c;
                    n += 1;
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
        let mut buf = [0u8; 1024];
        let n = unsafe { sys_pkg_list(buf.as_mut_ptr(), 1024) };
        if n > 0 {
            print(b"Available packages:\n");
            // 각 줄에 들여쓰기
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

    } else if args.starts_with(b"install ") {
        let pkg = trim(&args[8..]);
        // 존재 여부 확인: exec_pkg 시뮬레이션 없이 list에서 검색
        let mut buf = [0u8; 1024];
        let n = unsafe { sys_pkg_list(buf.as_mut_ptr(), 1024) }.max(0) as usize;
        let found = pkg_in_list(&buf[..n], pkg);
        if found {
            print(b"[mukg] Installing ");
            print(pkg);
            print(b"...\n[mukg] Package '");
            print(pkg);
            print(b"' installed successfully.\n");
        } else {
            print(b"[mukg] error: package '");
            print(pkg);
            print(b"' not found in registry.\n");
        }

    } else if args.starts_with(b"run ") {
        let rest = trim(&args[4..]);
        let ret = exec_pkg(rest);
        if ret < 0 {
            print(b"[mukg] error: package not found: ");
            let (name, _) = split_first_word(rest);
            print(name);
            print(b"\n");
        }

    } else if args.starts_with(b"remove ") {
        let pkg = trim(&args[7..]);
        print(b"[mukg] Package '");
        print(pkg);
        print(b"' removed.\n");

    } else if args == b"help" {
        print(MUKG_HELP);

    } else {
        print(b"[mukg] unknown subcommand. Try: mukg help\n");
    }
}

/// pkg_list 출력에서 첫 단어가 `pkg`인 줄이 있는지 확인
fn pkg_in_list(list: &[u8], pkg: &[u8]) -> bool {
    for line in list.split(|&b| b == b'\n') {
        let line = trim(line);
        if line.starts_with(pkg) {
            if line.len() == pkg.len() || line.get(pkg.len()) == Some(&b' ') {
                return true;
            }
        }
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
  echo [text]   print text\n\
  uname         OS/kernel information\n\
  whoami        current user\n\
  help          show this help\n\
  exit          exit the shell\n\
\n\
Package manager (mukg):\n\
  mukg list              list available packages\n\
  mukg install <pkg>     install a package\n\
  mukg run <pkg> [args]  run a package\n\
  mukg remove <pkg>      remove a package\n\
  <pkg> [args]           run package directly\n\
\n\
Examples:\n\
  sysinfo\n\
  muecho hello world\n\
  mucat /etc/hostname\n";

const MUKG_HELP: &[u8] = b"mukg - MuKernel Package Manager v0.1.0\n\
Usage:\n\
  mukg list              list available packages\n\
  mukg install <pkg>     install a package\n\
  mukg run <pkg> [args]  run a package\n\
  mukg remove <pkg>      remove a package\n\
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
