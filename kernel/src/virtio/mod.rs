//! VirtIO 드라이버 서브시스템 (ALPHA 10–11)
//!
//! ## 지원 디바이스
//! - Block (`blk`): QEMU virtio-blk-pci, 섹터 읽기/쓰기 (ALPHA 10)
//! - Net   (`net`): QEMU virtio-net-pci, ARP/Ethernet TX/RX (ALPHA 11)

pub mod blk;
pub mod net;
pub mod queue;

pub use blk::VirtioBlk;
pub use net::VirtioNet;
