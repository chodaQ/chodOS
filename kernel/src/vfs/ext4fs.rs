//! ext4 읽기 전용 드라이버 (ALPHA 8)
//!
//! ## 설계
//!
//! `ext4-view` 크레이트를 사용해 커널에 embed된 ext4 이미지를 읽는다.
//! 이미지는 `include_bytes!`로 커널 바이너리에 포함됨 (휘발성 없음).
//!
//! ## Ext4Read 구현
//!
//! ext4-view는 `Box<dyn Ext4Read>` 인터페이스를 통해 데이터를 읽는다.
//! `StaticSlice`가 `&'static [u8]`에 대해 `Ext4Read`를 구현한다.
//!
//! ## 경로 규칙
//!
//! ext4-view의 `Path`는 `/`로 시작하는 절대 경로를 받는다.
//! 예: `/etc/os-release`, `/var/log/boot.log`

use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use ext4_view::{Ext4, Ext4Read, PathBuf};

use crate::vfs::DirEntry;

// ── Ext4Read 구현 ────────────────────────────────────────────────────────────

/// `&'static [u8]` 슬라이스를 ext4-view가 요구하는 `Ext4Read` 인터페이스로 래핑.
struct StaticSlice(&'static [u8]);

type BoxedError = Box<dyn core::error::Error + Send + Sync + 'static>;

impl Ext4Read for StaticSlice {
    fn read(
        &mut self,
        start_byte: u64,
        dst: &mut [u8],
    ) -> Result<(), BoxedError> {
        let start = start_byte as usize;
        let end   = start + dst.len();
        if end > self.0.len() {
            return Err(Box::new(SliceReadError {
                start: start_byte,
                len: dst.len(),
                total: self.0.len(),
            }));
        }
        dst.copy_from_slice(&self.0[start..end]);
        Ok(())
    }
}

/// 슬라이스 범위 초과 시 반환하는 에러
#[derive(Debug)]
struct SliceReadError { start: u64, len: usize, total: usize }

impl core::fmt::Display for SliceReadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "read {} bytes at {} but slice is {} bytes",
               self.len, self.start, self.total)
    }
}

impl core::error::Error for SliceReadError {}

// ── Ext4 마운트 ──────────────────────────────────────────────────────────────

/// ext4 이미지 마운트 핸들.
///
/// `Ext4::load(Box<dyn Ext4Read>)` 로 초기화.
/// 이후 경로 기반으로 파일 읽기 / 디렉토리 열거 제공.
pub struct Ext4Fs {
    inner: Ext4,
}

impl Ext4Fs {
    /// `&'static [u8]` 이미지로부터 ext4 파일시스템 마운트.
    pub fn new(image: &'static [u8]) -> Result<Self, ext4_view::Ext4Error> {
        let reader: Box<dyn Ext4Read> = Box::new(StaticSlice(image));
        let inner = Ext4::load(reader)?;
        Ok(Ext4Fs { inner })
    }

    /// 파일을 읽어 `Vec<u8>` 반환. 경로가 없거나 읽기 실패 시 `None`.
    pub fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let p = ext4_view::Path::try_from(path).ok()?;
        self.inner.read(p).ok()
    }

    /// 디렉토리 항목 목록 반환 (`..` / `.` 제외).
    pub fn list_dir(&self, path: &str) -> Vec<DirEntry> {
        let p = match ext4_view::Path::try_from(path) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };
        let iter = match self.inner.read_dir(p) {
            Ok(i) => i,
            Err(_) => return Vec::new(),
        };

        let mut result = Vec::new();
        for entry in iter {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let raw = entry.file_name();
            // `.` / `..` 제외
            if raw.as_ref() == b"." || raw.as_ref() == b".." { continue; }

            let name = String::from_utf8_lossy(raw.as_ref()).into_owned();
            let is_dir = entry.file_type()
                .map(|t| t == ext4_view::FileType::Directory)
                .unwrap_or(false);
            let size = if is_dir {
                0
            } else {
                // 파일 크기: 경로를 재구성해서 읽기 시도
                let full = alloc::format!("{}/{}", path.trim_end_matches('/'), name);
                self.read_file(&full).map(|d| d.len()).unwrap_or(0)
            };

            result.push(DirEntry { name, is_dir, size });
        }
        result
    }

    /// 경로가 존재하는지 확인.
    pub fn exists(&self, path: &str) -> bool {
        let p = match ext4_view::Path::try_from(path) {
            Ok(p) => p,
            Err(_) => return false,
        };
        self.inner.metadata(p).is_ok()
    }
}
