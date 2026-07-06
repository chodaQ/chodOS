//! BETA 19~20: ELF .so 파서 & 재배치 엔진 + 동적 링커 (`ld-musl` 호환)
//!
//! ## 역할
//!
//! **BETA 19 (완료):**
//! - ET_DYN ELF(.so)를 유저 주소 공간의 지정된 base에 로드
//! - DT_NEEDED 목록 수집
//! - R_X86_64_RELATIVE / _64 / _GLOB_DAT / _JUMP_SLOT 재배치 적용
//!
//! **BETA 20 (이번 구현):**
//! - `SymbolMap`: 다중 라이브러리 심볼 이름 → VA 역방향 테이블
//! - `DynLinker`: 실행 파일 + 의존 .so 재귀 로드 + 전체 재배치 적용 파이프라인
//! - PT_INTERP 감지 → 인터프리터 로드 + 진입점 교체
//! - DT_INIT_ARRAY 초기화 주소 수집 (유저스페이스 트램펄린으로 실행)
//! - R_X86_64_COPY 구현 (cross-lib 지원 완료 후)
//!
//! ## 설계 원칙 (CLAUDE.md §4)
//!
//! "생성은 엄격하게, 사용은 가볍게":
//! - 로드 시점(load_so/DynLinker::load_exec): 검증 엄격
//! - 재배치 시점(apply_rela_with_map): HHDM 직접 쓰기, 검증 없음

use alloc::{vec::Vec, string::String};
use crate::elf::{
    self, Elf64, DynEntry,
    ET_DYN,
    DT_NEEDED, DT_SYMTAB, DT_STRTAB,
    DT_RELA, DT_RELASZ,
    DT_JMPREL, DT_PLTRELSZ,
    DT_INIT_ARRAY, DT_INIT_ARRAYSZ,
    R_X86_64_64, R_X86_64_COPY, R_X86_64_GLOB_DAT,
    R_X86_64_JUMP_SLOT, R_X86_64_RELATIVE,
};
use crate::memory::hhdm_offset;

// ── 공개 상수 ─────────────────────────────────────────────────────────────────

/// PIE 실행 파일의 로드 베이스 (ET_DYN exec)
pub const EXEC_LOAD_BASE: u64 = 0x0000_0000_0040_0000; // 4MB

/// .so 파일의 기본 로드 시작 주소 (2GB; ELF 실행 파일과 겹치지 않음)
pub const SO_LOAD_BASE: u64 = 0x0000_0000_8000_0000;

/// 각 .so에 할당되는 최대 VA 슬롯 크기 (16MB)
pub const SO_SLOT_SIZE: u64 = 0x0100_0000;

/// 인터프리터(.so 동적 링커)의 로드 베이스 (3GB)
pub const INTERP_LOAD_BASE: u64 = 0x0000_0000_C000_0000;

// ── SharedLib ────────────────────────────────────────────────────────────────

/// 로드된 공유 라이브러리 하나를 나타내는 구조체.
pub struct SharedLib {
    pub name:          String,
    pub load_base:     u64,
    pub symtab_foff:   usize,
    pub strtab_foff:   usize,
    pub rela_foff:     usize,
    pub rela_sz:       usize,
    pub plt_rela_foff: usize,
    pub plt_rela_sz:   usize,
    pub init_array:    Vec<u64>,
    pub needed:        Vec<String>,
    pub vaddr_end:     u64,
}

// ── SymbolMap (BETA 20) ───────────────────────────────────────────────────────

/// 심볼 이름 → VA 다중 라이브러리 역방향 테이블.
///
/// DynLinker가 모든 라이브러리를 로드한 뒤 구성하고,
/// `apply_rela_with_map()`의 resolver로 전달한다.
pub struct SymbolMap {
    // (이름, 주소) 쌍. 이름이 겹치면 먼저 들어온 것(로드 순) 우선.
    entries: Vec<(String, u64)>,
}

impl SymbolMap {
    fn new() -> Self { Self { entries: Vec::new() } }

    /// 이름을 먼저 검색하고 없으면 추가.
    fn define(&mut self, name: String, va: u64) {
        if self.lookup(name.as_bytes()).is_none() {
            self.entries.push((name, va));
        }
    }

    /// 이름으로 VA 조회.
    pub fn lookup(&self, name: &[u8]) -> Option<u64> {
        self.entries.iter().find_map(|(n, va)| {
            if n.as_bytes() == name { Some(*va) } else { None }
        })
    }

