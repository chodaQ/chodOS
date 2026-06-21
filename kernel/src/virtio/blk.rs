//! VirtIO Block 드라이버 (ALPHA 10)
//!
//! ## Legacy VirtIO BAR0 레지스터 (I/O 포트)
//!
//! ```text
//! BAR0+0x00: HOST_FEATURES (u32, R)
//! BAR0+0x04: GUEST_FEATURES (u32, W)
//! BAR0+0x08: QUEUE_ADDRESS  (u32, RW) — 큐 물리주소 / 4096 (PFN)
//! BAR0+0x0C: QUEUE_SIZE     (u16, R)
//! BAR0+0x0E: QUEUE_SELECT   (u16, W)
//! BAR0+0x10: QUEUE_NOTIFY   (u16, W)
//! BAR0+0x12: DEVICE_STATUS  (u8,  RW)
//! BAR0+0x13: ISR_STATUS     (u8,  R)
//! BAR0+0x14: capacity_lo    (u32, R) — 블록 디바이스 설정
//! BAR0+0x18: capacity_hi    (u32, R)
//! ```
//!
//! ## Virtqueue 메모리 레이아웃 (qsize N)
//!
//! ```text
//! [0        .. N*16):          Descriptor Table  (N × 16 bytes)
//! [N*16     .. N*16+6+N*2):    Available Ring
//! [align4k(avail_end) ..):     Used Ring
//! ```
//!
//! 모든 오프셋을 실제 디바이스 보고 qsize 기준으로 동적 계산.

use alloc::alloc::{alloc_zeroed, dealloc, Layout};
use core::sync::atomic::{fence, Ordering};

use crate::pci;

// ── 상수 ────────────────────────────────────────────────────────────────────

const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_BLK_DEVICE: u16 = 0x1001;

const R_GUEST_FEATURES: u16 = 0x04;
const R_QUEUE_ADDRESS:  u16 = 0x08;
const R_QUEUE_SIZE:     u16 = 0x0C;
const R_QUEUE_SELECT:   u16 = 0x0E;
const R_QUEUE_NOTIFY:   u16 = 0x10;
const R_STATUS:         u16 = 0x12;
const R_CAPACITY_LO:    u16 = 0x14;
const R_CAPACITY_HI:    u16 = 0x18;

const STATUS_ACK:       u8 = 1;
const STATUS_DRIVER:    u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FAILED:    u8 = 128;

const VIRTQ_DESC_F_NEXT:  u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

const BLK_T_IN:  u32 = 0; // 읽기
const BLK_T_OUT: u32 = 1; // 쓰기

// ── 큐 레이아웃 계산 ─────────────────────────────────────────────────────────

/// Available ring 시작 오프셋 (= descriptor table 크기)
fn avail_off(qsize: usize) -> usize { qsize * 16 }

/// Used ring 시작 오프셋 (available ring 끝을 4KB 올림)
fn used_off(qsize: usize) -> usize {
    let avail_end = avail_off(qsize) + 6 + qsize * 2; // flags(2) + idx(2) + ring(qsize*2) + used_event(2)
    (avail_end + 4095) & !4095
}

/// 전체 큐 할당 크기 (페이지 올림)
fn queue_alloc_size(qsize: usize) -> usize {
    let total = used_off(qsize) + 6 + qsize * 8; // used ring: flags+idx+ring+avail_event
    (total + 4095) & !4095
}

// ── Block 요청 헤더 ──────────────────────────────────────────────────────────

#[repr(C)]
struct BlkReqHdr {
    type_:    u32,
    reserved: u32,
    sector:   u64,
}

// ── 드라이버 ─────────────────────────────────────────────────────────────────

pub struct VirtioBlk {
    io_base:   u16,
    queue:     *mut u8,
    qsize:     usize,
    avail_off: usize,
    used_off:  usize,
    avail_idx: u16,
    last_used: u16,
    alloc_sz:  usize,
    pub capacity: u64,
}

unsafe impl Send for VirtioBlk {}
unsafe impl Sync for VirtioBlk {}

