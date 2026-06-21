//! BETA 12: /proc 가상 파일시스템
//!
//! 읽기 전용. pacman/glibc가 접근하는 최소한의 /proc 파일들만 구현.

use alloc::{format, string::String, vec::Vec};

/// `/proc` 하위 경로인지.
pub fn is_proc(path: &str) -> bool {
    path == "/proc" || path.starts_with("/proc/")
}

/// `/proc` 경로에서 내용 읽기. None이면 경로 없음.
pub fn read(path: &str) -> Option<Vec<u8>> {
    match path {
        "/proc/version" => Some(
            b"Linux version 5.15.0-mukernel (MuKernel) \
              (gcc 12.2.0) #1 SMP MuKernel 2026\n".to_vec()
        ),
        "/proc/meminfo" => Some(format!(
            "MemTotal:       {total} kB\n\
             MemFree:        {free} kB\n\
             MemAvailable:   {avail} kB\n\
             Buffers:        0 kB\n\
             Cached:         0 kB\n\
             SwapTotal:      0 kB\n\
             SwapFree:       0 kB\n",
            total = 512 * 1024,
            free  = 256 * 1024,
            avail = 256 * 1024,
        ).into_bytes()),
        "/proc/filesystems" => Some(b"nodev\ttmpfs\n\text4\n".to_vec()),
        "/proc/mounts" | "/proc/self/mounts" => Some(
            b"tmpfs / tmpfs rw,relatime 0 0\n\
              ext4 /ext4 ext4 ro,relatime 0 0\n".to_vec()
        ),
        "/proc/self/exe"     => Some(b"/usr/bin/sh\0".to_vec()),
        "/proc/self/cmdline" => Some(b"sh\0".to_vec()),
        "/proc/self/environ" => Some(b"PATH=/usr/bin:/bin\0HOME=/root\0\0".to_vec()),
        "/proc/self/status"  => Some(format!(
            "Name:\tsh\nState:\tR (running)\nPid:\t{pid}\nPPid:\t0\n\
             VmRSS:\t4096 kB\nThreads:\t1\n",
            pid = crate::process::userproc::current_pid(),
        ).into_bytes()),
        "/proc/self/maps" => Some(Vec::new()), // 빈 maps
        "/proc/self/fd"   => Some(Vec::new()),
        "/proc/sys/kernel/hostname" => Some(b"mukernel\n".to_vec()),
        "/proc/sys/kernel/ostype"   => Some(b"Linux\n".to_vec()),
        "/proc/sys/kernel/osrelease"=> Some(b"5.15.0-mukernel\n".to_vec()),
        "/proc/cpuinfo" => Some(format!(
            "processor\t: 0\nvendor_id\t: MuKernel\ncpu family\t: 6\n\
             model name\t: MuKernel x86_64 vCPU\ncpu cores\t: {}\n\
             cache size\t: 4096 KB\n\n",
            crate::smp::cpu_count()
        ).into_bytes()),
        _ => None,
    }
}

/// `/proc` 경로 존재 여부.
pub fn exists(path: &str) -> bool {
    match path {
        "/proc" | "/proc/self" | "/proc/self/fd"
        | "/proc/sys" | "/proc/sys/kernel" => true,
        _ => read(path).is_some(),
    }
}

/// `/proc` 디렉토리 목록.
pub fn list(path: &str) -> Vec<(String, bool)> {
    match path {
        "/proc" => alloc::vec![
            ("version".into(), false),
            ("meminfo".into(), false),
            ("filesystems".into(), false),
            ("mounts".into(), false),
            ("cpuinfo".into(), false),
            ("self".into(), true),
            ("sys".into(), true),
        ],
        "/proc/self" => alloc::vec![
            ("exe".into(), false),
            ("cmdline".into(), false),
            ("environ".into(), false),
            ("status".into(), false),
            ("maps".into(), false),
            ("fd".into(), true),
            ("mounts".into(), false),
        ],
        "/proc/sys" => alloc::vec![("kernel".into(), true)],
        "/proc/sys/kernel" => alloc::vec![
            ("hostname".into(), false),
            ("ostype".into(), false),
            ("osrelease".into(), false),
        ],
        "/proc/self/fd" => alloc::vec![],
        _ => alloc::vec![],
    }
}

/// readlink for /proc paths (proc/self/exe 등).
pub fn readlink(path: &str) -> Option<Vec<u8>> {
    match path {
        "/proc/self/exe" => Some(b"/usr/bin/sh".to_vec()),
        _ => None,
    }
}