    /// 라이브러리의 정의된 심볼 전체를 맵에 추가.
    pub fn add_lib(&mut self, lib: &SharedLib, elf_data: &[u8]) {
        for (_idx, sym) in Elf64::dynsym_all(elf_data, lib.symtab_foff, lib.strtab_foff) {
            if sym.is_undef() { continue; }
            if sym.st_value == 0 { continue; }
            let name_bytes = match elf_data.get(lib.strtab_foff + sym.st_name as usize..) {
                Some(s) => {
                    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
                    &s[..end]
                }
                None => continue,
            };
            if let Ok(name_str) = core::str::from_utf8(name_bytes) {
                let va = lib.load_base + sym.st_value;
                self.define(String::from(name_str), va);
            }
        }
    }
}

// ── DynLinker (BETA 20) ───────────────────────────────────────────────────────

/// 동적 링킹 파이프라인.
///
/// ## 사용 방법
///
/// ```rust
/// let mut dl = DynLinker::new(cr3);
/// let entry = dl.load_exec(exec_data, |name| vfs_read_lib(name))?;
/// let fails = dl.resolve_all();
/// // entry로 iretq
/// ```
pub struct DynLinker {
    libs:      Vec<SharedLib>,
    elf_bufs:  Vec<Vec<u8>>,   // libs[i]에 대응하는 원본 ELF 데이터
    cr3:       u64,
    next_slot: u64,            // 다음 .so 로드 시작 주소
    // AT_* aux vector 정보
    pub exec_phdr_va:   u64,
    pub exec_phent:     u16,
    pub exec_phnum:     u16,
    pub exec_load_base: u64,   // PIE면 EXEC_LOAD_BASE, ET_EXEC면 0
    pub exec_entry:     u64,   // 실행 파일 원래 진입점
    pub interp_entry:   u64,   // 인터프리터 진입점 (0이면 없음)
    pub exec_vaddr_end: u64,   // exec의 BSS end (brk base 초기값)
}

impl DynLinker {
    pub fn new(cr3: u64) -> Self {
        DynLinker {
            libs:           Vec::new(),
            elf_bufs:       Vec::new(),
            cr3,
            next_slot:      SO_LOAD_BASE,
            exec_phdr_va:   0,
            exec_phent:     0,
            exec_phnum:     0,
            exec_load_base: 0,
            exec_entry:     0,
            interp_entry:   0,
            exec_vaddr_end: 0,
        }
    }

