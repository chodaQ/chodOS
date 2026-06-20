//! Virtqueue 메모리 레이아웃 & 원시 접근 (blk + net 공용)
//!
//! ## 레이아웃 (qsize = N)
//!
//! ```text
//! [0       .. N*16):         Descriptor Table  (N × 16 bytes)
//! [N*16    .. ao+6+N*2):     Available Ring    (flags u16, idx u16, ring[N] u16)
//! [align4k ..):              Used Ring         (flags u16, idx u16, ring[N] {id u32, len u32})
//! ```

use alloc::alloc::{alloc_zeroed, dealloc, Layout};

// ── 오프셋 계산 ──────────────────────────────────────────────────────────────

pub fn avail_off(n: usize) -> usize { n * 16 }

pub fn used_off(n: usize) -> usize {
    let end = avail_off(n) + 6 + n * 2;
    (end + 4095) & !4095
}

pub fn alloc_size(n: usize) -> usize {
    let total = used_off(n) + 6 + n * 8;
    (total + 4095) & !4095
}

// ── 큐 핸들 ─────────────────────────────────────────────────────────────────

/// 단일 virtqueue 상태 (드라이버 쪽 카운터 + 메모리 포인터)
pub struct Queue {
    pub base:     *mut u8,
    pub qsize:    usize,
    pub ao:       usize, // avail ring offset
    pub uo:       usize, // used  ring offset
    pub alloc_sz: usize,
    pub ai:       u16,   // avail_idx (우리가 유지)
    pub lu:       u16,   // last_used (우리가 유지)
    pub nd:       u16,   // next free descriptor index
}

unsafe impl Send for Queue {}
unsafe impl Sync for Queue {}

impl Queue {
    /// `io_base` BAR0에서 `sel` 번 큐를 초기화하고 PFN을 디바이스에 등록.
    pub fn init(io_base: u16, sel: u16) -> Option<Self> {
        unsafe {
            use crate::pci;
            pci::outw(io_base + 0x0E, sel);
            let qsize = pci::inw(io_base + 0x0C) as usize;
            if qsize == 0 { return None; }

            let ao       = avail_off(qsize);
            let uo       = used_off(qsize);
            let alloc_sz = alloc_size(qsize);

            let layout = Layout::from_size_align(alloc_sz, 4096).unwrap();
            let base   = alloc_zeroed(layout);
            if base.is_null() { return None; }

            let phys = base as u64 - crate::memory::hhdm_offset();
            pci::outl(io_base + 0x08, (phys / 4096) as u32);

            Some(Queue { base, qsize, ao, uo, alloc_sz, ai: 0, lu: 0, nd: 0 })
        }
    }

    // ── Descriptor Table ───────────────────────────────────────────────────

    pub unsafe fn write_desc(
        &self, idx: usize,
        addr: u64, len: u32, flags: u16, next: u16,
    ) {
        let b = self.base as usize + idx * 16;
        (b as *mut u64).write_volatile(addr);
        ((b + 8)  as *mut u32).write_volatile(len);
        ((b + 12) as *mut u16).write_volatile(flags);
        ((b + 14) as *mut u16).write_volatile(next);
    }

    // ── Available Ring ─────────────────────────────────────────────────────

    /// avail ring[slot]에 desc 시작 인덱스 기록
    pub unsafe fn avail_put(&self, slot: usize, head: u16) {
        let ptr = (self.base as usize + self.ao + 4 + slot * 2) as *mut u16;
        ptr.write_volatile(head);
    }

    /// avail.idx 갱신 (디바이스가 이 값까지 처리)
    pub unsafe fn avail_commit(&self, idx: u16) {
        let ptr = (self.base as usize + self.ao + 2) as *mut u16;
        ptr.write_volatile(idx);
    }

    // ── Used Ring ─────────────────────────────────────────────────────────

    pub unsafe fn used_idx(&self) -> u16 {
        let ptr = (self.base as usize + self.uo + 2) as *const u16;
        ptr.read_volatile()
    }

    /// used ring[slot].id
    pub unsafe fn used_id(&self, slot: usize) -> u32 {
        let ptr = (self.base as usize + self.uo + 4 + slot * 8) as *const u32;
        ptr.read_volatile()
    }

    /// used ring[slot].len (디바이스가 썼거나 읽은 바이트 수)
    pub unsafe fn used_len(&self, slot: usize) -> u32 {
        let ptr = (self.base as usize + self.uo + 4 + slot * 8 + 4) as *const u32;
        ptr.read_volatile()
    }

    // ── 편의 함수 ──────────────────────────────────────────────────────────

    /// 다음 free descriptor 인덱스를 할당 (단순 순환)
    pub fn alloc_desc(&mut self) -> usize {
        let idx = self.nd as usize;
        self.nd = self.nd.wrapping_add(1) % self.qsize as u16;
        idx
    }

    /// 1-descriptor 항목을 avail ring에 추가하고 ai 갱신
    pub unsafe fn push_one(&mut self, addr: u64, len: u32, flags: u16) -> u16 {
        let d = self.alloc_desc();
        self.write_desc(d, addr, len, flags, 0);
        let slot = self.ai as usize % self.qsize;
        self.avail_put(slot, d as u16);
        self.ai = self.ai.wrapping_add(1);
        self.avail_commit(self.ai);
        d as u16
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        if !self.base.is_null() {
            unsafe {
                let layout = Layout::from_size_align(self.alloc_sz, 4096).unwrap();
                dealloc(self.base, layout);
            }
            self.base = core::ptr::null_mut();
        }
    }
}
