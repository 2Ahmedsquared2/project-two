//! Pre-touched, huge-page-backed memory for hot-path buffers.

use memmap2::{MmapMut, MmapOptions};

const HUGE: usize = 2 << 20;
const PAGE: usize = 4096;

/// Which backing an [`Arena`] obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backing {
    /// `MAP_HUGETLB` 2 MiB pages from the reserved pool.
    HugeTlb,
    /// Normal mapping with `madvise(MADV_HUGEPAGE)` accepted (THP may or may
    /// not back it; `AnonHugePages` in `/proc/self/smaps` tells).
    ThpAdvised,
    /// Normal 4 KiB pages.
    Normal,
}

/// Anonymous memory mapped and written once at creation, so the hot path never
/// takes a first-touch fault in it.
#[derive(Debug)]
pub struct Arena {
    map: MmapMut,
    backing: Backing,
    touched: usize,
}

#[cfg(target_os = "linux")]
fn map_backed(len: usize) -> std::io::Result<(MmapMut, Backing)> {
    if let Ok(m) = MmapOptions::new().len(len).huge(Some(21)).map_anon() {
        return Ok((m, Backing::HugeTlb));
    }
    let m = MmapOptions::new().len(len).map_anon()?;
    let b = if m.advise(memmap2::Advice::HugePage).is_ok() {
        Backing::ThpAdvised
    } else {
        Backing::Normal
    };
    Ok((m, b))
}

#[cfg(not(target_os = "linux"))]
fn map_backed(len: usize) -> std::io::Result<(MmapMut, Backing)> {
    Ok((MmapOptions::new().len(len).map_anon()?, Backing::Normal))
}

impl Arena {
    /// Maps at least `len` bytes (rounded up to 2 MiB), trying `MAP_HUGETLB`,
    /// then THP advice, then normal pages; then writes every 4 KiB page.
    /// Off Linux only normal pages are tried (memmap2 defines `MAP_HUGETLB`
    /// as 0 there, so a "huge" mapping would silently be a normal one).
    pub fn new(len: usize) -> std::io::Result<Self> {
        let len = len.div_ceil(HUGE).max(1) * HUGE;
        let (mut map, backing) = map_backed(len)?;
        let mut touched = 0;
        for i in (0..map.len()).step_by(PAGE) {
            // A write, not a read: a read of a fresh anonymous page maps the
            // shared zero page and the first write would fault again.
            map[i] = 0;
            touched += 1;
        }
        Ok(Self {
            map,
            backing,
            touched,
        })
    }

    /// Bytes mapped.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Always false (an arena is at least 2 MiB).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The backing obtained.
    #[must_use]
    pub fn backing(&self) -> Backing {
        self.backing
    }

    /// 4 KiB pages written at creation.
    #[must_use]
    pub fn touched_pages(&self) -> usize {
        self.touched
    }

    /// The memory.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.map[..]
    }
}