    /// 실행 파일 + 모든 DT_NEEDED를 재귀 로드.
    ///
    /// `lib_provider`: 라이브러리 이름("libc.so") → ELF 바이트 슬라이스 반환 클로저.
    /// 없는 라이브러리는 경고 후 스킵.
    ///
    /// 반환: 최종 진입점 VA (인터프리터 있으면 인터프리터 진입점).
    pub fn load_exec<F>(&mut self, exec_data: &[u8], lib_provider: F) -> Option<u64>
    where
        F: Fn(&str) -> Option<Vec<u8>>,
    {
        let elf = Elf64::parse(exec_data)?;

        // ── 실행 파일 로드 ────────────────────────────────────────────────────
        let (_exec_base, exec_entry_va) = if elf.elf_type == ET_DYN {
            // PIE 실행 파일 — EXEC_LOAD_BASE에 로드
            let base = EXEC_LOAD_BASE;
            let lib = load_so_at("(exec)", exec_data, self.cr3, base)?;
            let entry = base + elf.entry();
            self.exec_load_base = base;
            self.exec_entry     = entry;
            self.exec_phdr_va   = base + u64le_hdr(exec_data, 32); // e_phoff
            self.exec_phent     = u16le_hdr(exec_data, 54);
            self.exec_phnum     = u16le_hdr(exec_data, 56);
            let base_out = base;
            self.exec_vaddr_end = lib.vaddr_end;
            self.libs.push(lib);
            self.elf_bufs.push(exec_data.to_vec());
            (base_out, entry)
        } else {
            // 고정 주소 ET_EXEC + PT_INTERP — PT_LOAD를 절대 vaddr에 매핑
            let base = 0u64;
            let lib = load_so_at("(exec)", exec_data, self.cr3, base)?;
            let entry = elf.entry();
            self.exec_entry = entry;
            // ET_EXEC: exec_phdr_va = PT_LOAD 가상 주소 + (e_phoff - PT_LOAD.p_offset)
            // (파일 오프셋 e_phoff를 직접 쓰면 잘못된 주소가 AT_PHDR에 들어감)
            let e_phoff = u64le_hdr(exec_data, 32);
            self.exec_phdr_va = elf.load_segments()
                .find(|s| s.offset as u64 <= e_phoff
                       && e_phoff < s.offset as u64 + s.filesz as u64)
                .map(|s| s.vaddr + e_phoff - s.offset as u64)
                .unwrap_or(e_phoff);   // fallback: 파일 오프셋 그대로
            self.exec_phent   = u16le_hdr(exec_data, 54);
            self.exec_phnum   = u16le_hdr(exec_data, 56);
            self.exec_load_base = 0;   // ET_EXEC는 슬라이드 없음 (AT_BASE = 0)
            self.exec_vaddr_end = lib.vaddr_end;
            self.libs.push(lib);
            self.elf_bufs.push(exec_data.to_vec());
            (base, entry)
        };

        // ── PT_INTERP 처리 ────────────────────────────────────────────────────
        if let Some(interp_path) = elf.interp() {
            let path_str = core::str::from_utf8(interp_path).unwrap_or("");
            crate::serial_println!("[dynlink] PT_INTERP 감지: {}", path_str);

            // 파일 이름만 추출 (경로 마지막 '/' 이후)
            let lib_name = path_str.rsplit('/').next().unwrap_or(path_str);

            if let Some(interp_data) = lib_provider(lib_name) {
                let interp_elf = Elf64::parse(&interp_data)?;
                let interp_base = INTERP_LOAD_BASE;
                let lib = load_so_at(lib_name, &interp_data, self.cr3, interp_base)?;
                let interp_entry = interp_base + interp_elf.entry();
                self.interp_entry = interp_entry;
                self.libs.push(lib);
                self.elf_bufs.push(interp_data);
                crate::serial_println!(
                    "[dynlink] 인터프리터 로드: base={:#x} entry={:#x}",
                    interp_base, interp_entry
                );
            } else {
                crate::serial_println!("[dynlink] 인터프리터 파일 없음: {}", lib_name);
            }
        }

        // ── DT_NEEDED 재귀 로드 ───────────────────────────────────────────────
        let needed: Vec<String> = self.libs[0].needed.clone();
        self.load_deps(&needed, &lib_provider);

        // ── 진입점 결정 ───────────────────────────────────────────────────────
        let entry = if self.interp_entry != 0 {
            self.interp_entry
        } else {
            exec_entry_va
        };

        crate::serial_println!(
            "[dynlink] load_exec 완료: libs={} entry={:#x}",
            self.libs.len(), entry
        );
        Some(entry)
    }

    /// DT_NEEDED 의존성 재귀 로드 (이미 로드된 라이브러리는 스킵).
    fn load_deps<F>(&mut self, needed: &[String], lib_provider: &F)
    where
        F: Fn(&str) -> Option<Vec<u8>>,
    {
        for dep_name in needed {
            // 이미 로드된 경우 스킵
            if self.libs.iter().any(|l| &l.name == dep_name) { continue; }

            let lib_name_only = dep_name.rsplit('/').next().unwrap_or(dep_name.as_str());
            match lib_provider(lib_name_only) {
                None => {
                    crate::serial_println!("[dynlink] 라이브러리 없음: {}", dep_name);
                }
                Some(data) => {
                    let slot = self.next_slot;
                    self.next_slot += SO_SLOT_SIZE;
                    match load_so_at(lib_name_only, &data, self.cr3, slot) {
                        None => {
                            crate::serial_println!("[dynlink] .so 로드 실패: {}", dep_name);
                        }
                        Some(lib) => {
                            let sub_needed = lib.needed.clone();
                            self.libs.push(lib);
                            self.elf_bufs.push(data);
                            // 재귀 의존성 로드
                            self.load_deps(&sub_needed, lib_provider);
                        }
                    }
                }
            }
        }
    }

