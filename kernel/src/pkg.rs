//! BETA 8 패키지 레지스트리
//!
//! 커널에 embed된 정적 패키지 목록.
//! mushell의 `mukg` 명령이 `sys_pkg_*` syscall을 통해 접근.
//! BETA 8: deps 필드 + 설치 상태 비트맵 + info/install/remove/installed syscall

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub struct Package {
    pub name:    &'static str,
    pub version: &'static str,
    pub desc:    &'static str,
    pub deps:    &'static [&'static str],
    pub elf:     &'static [u8],
}

pub static PACKAGES: &[Package] = &[
    Package {
        name:    "sysinfo",
        version: "0.1.0",
        desc:    "Display system information",
        deps:    &[],
        elf:     include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/sysinfo.elf")),
    },
    Package {
        name:    "muecho",
        version: "0.1.0",
        desc:    "Echo arguments to stdout",
        deps:    &[],
        elf:     include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/muecho.elf")),
    },
    Package {
        name:    "mucat",
        version: "0.1.0",
        desc:    "Read and print a file from ext4",
        deps:    &[],
        elf:     include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/mucat.elf")),
    },
    Package {
        name:    "muls",
        version: "0.1.0",
        desc:    "List directory entries (ls)",
        deps:    &[],
        elf:     include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/muls.elf")),
    },
    Package {
        name:    "mupwd",
        version: "0.1.0",
        desc:    "Print working directory (pwd)",
        deps:    &[],
        elf:     include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/mupwd.elf")),
    },
    // BETA 2-1: musl-static 바이너리 — Linux ABI 호환성 검증용
    Package {
        name:    "hello",
        version: "1.0.0",
        desc:    "musl-static hello world (BETA 2-1 compat test)",
        deps:    &[],
        elf:     include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/musl_hello.elf")),
    },
    Package {
        name:    "uname",
        version: "1.0.0",
        desc:    "musl-static uname test (syscall 63)",
        deps:    &[],
        elf:     include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../build/musl_uname.elf")),
    },
];

// 설치 상태 비트맵: bit i = PACKAGES[i]가 설치됨
static PKG_INSTALL_STATE: AtomicU64 = AtomicU64::new(0);

// 현재 실행 중인 패키지에 전달할 args 버퍼
static mut PKG_ARGS_BUF: [u8; 512] = [0u8; 512];
static PKG_ARGS_LEN: AtomicUsize = AtomicUsize::new(0);

pub fn find(name: &str) -> Option<usize> {
    PACKAGES.iter().position(|p| p.name == name)
}

pub fn install(idx: usize) {
    if idx < 64 {
        PKG_INSTALL_STATE.fetch_or(1u64 << idx, Ordering::Relaxed);
    }
}

pub fn remove(idx: usize) {
    if idx < 64 {
        PKG_INSTALL_STATE.fetch_and(!(1u64 << idx), Ordering::Relaxed);
    }
}

pub fn is_installed(idx: usize) -> bool {
    if idx >= 64 { return false; }
    PKG_INSTALL_STATE.load(Ordering::Relaxed) & (1u64 << idx) != 0
}

pub fn set_args(args: &[u8]) {
    let len = args.len().min(511);
    unsafe {
        let ptr = core::ptr::addr_of_mut!(PKG_ARGS_BUF) as *mut u8;
        core::ptr::copy_nonoverlapping(args.as_ptr(), ptr, len);
        ptr.add(len).write(0);
    }
    PKG_ARGS_LEN.store(len, Ordering::Relaxed);
}

pub fn get_args() -> &'static [u8] {
    let len = PKG_ARGS_LEN.load(Ordering::Relaxed);
    unsafe { core::slice::from_raw_parts(core::ptr::addr_of!(PKG_ARGS_BUF) as *const u8, len) }
}
