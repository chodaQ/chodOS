/// ELF64 파서 — BETA 19: ET_EXEC + ET_DYN (.so) 지원
///
/// ## 지원 형식
/// - ELF64, little-endian, ET_EXEC (정적 실행 파일) / ET_DYN (공유 라이브러리)
/// - PT_LOAD, PT_DYNAMIC, PT_INTERP 세그먼트 처리
/// - SHT_RELA / SHT_DYNSYM 파싱 (재배치, 심볼 테이블)

// ── ELF 타입 상수 ────────────────────────────────────────────────────────────

pub const ET_EXEC: u16 = 2;
pub const ET_DYN:  u16 = 3;

pub const PT_LOAD:    u32 = 1;
pub const PT_DYNAMIC: u32 = 2;
pub const PT_INTERP:  u32 = 3;

/// DT_* 태그
pub const DT_NULL:         i64 = 0;
pub const DT_NEEDED:       i64 = 1;
pub const DT_PLTRELSZ:     i64 = 2;
pub const DT_PLTGOT:       i64 = 3;
pub const DT_SYMTAB:       i64 = 6;
pub const DT_STRTAB:       i64 = 5;
pub const DT_STRSZ:        i64 = 10;
pub const DT_SYMENT:       i64 = 11;
pub const DT_INIT:         i64 = 12;
pub const DT_FINI:         i64 = 13;
pub const DT_RELA:         i64 = 7;
pub const DT_RELASZ:       i64 = 8;
pub const DT_RELAENT:      i64 = 9;
pub const DT_REL:          i64 = 17;
pub const DT_RELSZ:        i64 = 18;
pub const DT_RELENT:       i64 = 19;
pub const DT_PLTREL:       i64 = 20;
pub const DT_JMPREL:       i64 = 23;
pub const DT_INIT_ARRAY:   i64 = 25;
pub const DT_FINI_ARRAY:   i64 = 26;
pub const DT_INIT_ARRAYSZ: i64 = 27;
pub const DT_FINI_ARRAYSZ: i64 = 28;

/// x86_64 재배치 타입
pub const R_X86_64_64:        u32 = 1;
pub const R_X86_64_COPY:      u32 = 5;
pub const R_X86_64_GLOB_DAT:  u32 = 6;
pub const R_X86_64_JUMP_SLOT: u32 = 7;
pub const R_X86_64_RELATIVE:  u32 = 8;

// ── Elf64 파서 ───────────────────────────────────────────────────────────────

pub struct Elf64<'a> {
    pub data: &'a [u8],
    pub elf_type: u16,
}

