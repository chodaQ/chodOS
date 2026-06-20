/// ELF64 파서 — 정적 링크 실행 바이너리 전용 (ALPHA 14)
///
/// ## 지원 형식
/// - ELF64, little-endian, ET_EXEC (정적 실행 파일)
/// - PT_LOAD 세그먼트만 처리
///
/// ## 메모리 레이아웃
/// ```
/// ELF Header (64 bytes)
/// Program Header Table (N × 56 bytes)
/// Segments (LOAD 세그먼트 데이터)
/// ```

pub struct Elf64<'a> {
    data: &'a [u8],
}

impl<'a> Elf64<'a> {
    /// ELF64 헤더를 검증하고 파서 반환. 실패 시 None.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < 64 { return None; }
        if &data[0..4] != b"\x7FELF" { return None; }
        if data[4] != 2 { return None; }    // ELFCLASS64
        if data[5] != 1 { return None; }    // ELFDATA2LSB (little-endian)
        if u16le(data, 16) != 2 { return None; } // ET_EXEC
        if u16le(data, 18) != 0x3E { return None; } // EM_X86_64
        Some(Self { data })
    }

    /// ELF 진입점 가상 주소 (e_entry)
    pub fn entry(&self) -> u64 {
        u64le(self.data, 24)
    }

    /// PT_LOAD 세그먼트 이터레이터
    pub fn load_segments(&self) -> impl Iterator<Item = PtLoad> + '_ {
        let phoff = u64le(self.data, 32) as usize; // e_phoff
        let phnum = u16le(self.data, 56) as usize;  // e_phnum
        let phsz  = u16le(self.data, 54) as usize;  // e_phentsize (보통 56)

        (0..phnum).filter_map(move |i| {
            let base = phoff + i * phsz;
            if base + phsz > self.data.len() { return None; }
            if u32le(self.data, base) != 1 { return None; } // p_type != PT_LOAD
            Some(PtLoad {
                flags:  u32le(self.data, base + 4),
                offset: u64le(self.data, base + 8)  as usize,
                vaddr:  u64le(self.data, base + 16),
                filesz: u64le(self.data, base + 32) as usize,
                memsz:  u64le(self.data, base + 40) as usize,
            })
        })
    }
}

/// PT_LOAD 세그먼트 설명
pub struct PtLoad {
    pub flags:  u32,  // PF_R=4, PF_W=2, PF_X=1
    pub offset: usize, // 파일 내 오프셋
    pub vaddr:  u64,  // 매핑할 가상 주소
    pub filesz: usize, // 파일 크기 (실제 데이터)
    pub memsz:  usize, // 메모리 크기 (BSS 포함)
}

// ── 리틀엔디언 읽기 헬퍼 ────────────────────────────────────────────────────

fn u16le(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(d[off..off+2].try_into().unwrap())
}
fn u32le(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(d[off..off+4].try_into().unwrap())
}
fn u64le(d: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(d[off..off+8].try_into().unwrap())
}
