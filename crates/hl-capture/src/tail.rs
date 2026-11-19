//! Follows the newest hourly node output file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use hl_latency::{parse_node_line, Observation};

use crate::lines::{LineSplitter, StampedLine};

/// Sort key of an hourly file path `.../{YYYYMMDD}/{H}`: `(date, hour)`.
/// `None` when the last two components are not a date and an hour 0..=23.
#[must_use]
pub fn hourly_key(path: &Path) -> Option<(u32, u8)> {
    let hour = path.file_name()?.to_str()?;
    let date = path.parent()?.file_name()?.to_str()?;
    let digits = |s: &str, lens: std::ops::RangeInclusive<usize>| {
        lens.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    };
    if !digits(hour, 1..=2) || !digits(date, 8..=8) {
        return None;
    }
    let hour: u8 = hour.parse().ok()?;
    (hour <= 23).then_some(())?;
    Some((date.parse().ok()?, hour))
}

/// Deepest directory level searched below the root (`hourly/{date}/{hour}`
/// is three).
const MAX_DEPTH: usize = 4;

/// The newest hourly file anywhere under `root`, by [`hourly_key`]
/// (numeric, so hour `10` is newer than hour `9`). `Ok(None)` when none.
pub fn newest_hourly_file(root: &Path) -> std::io::Result<Option<PathBuf>> {
    fn walk(
        dir: &Path,
        depth: usize,
        best: &mut Option<((u32, u8), PathBuf)>,
    ) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let ty = entry.file_type()?;
            if ty.is_dir() {
                if depth < MAX_DEPTH {
                    walk(&path, depth + 1, best)?;
                }
            } else if ty.is_file() {
                if let Some(key) = hourly_key(&path) {
                    if best.as_ref().is_none_or(|(k, _)| key > *k) {
                        *best = Some((key, path));
                    }
                }
            }
        }
        Ok(())
    }
    let mut best = None;
    match walk(root, 1, &mut best) {
        Ok(()) => Ok(best.map(|(_, p)| p)),
        // The node has not created its output directory yet.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Counters a run reports beside its observations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TailStats {
    /// Lines parsed into observations.
    pub lines: u64,
    /// Lines `hl_latency::parse_node_line` rejected, plus unfinished lines
    /// abandoned at a rotation (counted, not fatal).
    pub bad_lines: u64,
    /// Switches to a newer hourly file.
    pub rotations: u64,
    /// `read` calls that returned data.
    pub reads: u64,
}

#[derive(Debug)]
struct Current {
    path: PathBuf,
    key: (u32, u8),
    file: File,
}

/// Read buffer size: one `read` per poll in the common case.
const BUF: usize = 64 * 1024;

/// Follows the newest hourly file under a root directory.
#[derive(Debug)]
pub struct NodeTailer {
    root: PathBuf,
    current: Option<Current>,
    polled: bool,
    splitter: LineSplitter,
    buf: Vec<u8>,
    lines: Vec<StampedLine>,
    stats: TailStats,
}

impl NodeTailer {
    /// A tailer over `root`. A file that already exists at the first poll is
    /// read from its end (history is not an arrival); every file that
    /// appears later is read from its start.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            current: None,
            polled: false,
            splitter: LineSplitter::new(),
            buf: vec![0; BUF],
            lines: Vec::new(),
            stats: TailStats::default(),
        }
    }

    /// Reads everything available now and returns the observations it
    /// completed. `clock` is called once right after each `read` returns
    /// data, before that data is parsed.
    pub fn poll(&mut self, clock: &mut dyn FnMut() -> u64) -> std::io::Result<Vec<Observation>> {
        let first = !self.polled;
        self.polled = true;
        let mut out = Vec::new();
        self.drain(clock, &mut out)?;
        if let Some(newest) = newest_hourly_file(&self.root)? {
            let key = hourly_key(&newest);
            let newer = match (&self.current, key) {
                (None, Some(_)) => true,
                (Some(c), Some(k)) => k > c.key,
                (_, None) => false,
            };
            if let (true, Some(key)) = (newer, key) {
                let mut file = File::open(&newest)?;
                if self.current.is_some() {
                    self.stats.rotations += 1;
                    if self.splitter.clear() {
                        self.stats.bad_lines += 1;
                    }
                } else if first {
                    file.seek(SeekFrom::End(0))?;
                }
                self.current = Some(Current {
                    path: newest,
                    key,
                    file,
                });
                self.drain(clock, &mut out)?;
            }
        }
        Ok(out)
    }

    fn drain(
        &mut self,
        clock: &mut dyn FnMut() -> u64,
        out: &mut Vec<Observation>,
    ) -> std::io::Result<()> {
        let Some(cur) = self.current.as_mut() else {
            return Ok(());
        };
        loop {
            let n = cur.file.read(&mut self.buf)?;
            if n == 0 {
                break;
            }
            // Stamp before anything else touches the bytes.
            let recv_ns = clock();
            self.stats.reads += 1;
            self.splitter.push(
                self.buf.get(..n).unwrap_or_default(),
                recv_ns,
                &mut self.lines,
            );
        }
        for line in self.lines.drain(..) {
            match std::str::from_utf8(&line.bytes)
                .ok()
                .and_then(|s| parse_node_line(s, line.recv_ns).ok())
            {
                Some(o) => {
                    self.stats.lines += 1;
                    out.push(o);
                }
                None => self.stats.bad_lines += 1,
            }
        }
        Ok(())
    }

    /// Counters so far.
    #[must_use]
    pub fn stats(&self) -> TailStats {
        self.stats
    }

    /// The file currently followed.
    #[must_use]
    pub fn current_file(&self) -> Option<&Path> {
        self.current.as_ref().map(|c| c.path.as_path())
    }
}