impl<'a> Elf64<'a> {
    /// ELF64 헤더를 검증하고 파서 반환. ET_EXEC 또는 ET_DYN 모두 허용.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < 64 { return None; }
        if &data[0..4] != b"\x7FELF" { return None; }
        if data[4] != 2 { return None; }    // ELFCLASS64
        if data[5] != 1 { return None; }    // ELFDATA2LSB (little-endian)
        let elf_type = u16le(data, 16);
        if elf_type != ET_EXEC && elf_type != ET_DYN { return None; }
        if u16le(data, 18) != 0x3E { return None; } // EM_X86_64
        Some(Self { data, elf_type })
    }

    /// ELF 진입점 가상 주소 (e_entry)
    pub fn entry(&self) -> u64 {
        u64le(self.data, 24)
    }

    /// PT_LOAD 세그먼트 이터레이터
    pub fn load_segments(&self) -> impl Iterator<Item = PtLoad> + '_ {
        self.phdr_iter().filter_map(|ph| {
            if ph.p_type != PT_LOAD { return None; }
            Some(PtLoad {
                flags:  ph.p_flags,
                offset: ph.p_offset as usize,
                vaddr:  ph.p_vaddr,
                filesz: ph.p_filesz as usize,
                memsz:  ph.p_memsz as usize,
                align:  ph.p_align,
            })
        })
    }

    /// PT_DYNAMIC 세그먼트 위치 반환 (파일 오프셋, 크기)
    pub fn dynamic_segment(&self) -> Option<(usize, usize)> {
        self.phdr_iter().find_map(|ph| {
            if ph.p_type == PT_DYNAMIC {
                Some((ph.p_offset as usize, ph.p_filesz as usize))
            } else {
                None
            }
        })
    }

    /// PT_INTERP 세그먼트에서 인터프리터 경로 파싱
    pub fn interp(&self) -> Option<&'a [u8]> {
        self.phdr_iter().find_map(|ph| {
            if ph.p_type != PT_INTERP { return None; }
            let off = ph.p_offset as usize;
            let sz  = ph.p_filesz as usize;
            if off + sz > self.data.len() { return None; }
            let s = &self.data[off..off + sz];
            // null 종단 문자 제거
            Some(s.split(|&b| b == 0).next().unwrap_or(s))
        })
    }

    /// PT_DYNAMIC 파싱 — (d_tag, d_val) 이터레이터
    ///
    /// load_base: ET_DYN일 때 세그먼트가 실제 로드된 가상 주소 오프셋.
    /// 파일 오프셋 기반 파싱이므로 load_base는 여기서 사용 안 함.
    pub fn dynamic_entries(&self) -> impl Iterator<Item = DynEntry> + '_ {
        let (off, sz) = match self.dynamic_segment() {
            Some(x) => x,
            None    => return DynIter { data: self.data, off: 0, end: 0 },
        };
        let end = (off + sz).min(self.data.len());
        DynIter { data: self.data, off, end }
    }

    /// DT_STRTAB 오프셋에서 null-terminated 문자열 반환.
    ///
    /// strtab_file_off: DT_STRTAB의 가상 주소를 파일 오프셋으로 변환한 값.
    pub fn dyn_str(&self, strtab_file_off: usize, idx: usize) -> Option<&'a [u8]> {
        let start = strtab_file_off + idx;
        if start >= self.data.len() { return None; }
        let end = self.data[start..].iter().position(|&b| b == 0)
            .map(|p| start + p)
            .unwrap_or(self.data.len());
        Some(&self.data[start..end])
    }

    /// Rela 재배치 테이블 파싱 (파일 오프셋 기반)
    pub fn rela_entries(&self, file_off: usize, total_sz: usize) -> impl Iterator<Item = Elf64Rela> + '_ {
        let end = (file_off + total_sz).min(self.data.len());
        RelaIter { data: self.data, off: file_off, end }
    }

    /// 심볼 테이블 항목 반환
    pub fn sym(&self, symtab_file_off: usize, sym_idx: usize) -> Option<Elf64Sym> {
        const SYMENT: usize = 24;
        let off = symtab_file_off + sym_idx * SYMENT;
        if off + SYMENT > self.data.len() { return None; }
        Some(Elf64Sym {
            st_name:  u32le(self.data, off),
            st_info:  self.data[off + 4],
            st_other: self.data[off + 5],
            st_shndx: u16le(self.data, off + 6),
            st_value: u64le(self.data, off + 8),
            st_size:  u64le(self.data, off + 16),
        })
    }

    /// 심볼 테이블 전체 순회 — BETA 20 SymbolMap 구성에 사용.
    ///
    /// symtab_foff ~ strtab_foff 범위를 24바이트씩 나눠 심볼 수를 추정.
    pub fn dynsym_all<'b>(
        data: &'b [u8],
        symtab_foff: usize,
        strtab_foff: usize,
    ) -> impl Iterator<Item = (usize, Elf64Sym)> + 'b {
        const SYMENT: usize = 24;
        let count = if strtab_foff > symtab_foff {
            (strtab_foff - symtab_foff) / SYMENT
        } else {
            0
        };
        (0..count).filter_map(move |i| {
            let off = symtab_foff + i * SYMENT;
            if off + SYMENT > data.len() { return None; }
            Some((i, Elf64Sym {
                st_name:  u32le(data, off),
                st_info:  data[off + 4],
                st_other: data[off + 5],
                st_shndx: u16le(data, off + 6),
                st_value: u64le(data, off + 8),
                st_size:  u64le(data, off + 16),
            }))
        })
    }

    /// 가상 주소 → 파일 오프셋 변환 (PT_LOAD 세그먼트 기반).
    ///
    /// ET_DYN의 DT_SYMTAB/DT_STRTAB/DT_RELA 값은 파일 내 가상 주소이므로,
    /// 대응하는 PT_LOAD 세그먼트에서 오프셋을 역산해야 함.
    pub fn vaddr_to_file_off(&self, vaddr: u64) -> Option<usize> {
        self.phdr_iter().find_map(|ph| {
            if ph.p_type != PT_LOAD { return None; }
            if vaddr < ph.p_vaddr || vaddr >= ph.p_vaddr + ph.p_filesz { return None; }
            Some((ph.p_offset + (vaddr - ph.p_vaddr)) as usize)
        })
    }

    // ── 프로그램 헤더 raw 이터레이터 ─────────────────────────────────────────

    fn phdr_iter(&self) -> PhdrIter<'_> {
        let phoff = u64le(self.data, 32) as usize;
        let phnum = u16le(self.data, 56) as usize;
        let phsz  = u16le(self.data, 54) as usize;
        PhdrIter { data: self.data, phoff, phsz, phnum, idx: 0 }
    }
}