impl VirtioBlk {
    pub fn init() -> Option<Self> {
        let (bus, dev) = pci::find_device(VIRTIO_VENDOR, VIRTIO_BLK_DEVICE)?;
        crate::serial_println!("[virtio-blk] found at PCI {:02x}:{:02x}", bus, dev);

        pci::enable_io_and_busmaster(bus, dev);
        let io_base = pci::bar_io_base(bus, dev, 0)?;
        crate::serial_println!("[virtio-blk] BAR0 I/O = 0x{:04x}", io_base);

        unsafe {
            // 리셋 → ACK → DRIVER
            outb(io_base + R_STATUS, 0);
            outb(io_base + R_STATUS, STATUS_ACK | STATUS_DRIVER);
            // feature 협상 없음 (최소 동작)
            outl(io_base + R_GUEST_FEATURES, 0);

            // 큐 0 선택 및 크기 조회
            outw(io_base + R_QUEUE_SELECT, 0);
            let qsize = inw(io_base + R_QUEUE_SIZE) as usize;
            if qsize == 0 {
                crate::serial_println!("[virtio-blk] queue size = 0");
                outb(io_base + R_STATUS, STATUS_FAILED);
                return None;
            }
            crate::serial_println!("[virtio-blk] queue size = {}", qsize);

            // 동적 레이아웃 계산
            let avail_off  = avail_off(qsize);
            let used_off   = used_off(qsize);
            let alloc_sz   = queue_alloc_size(qsize);
            crate::serial_println!(
                "[virtio-blk] layout: desc=0..{}, avail={}, used={}, total={}B",
                avail_off, avail_off, used_off, alloc_sz
            );

            // 큐 메모리 할당 (4KB 정렬)
            let layout = Layout::from_size_align(alloc_sz, 4096).unwrap();
            let queue  = alloc_zeroed(layout);
            if queue.is_null() {
                crate::serial_println!("[virtio-blk] queue alloc failed");
                outb(io_base + R_STATUS, STATUS_FAILED);
                return None;
            }

            // PFN 등록
            let virt  = queue as u64;
            let phys  = virt - crate::memory::hhdm_offset();
            let pfn   = (phys / 4096) as u32;
            outl(io_base + R_QUEUE_ADDRESS, pfn);
            crate::serial_println!("[virtio-blk] queue pfn={} (phys=0x{:x})", pfn, phys);

            // DRIVER_OK
            outb(io_base + R_STATUS, STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK);

            // 디바이스 용량
            let cap_lo   = inl(io_base + R_CAPACITY_LO) as u64;
            let cap_hi   = inl(io_base + R_CAPACITY_HI) as u64;
            let capacity = cap_lo | (cap_hi << 32);
            crate::serial_println!(
                "[virtio-blk] capacity = {} sectors ({} KB)",
                capacity, capacity / 2
            );

            Some(VirtioBlk {
                io_base, queue, qsize, avail_off, used_off,
                avail_idx: 0, last_used: 0, alloc_sz, capacity,
            })
        }
    }

    /// 섹터 읽기 (512 bytes)
    pub fn read_sector(&mut self, sector: u64, buf: &mut [u8; 512]) -> bool {
        self.do_request(BLK_T_IN, sector, buf)
    }

    /// 섹터 쓰기 (512 bytes)
    pub fn write_sector(&mut self, sector: u64, buf: &[u8; 512]) -> bool {
        let mut tmp = [0u8; 512];
        tmp.copy_from_slice(buf);
        self.do_request(BLK_T_OUT, sector, &mut tmp)
    }

    // ── 내부 요청 처리 ────────────────────────────────────────────────────────

