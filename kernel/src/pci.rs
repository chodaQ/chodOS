//! PCI 설정 공간 접근 (Config Mechanism #1)
//!
//! CONFIG_ADDRESS (0xCF8): 버스/디바이스/함수/오프셋 선택
//! CONFIG_DATA    (0xCFC): 선택된 레지스터 읽기/쓰기
//!
//! 포트 I/O 헬퍼(outl/inl/outw/inw/outb/inb)도 여기서 제공.

const ADDR_PORT: u16 = 0xCF8;
const DATA_PORT: u16 = 0xCFC;

fn mk_addr(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    (1u32 << 31)
        | ((bus  as u32) << 16)
        | ((dev  as u32) << 11)
        | ((func as u32) <<  8)
        | ((off  as u32) & 0xFC)
}

// ── PCI 설정 읽기/쓰기 ──────────────────────────────────────────────────────

pub fn read32(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    unsafe { outl(ADDR_PORT, mk_addr(bus, dev, func, off)); inl(DATA_PORT) }
}

pub fn read16(bus: u8, dev: u8, func: u8, off: u8) -> u16 {
    (read32(bus, dev, func, off & !3) >> ((off & 2) * 8)) as u16
}

pub fn write32(bus: u8, dev: u8, func: u8, off: u8, val: u32) {
    unsafe { outl(ADDR_PORT, mk_addr(bus, dev, func, off)); outl(DATA_PORT, val); }
}

pub fn write16(bus: u8, dev: u8, func: u8, off: u8, val: u16) {
    let old = read32(bus, dev, func, off & !3);
    let shift = (off & 2) * 8;
    let new = (old & !(0xFFFF << shift)) | ((val as u32) << shift);
    write32(bus, dev, func, off & !3, new);
}

// ── 디바이스 탐색 ─────────────────────────────────────────────────────────────

/// PCI 설정 공간에서 vendor/device ID로 디바이스 검색.
/// 반환: (bus, dev) 또는 None
pub fn find_device(vendor: u16, device: u16) -> Option<(u8, u8)> {
    for bus in 0u8..=255 {
        for dev in 0u8..32 {
            let v = read32(bus, dev, 0, 0x00);
            if v == 0xFFFF_FFFF { continue; }
            let vid = (v & 0xFFFF) as u16;
            let did = (v >> 16) as u16;
            if vid == vendor && did == device {
                return Some((bus, dev));
            }
        }
    }
    None
}

/// BAR(idx) I/O 포트 베이스 주소 (bit0=1이면 I/O 공간).
/// 하위 2비트를 마스크해서 반환.
pub fn bar_io_base(bus: u8, dev: u8, bar_idx: u8) -> Option<u16> {
    let bar = read32(bus, dev, 0, 0x10 + bar_idx * 4);
    if bar & 1 == 1 { Some((bar & !3) as u16) } else { None }
}

/// Command 레지스터에 I/O Space Enable + Bus Master Enable 설정.
pub fn enable_io_and_busmaster(bus: u8, dev: u8) {
    let cmd = read16(bus, dev, 0, 0x04);
    write16(bus, dev, 0, 0x04, cmd | 0x0005); // bit0=I/O, bit2=BusMaster
}

// ── 포트 I/O 헬퍼 ────────────────────────────────────────────────────────────

#[inline]
pub unsafe fn outl(port: u16, val: u32) {
    core::arch::asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack));
}

#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    let v: u32;
    core::arch::asm!("in eax, dx", out("eax") v, in("dx") port, options(nomem, nostack));
    v
}

#[inline]
pub unsafe fn outw(port: u16, val: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack));
}

#[inline]
pub unsafe fn inw(port: u16) -> u16 {
    let v: u16;
    core::arch::asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack));
    v
}

#[inline]
pub unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
}

#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack));
    v
}
