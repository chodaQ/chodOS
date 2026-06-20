//! ALPHA 16 패키지 레지스트리
//!
//! 커널에 embed된 정적 패키지 목록.
//! mushell의 `mukg` 명령이 `sys_pkg_list` / `sys_execve` syscall을 통해 접근.

use core::sync::atomic::{AtomicUsize, Ordering};

pub struct Package {
    pub name: &'static str,
    pub version: &'static str,
    pub desc: &'static str,
    pub elf: &'static [u8],
}

pub static PACKAGES: &[Package] = &[
    Package {
        name: "sysinfo",
        version: "0.1.0",
        desc: "Display system information",
        elf: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/sysinfo.elf")),
    },
    Package {
        name: "muecho",
        version: "0.1.0",
        desc: "Echo arguments to stdout",
        elf: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/muecho.elf")),
    },
    Package {
        name: "mucat",
        version: "0.1.0",
        desc: "Read and print a file from ext4",
        elf: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/mucat.elf")),
    },
];

// 현재 실행 중인 패키지에 전달할 args 버퍼 (sys_execve가 저장, sys_pkg_getargs가 읽음)
static mut PKG_ARGS_BUF: [u8; 512] = [0u8; 512];
static PKG_ARGS_LEN: AtomicUsize = AtomicUsize::new(0);

pub fn find(name: &str) -> Option<usize> {
    PACKAGES.iter().position(|p| p.name == name)
}

pub fn set_args(args: &[u8]) {
    let len = args.len().min(511);
    unsafe {
        core::ptr::copy_nonoverlapping(args.as_ptr(), PKG_ARGS_BUF.as_mut_ptr(), len);
        PKG_ARGS_BUF[len] = 0;
    }
    PKG_ARGS_LEN.store(len, Ordering::Relaxed);
}

pub fn get_args() -> &'static [u8] {
    let len = PKG_ARGS_LEN.load(Ordering::Relaxed);
    unsafe { &PKG_ARGS_BUF[..len] }
}