// ── 구조체 ───────────────────────────────────────────────────────────────────

/// PT_LOAD 세그먼트 설명
pub struct PtLoad {
    pub flags:  u32,
    pub offset: usize,
    pub vaddr:  u64,
    pub filesz: usize,
    pub memsz:  usize,
    pub align:  u64,
}

/// 동적 섹션 엔트리 (d_tag, d_val/d_ptr)
#[derive(Clone, Copy, Debug)]
pub struct DynEntry {
    pub tag: i64,
    pub val: u64,
}

/// Elf64_Rela — 재배치 엔트리 (addend 포함)
#[derive(Clone, Copy, Debug)]
pub struct Elf64Rela {
    pub r_offset: u64,  // 재배치 대상 가상 주소
    pub r_sym:    u32,  // 심볼 인덱스
    pub r_type:   u32,  // 재배치 타입 (R_X86_64_*)
    pub r_addend: i64,  // 덧셈 상수
}

/// Elf64_Sym — 심볼 테이블 엔트리
#[derive(Clone, Copy, Debug)]
pub struct Elf64Sym {
    pub st_name:  u32,
    pub st_info:  u8,
    pub st_other: u8,
    pub st_shndx: u16,  // SHN_UNDEF = 0
    pub st_value: u64,
    pub st_size:  u64,
}

impl Elf64Sym {
    /// 정의되지 않은 심볼 (SHN_UNDEF)
    pub fn is_undef(&self) -> bool { self.st_shndx == 0 }
}

// ── raw 프로그램 헤더 ─────────────────────────────────────────────────────────

struct RawPhdr {
    p_type:   u32,
    p_flags:  u32,
    p_offset: u64,
    p_vaddr:  u64,
    _p_paddr: u64,
    p_filesz: u64,
    p_memsz:  u64,
    p_align:  u64,
}

struct PhdrIter<'a> {
    data:  &'a [u8],
    phoff: usize,
    phsz:  usize,
    phnum: usize,
    idx:   usize,
}

impl<'a> Iterator for PhdrIter<'a> {
    type Item = RawPhdr;
    fn next(&mut self) -> Option<RawPhdr> {
        if self.idx >= self.phnum { return None; }
        let base = self.phoff + self.idx * self.phsz;
        if base + self.phsz > self.data.len() { return None; }
        self.idx += 1;
        let d = self.data;
        Some(RawPhdr {
            p_type:   u32le(d, base),
            p_flags:  u32le(d, base + 4),
            p_offset: u64le(d, base + 8),
            p_vaddr:  u64le(d, base + 16),
            _p_paddr: u64le(d, base + 24),
            p_filesz: u64le(d, base + 32),
            p_memsz:  u64le(d, base + 40),
            p_align:  u64le(d, base + 48),
        })
    }
}

// ── 동적 섹션 이터레이터 ──────────────────────────────────────────────────────

struct DynIter<'a> {
    data: &'a [u8],
    off:  usize,
    end:  usize,
}

impl<'a> Iterator for DynIter<'a> {
    type Item = DynEntry;
    fn next(&mut self) -> Option<DynEntry> {
        if self.off + 16 > self.end { return None; }
        let tag = i64le(self.data, self.off);
        let val = u64le(self.data, self.off + 8);
        self.off += 16;
        if tag == DT_NULL { return None; }
        Some(DynEntry { tag, val })
    }
}

// ── Rela 이터레이터 ──────────────────────────────────────────────────────────

struct RelaIter<'a> {
    data: &'a [u8],
    off:  usize,
    end:  usize,
}

impl<'a> Iterator for RelaIter<'a> {
    type Item = Elf64Rela;
    fn next(&mut self) -> Option<Elf64Rela> {
        if self.off + 24 > self.end { return None; }
        let r_offset = u64le(self.data, self.off);
        let r_info   = u64le(self.data, self.off + 8);
        let r_addend = i64le(self.data, self.off + 16);
        self.off += 24;
        Some(Elf64Rela {
            r_offset,
            r_sym:  (r_info >> 32) as u32,
            r_type: (r_info & 0xFFFF_FFFF) as u32,
            r_addend,
        })
    }
}

// ── 리틀엔디언 읽기 헬퍼 ─────────────────────────────────────────────────────

pub fn u16le(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(d[off..off+2].try_into().unwrap())
}
pub fn u32le(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(d[off..off+4].try_into().unwrap())
}
pub fn u64le(d: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(d[off..off+8].try_into().unwrap())
}
fn i64le(d: &[u8], off: usize) -> i64 {
    i64::from_le_bytes(d[off..off+8].try_into().unwrap())
}