    /// 모든 라이브러리의 재배치를 적용 (크로스 라이브러리 심볼 해석 포함).
    ///
    /// 1. 모든 라이브러리에서 정의된 심볼 → SymbolMap 구성
    /// 2. 각 라이브러리의 RELA + PLT RELA 재배치 적용
    ///
    /// 반환: 총 실패 재배치 수
    pub fn resolve_all(&self) -> usize {
        // ── 심볼 맵 구성 ─────────────────────────────────────────────────────
        let mut sym_map = SymbolMap::new();
        for (lib, data) in self.libs.iter().zip(self.elf_bufs.iter()) {
            sym_map.add_lib(lib, data);
        }

        // ── 각 라이브러리 재배치 적용 ────────────────────────────────────────
        let mut total_failed = 0usize;
        for (lib, data) in self.libs.iter().zip(self.elf_bufs.iter()) {
            let failed = apply_rela_with_map(lib, data, self.cr3, &sym_map);
            total_failed += failed;
        }

        crate::serial_println!(
            "[dynlink] resolve_all 완료: 총 실패 {}건", total_failed
        );
        total_failed
    }

    /// 최종 진입점 반환.
    pub fn entry_point(&self) -> u64 {
        if self.interp_entry != 0 { self.interp_entry } else { self.exec_entry }
    }

    /// DT_INIT_ARRAY 주소 목록 (모든 라이브러리, 의존성 역순으로 정렬).
    ///
    /// 유저스페이스 트램펄린에서 순서대로 호출해야 한다.
    pub fn collect_init_arrays(&self) -> Vec<u64> {
        let mut result: Vec<u64> = Vec::new();
        // 의존성 역순 (마지막 로드된 라이브러리부터)
        for lib in self.libs.iter().rev() {
            for &addr in &lib.init_array {
                result.push(addr);
            }
        }
        result
    }
}

// ── load_so 공개 API (BETA 19 호환) ──────────────────────────────────────────

/// ET_DYN ELF 파일을 `cr3` 주소 공간의 `slot_base`에 로드.
///
/// DynLinker 없이 단독으로도 쓸 수 있는 BETA 19 API.
pub fn load_so(name: &str, data: &[u8], cr3: u64, slot_base: u64) -> Option<SharedLib> {
    load_so_at(name, data, cr3, slot_base)
}

// ── 내부 로드 함수 ────────────────────────────────────────────────────────────