    fn do_request(&mut self, type_: u32, sector: u64, buf: &mut [u8; 512]) -> bool {
        unsafe {
            let hhdm = crate::memory::hhdm_offset();

            // 요청 헤더 + 상태 바이트 (스택에 할당)
            let hdr    = BlkReqHdr { type_, reserved: 0, sector };
            let status: u8 = 0xFF;

            let hdr_phys    = (&hdr    as *const BlkReqHdr as u64).wrapping_sub(hhdm);
            let buf_phys    = (buf.as_ptr()                 as u64).wrapping_sub(hhdm);
            let status_phys = (&status as *const u8         as u64).wrapping_sub(hhdm);

            // ── 디스크립터 3개 (인덱스 0, 1, 2) ──────────────────────────────
            // Desc[0]: 헤더 (device reads)
            self.write_desc(0,
                hdr_phys,
                core::mem::size_of::<BlkReqHdr>() as u32,
                VIRTQ_DESC_F_NEXT, 1,
            );
            // Desc[1]: 데이터 (device writes for IN, reads for OUT)
            let data_flags = if type_ == BLK_T_IN {
                VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT
            } else {
                VIRTQ_DESC_F_NEXT
            };
            self.write_desc(1, buf_phys, 512, data_flags, 2);
            // Desc[2]: 상태 (device writes)
            self.write_desc(2, status_phys, 1, VIRTQ_DESC_F_WRITE, 0);

            // ── Available ring 갱신 ───────────────────────────────────────────
            let slot = (self.avail_idx as usize) % self.qsize;
            self.avail_ring_write(slot, 0); // desc chain 시작은 0번

            fence(Ordering::SeqCst);

            self.avail_idx = self.avail_idx.wrapping_add(1);
            self.avail_idx_write(self.avail_idx);

            fence(Ordering::SeqCst);

            // ── 큐 알림 ───────────────────────────────────────────────────────
            outw(self.io_base + R_QUEUE_NOTIFY, 0);

            // ── Used ring 폴링 (완료 대기, 최대 ~1초) ────────────────────────
            let deadline = crate::interrupts::handlers::TICK
                .load(Ordering::Relaxed) + 18;
            loop {
                fence(Ordering::SeqCst);
                if self.used_idx_read() != self.last_used {
                    self.last_used = self.last_used.wrapping_add(1);
                    break;
                }
                if crate::interrupts::handlers::TICK.load(Ordering::Relaxed) > deadline {
                    crate::serial_println!(
                        "[virtio-blk] timeout sector={}", sector
                    );
                    return false;
                }
                core::arch::asm!("pause", options(nomem, nostack));
            }

            status == 0
        }
    }

    // ── 큐 메모리 원시 접근 ───────────────────────────────────────────────────

    /// Descriptor Table에 항목 기록 (16 bytes per entry)
    unsafe fn write_desc(&self, idx: usize, addr: u64, len: u32, flags: u16, next: u16) {
        let base = self.queue as usize + idx * 16;
        (base as *mut u64).write_volatile(addr);
        ((base + 8) as *mut u32).write_volatile(len);
        ((base + 12) as *mut u16).write_volatile(flags);
        ((base + 14) as *mut u16).write_volatile(next);
    }

    /// Available ring의 ring[slot]에 desc_head 인덱스 기록
    unsafe fn avail_ring_write(&self, slot: usize, desc_head: u16) {
        // available ring layout: flags(u16) idx(u16) ring[qsize](u16) ...
        let ring_base = self.queue as usize + self.avail_off + 4; // flags(2)+idx(2)
        ((ring_base + slot * 2) as *mut u16).write_volatile(desc_head);
    }

    /// Available ring의 idx 필드 갱신
    unsafe fn avail_idx_write(&self, idx: u16) {
        let ptr = (self.queue as usize + self.avail_off + 2) as *mut u16;
        ptr.write_volatile(idx);
    }

    /// Used ring의 idx 필드 읽기
    unsafe fn used_idx_read(&self) -> u16 {
        let ptr = (self.queue as usize + self.used_off + 2) as *const u16;
        ptr.read_volatile()
    }
}

impl Drop for VirtioBlk {
    fn drop(&mut self) {
        unsafe {
            outb(self.io_base + R_STATUS, 0); // 디바이스 리셋
            let layout = Layout::from_size_align(self.alloc_sz, 4096).unwrap();
            dealloc(self.queue, layout);
        }
    }
}

// ── 포트 I/O ────────────────────────────────────────────────────────────────

#[inline] unsafe fn outb(p: u16, v: u8)  { pci::outb(p, v); }
#[inline] unsafe fn outw(p: u16, v: u16) { pci::outw(p, v); }
#[inline] unsafe fn outl(p: u16, v: u32) { pci::outl(p, v); }
#[inline] unsafe fn inw(p: u16) -> u16   { pci::inw(p) }
#[inline] unsafe fn inl(p: u16) -> u32   { pci::inl(p) }