fn load_so_at(name: &str, data: &[u8], cr3: u64, slot_base: u64) -> Option<SharedLib> {
    let elf = Elf64::parse(data)?;
    // ET_DYN + ET_EXEC 모두 허용: slot_base=0 이면 vaddr 그대로(ET_EXEC),
    // slot_base>0 이면 PIE/SO 슬라이드(ET_DYN)
    if elf.elf_type != elf::ET_DYN && elf.elf_type != elf::ET_EXEC {
        crate::serial_println!("[dynlink] {} : 지원하지 않는 ELF 타입", name);
        return None;
    }

    // ── PT_LOAD 세그먼트 매핑 ────────────────────────────────────────────────
    let mut vaddr_end: u64 = slot_base;

    for seg in elf.load_segments() {
        let mapped_vaddr = slot_base + seg.vaddr;
        let page_start   = mapped_vaddr & !0xFFF;
        let page_end     = (mapped_vaddr + seg.memsz as u64 + 0xFFF) & !0xFFF;
        let pages        = ((page_end - page_start) / 4096) as usize;
        let writable     = seg.flags & 2 != 0;

        for i in 0..pages {
            let va   = page_start + i as u64 * 4096;
            let phys = crate::memory::frame::alloc_frame().expect("OOM: load_so_at PT_LOAD");
            let virt = (phys + hhdm_offset()) as *mut u8;
            unsafe { core::ptr::write_bytes(virt, 0, 4096); }

            let seg_start_va = mapped_vaddr;
            let page_va_end  = va + 4096;
            let copy_lo = seg_start_va.max(va);
            let copy_hi = (seg_start_va + seg.filesz as u64).min(page_va_end);

            if copy_lo < copy_hi {
                let dst_off = (copy_lo - va) as usize;
                let src_off = seg.offset + (copy_lo - seg_start_va) as usize;
                let n       = (copy_hi - copy_lo) as usize;
                if src_off + n <= data.len() {
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            data.as_ptr().add(src_off),
                            virt.add(dst_off),
                            n,
                        );
                    }
                }
            }

            unsafe { crate::paging::pub_map_4k(cr3, va, phys, writable); }
        }

        if page_end > vaddr_end { vaddr_end = page_end; }

        crate::serial_println!(
            "[dynlink] {} PT_LOAD: slot_va={:#x} memsz={:#x} pf={:#x}",
            name, mapped_vaddr, seg.memsz, seg.flags
        );
    }

    // ── PT_DYNAMIC 파싱 ──────────────────────────────────────────────────────
    let mut symtab_va:   u64 = 0;
    let mut strtab_va:   u64 = 0;
    let mut rela_va:     u64 = 0;
    let mut rela_sz:     u64 = 0;
    let mut jmprel_va:   u64 = 0;
    let mut pltrelsz:    u64 = 0;
    let mut init_arr_va: u64 = 0;
    let mut init_arr_sz: u64 = 0;
    let mut needed_idxs: Vec<u64> = Vec::new();

    for DynEntry { tag, val } in elf.dynamic_entries() {
        match tag {
            t if t == DT_SYMTAB       => symtab_va   = val,
            t if t == DT_STRTAB       => strtab_va   = val,
            t if t == DT_RELA         => rela_va      = val,
            t if t == DT_RELASZ       => rela_sz      = val,
            t if t == DT_JMPREL       => jmprel_va    = val,
            t if t == DT_PLTRELSZ     => pltrelsz     = val,
            t if t == DT_INIT_ARRAY   => init_arr_va  = val,
            t if t == DT_INIT_ARRAYSZ => init_arr_sz  = val,
            t if t == DT_NEEDED       => { needed_idxs.push(val); }
            _ => {}
        }
    }

    let symtab_foff   = elf.vaddr_to_file_off(symtab_va).unwrap_or(0);
    let strtab_foff   = elf.vaddr_to_file_off(strtab_va).unwrap_or(0);
    let rela_foff     = elf.vaddr_to_file_off(rela_va).unwrap_or(0);
    let plt_rela_foff = elf.vaddr_to_file_off(jmprel_va).unwrap_or(0);

    // ── DT_NEEDED 문자열 수집 ────────────────────────────────────────────────
    let mut needed: Vec<String> = Vec::new();
    for idx in needed_idxs {
        if let Some(s) = elf.dyn_str(strtab_foff, idx as usize) {
            if let Ok(s) = core::str::from_utf8(s) {
                needed.push(String::from(s));
            }
        }
    }

    // ── DT_INIT_ARRAY 수집 ───────────────────────────────────────────────────
    let mut init_array: Vec<u64> = Vec::new();
    if init_arr_va != 0 && init_arr_sz > 0 {
        let n = (init_arr_sz / 8) as usize;
        if let Some(foff) = elf.vaddr_to_file_off(init_arr_va) {
            for i in 0..n {
                let off = foff + i * 8;
                if off + 8 <= data.len() {
                    let fn_va = elf::u64le(data, off);
                    init_array.push(slot_base + fn_va);
                }
            }
        }
    }

    crate::serial_println!(
        "[dynlink] {} 로드 완료: base={:#x}..{:#x} needed={:?}",
        name, slot_base, vaddr_end, needed
    );

    Some(SharedLib {
        name:          String::from(name),
        load_base:     slot_base,
        symtab_foff,
        strtab_foff,
        rela_foff,
        rela_sz:       rela_sz as usize,
        plt_rela_foff,
        plt_rela_sz:   pltrelsz as usize,
        init_array,
        needed,
        vaddr_end,
    })
}

// ── 재배치 API ───────────────────────────────────────────────────────────────

/// BETA 19 호환 단일 라이브러리 재배치 콜백 타입.
pub type SymbolResolver<'a> = &'a dyn Fn(&[u8]) -> Option<u64>;

/// `lib`의 RELA + PLT RELA 재배치 적용 (단순 콜백 버전 — BETA 19 호환).
pub fn apply_rela(
    lib:      &SharedLib,
    elf_data: &[u8],
    cr3:      u64,
    resolver: SymbolResolver<'_>,
) -> usize {
    apply_rela_impl(lib, elf_data, cr3, |name| resolver(name))
}

/// `lib`의 RELA + PLT RELA 재배치 적용 (SymbolMap 버전 — BETA 20).
pub fn apply_rela_with_map(
    lib:     &SharedLib,
    elf_data: &[u8],
    cr3:      u64,
    sym_map:  &SymbolMap,
) -> usize {
    apply_rela_impl(lib, elf_data, cr3, |name| sym_map.lookup(name))
}

fn apply_rela_impl<F>(
    lib:      &SharedLib,
    elf_data: &[u8],
    cr3:      u64,
    resolver: F,
) -> usize
where
    F: Fn(&[u8]) -> Option<u64>,
{
    let elf = match Elf64::parse(elf_data) {
        Some(e) => e,
        None    => return 0,
    };

    let mut failed = 0usize;

    let sections: [(usize, usize); 2] = [
        (lib.rela_foff,     lib.rela_sz),
        (lib.plt_rela_foff, lib.plt_rela_sz),
    ];

    for (foff, sz) in sections {
        if sz == 0 { continue; }

        for rela in elf.rela_entries(foff, sz) {
            // ET_DYN: r_offset는 파일 내 VA (0 기반) → load_base 더해야 함
            // ET_EXEC: r_offset는 절대 VA → load_base=0이면 그대로
            let target_va = lib.load_base + rela.r_offset;

            let target_phys = match unsafe { crate::paging::virt_to_phys(cr3, target_va) } {
                Some(p) => p,
                None    => {
                    crate::serial_println!(
                        "[dynlink] 재배치 주소 미매핑: va={:#x} type={}", target_va, rela.r_type
                    );
                    failed += 1;
                    continue;
                }
            };
            let ptr = (target_phys + hhdm_offset()) as *mut u64;

            match rela.r_type {
                // *target = load_base + addend
                R_X86_64_RELATIVE => {
                    let val = lib.load_base.wrapping_add_signed(rela.r_addend);
                    unsafe { *ptr = val; }
                }

                // *target = sym_va + addend
                R_X86_64_64 => {
                    let sym_va = resolve_sym_inner(&elf, lib, rela.r_sym, &resolver);
                    let val    = sym_va.wrapping_add_signed(rela.r_addend);
                    unsafe { *ptr = val; }
                }

                // *target = sym_va  (GOT 항목)
                R_X86_64_GLOB_DAT => {
                    let sym_va = resolve_sym_inner(&elf, lib, rela.r_sym, &resolver);
                    unsafe { *ptr = sym_va; }
                }

                // *target = sym_va  (PLT 항목; RTLD_NOW 즉시 바인딩)
                R_X86_64_JUMP_SLOT => {
                    let sym_va = resolve_sym_inner(&elf, lib, rela.r_sym, &resolver);
                    unsafe { *ptr = sym_va; }
                }

                // *target ← sym 정의의 데이터 복사 (실행 파일 BSS ← .so 정의)
                R_X86_64_COPY => {
                    let sym_va = resolve_sym_inner(&elf, lib, rela.r_sym, &resolver);
                    if sym_va == 0 { continue; }
                    // 심볼 크기 조회
                    if let Some(sym) = elf.sym(lib.symtab_foff, rela.r_sym as usize) {
                        let sz = sym.st_size as usize;
                        if sz == 0 { continue; }
                        let src_phys = match unsafe { crate::paging::virt_to_phys(cr3, sym_va) } {
                            Some(p) => p,
                            None    => continue,
                        };
                        let src_ptr = (src_phys + hhdm_offset()) as *const u8;
                        let dst_ptr = (target_phys + hhdm_offset()) as *mut u8;
                        unsafe { core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, sz); }
                    }
                }

                other => {
                    crate::serial_println!("[dynlink] 미지원 재배치 타입 {}", other);
                }
            }
        }
    }

    if failed > 0 {
        crate::serial_println!("[dynlink] {} 재배치 실패: {}건", lib.name, failed);
    } else {
        crate::serial_println!("[dynlink] {} 재배치 완료", lib.name);
    }
    failed
}

// ── 내부 헬퍼 ────────────────────────────────────────────────────────────────

fn resolve_sym_inner<F>(elf: &Elf64<'_>, lib: &SharedLib, sym_idx: u32, resolver: &F) -> u64
where
    F: Fn(&[u8]) -> Option<u64>,
{
    let sym = match elf.sym(lib.symtab_foff, sym_idx as usize) {
        Some(s) => s,
        None    => return 0,
    };
    if !sym.is_undef() {
        return lib.load_base + sym.st_value;
    }
    let name_bytes = elf.dyn_str(lib.strtab_foff, sym.st_name as usize)
        .unwrap_or(b"??");
    resolver(name_bytes).unwrap_or_else(|| {
        crate::serial_println!(
            "[dynlink] 미해결 심볼: {}",
            core::str::from_utf8(name_bytes).unwrap_or("??")
        );
        0
    })
}

// ── ELF 헤더 직접 읽기 헬퍼 ──────────────────────────────────────────────────

fn u64le_hdr(d: &[u8], off: usize) -> u64 {
    if off + 8 > d.len() { return 0; }
    elf::u64le(d, off)
}

fn u16le_hdr(d: &[u8], off: usize) -> u16 {
    if off + 2 > d.len() { return 0; }
    elf::u16le(d, off)
}
