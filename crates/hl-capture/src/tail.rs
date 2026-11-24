//! Follows the newest hourly node output file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use hl_latency::{parse_node_line, scan_node_header, CompleteLine, Observation, Source};

use crate::lines::{LineSink, LineSplitter};

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

/// Receives each observation a [`NodeTailer`] completes, in file order.
pub trait ObservationSink {
    /// One stamped arrival.
    fn observe(&mut self, obs: Observation);
}

/// Collects observations; grows on the heap. For tests and cold callers.
impl ObservationSink for Vec<Observation> {
    fn observe(&mut self, obs: Observation) {
        self.push(obs);
    }
}

/// Counters a run reports beside its observations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TailStats {
    /// Lines turned into observations.
    pub lines: u64,
    /// Lines that are not node lines (both `hl_latency::scan_node_header`
    /// and `hl_latency::parse_node_line` rejected them), plus unfinished
    /// lines abandoned at a rotation (counted, not fatal).
    pub bad_lines: u64,
    /// Switches to a newer hourly file.
    pub rotations: u64,
    /// `read` calls that returned data.
    pub reads: u64,
    /// Lines dropped for exceeding the line cap (see
    /// [`NodeTailer::with_max_line`]); not included in `bad_lines`.
    pub oversized_lines: u64,
    /// Directory walks looking for a newer hourly file.
    pub rescans: u64,
    /// Lines `hl_latency::scan_node_header` refused that the strict
    /// `hl_latency::parse_node_line` then accepted (included in `lines`).
    pub scan_fallbacks: u64,
    /// Observations whose `block_number` is not the previous one plus one.
    pub height_breaks: u64,
    /// Observations read by the poll that switched to a file (see
    /// [`Switch`]); their stamps can be late by the rescan interval.
    pub post_rotation_lines: u64,
}

/// What the poll that switched to a new file read from it. That file may
/// have existed for up to the caller's rescan interval before the switch, so
/// these observations' stamps are an upper bound, not an arrival time: leave
/// them out of latency percentiles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Switch {
    /// Observations read from the new file by the switching poll.
    pub lines: u64,
    /// `block_number` of the first of them.
    pub first_block_number: Option<u64>,
    /// `block_number` of the last of them.
    pub last_block_number: Option<u64>,
}

/// Turns each complete line into an observation, in place.
struct Parser<'a, S: ?Sized> {
    stats: &'a mut TailStats,
    sink: &'a mut S,
}

impl<S: ObservationSink + ?Sized> LineSink for Parser<'_, S> {
    fn line(&mut self, bytes: &[u8], recv_ns: u64) {
        // The splitter only hands out lines whose newline it has seen.
        let line = CompleteLine::assume_newline_seen(bytes);
        let obs = match scan_node_header(line) {
            Some((block_time_ms, block_number)) => Some(Observation {
                source: Source::Node,
                block_time_ms,
                block_number: Some(block_number),
                recv_ns,
            }),
            // Not hl-node's exact header shape: let the strict parse decide.
            None => std::str::from_utf8(bytes)
                .ok()
                .and_then(|s| parse_node_line(s, recv_ns).ok()),
        };
        match obs {
            Some(o) => {
                self.stats.lines += 1;
                self.sink.observe(o);
            }
            None => self.stats.bad_lines += 1,
        }
    }
}

#[derive(Debug)]
struct Current {
    path: PathBuf,
    key: (u32, u8),
    file: File,
}

/// Read buffer size: one `read` per poll in the common case.
const BUF: usize = 64 * 1024;

/// Most `read` calls one [`NodeTailer::poll`] makes (16 x 64 KiB = 1 MiB),
/// so a busy file cannot hold a caller past its deadline: the caller checks
/// the time between polls and polls again at once while
/// [`NodeTailer::backlogged`] is true.
pub const MAX_READS_PER_POLL: usize = 16;

/// Follows the newest hourly file under a root directory.
#[derive(Debug)]
pub struct NodeTailer {
    root: PathBuf,
    current: Option<Current>,
    polled: bool,
    splitter: LineSplitter,
    buf: Vec<u8>,
    stats: TailStats,
    backlogged: bool,
}

impl NodeTailer {
    /// A tailer over `root`. A file that already exists at the first poll is
    /// read from its end (history is not an arrival); every file that
    /// appears later is read from its start.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_max_line(root, crate::DEFAULT_MAX_LINE)
    }

    /// Like [`Self::new`], dropping (and counting) lines longer than
    /// `max_line` bytes so memory stays bounded.
    #[must_use]
    pub fn with_max_line(root: impl Into<PathBuf>, max_line: usize) -> Self {
        Self {
            root: root.into(),
            current: None,
            polled: false,
            splitter: LineSplitter::with_max_line(max_line),
            buf: vec![0; BUF],
            stats: TailStats::default(),
            backlogged: false,
        }
    }

    /// [`Self::poll_into`] collected into a new `Vec`: allocates, so it is
    /// for tests and cold callers, not for a capture loop.
    pub fn poll(&mut self, clock: &mut dyn FnMut() -> u64) -> std::io::Result<Vec<Observation>> {
        let mut out = Vec::new();
        self.poll_into(clock, &mut out)?;
        Ok(out)
    }

    /// Reads what is available now, at most [`MAX_READS_PER_POLL`] reads,
    /// and hands each observation it completes to `sink`. `clock` is called
    /// once right after each `read` returns data, before that data is
    /// parsed. When the budget runs out, [`Self::backlogged`] is true and
    /// the rest (including any switch to a newer file) waits for the next
    /// poll.
    ///
    /// A poll that finds no newer file does not allocate: lines are parsed
    /// in place, from the read buffer or the splitter's reserved buffer.
    pub fn poll_into<S: ObservationSink + ?Sized>(
        &mut self,
        clock: &mut dyn FnMut() -> u64,
        sink: &mut S,
    ) -> std::io::Result<()> {
        let first = !self.polled;
        self.polled = true;
        let mut budget = MAX_READS_PER_POLL;
        self.backlogged = false;
        self.drain(clock, sink, &mut budget)?;
        if self.backlogged {
            // Finish the current file before looking for a newer one.
            return Ok(());
        }
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
                self.drain(clock, sink, &mut budget)?;
            }
        }
        Ok(())
    }

    fn drain<S: ObservationSink + ?Sized>(
        &mut self,
        clock: &mut dyn FnMut() -> u64,
        sink: &mut S,
        budget: &mut usize,
    ) -> std::io::Result<()> {
        let Some(cur) = self.current.as_mut() else {
            return Ok(());
        };
        let mut parser = Parser {
            stats: &mut self.stats,
            sink,
        };
        loop {
            if *budget == 0 {
                self.backlogged = true;
                break;
            }
            *budget -= 1;
            let n = cur.file.read(&mut self.buf)?;
            if n == 0 {
                break;
            }
            // Stamp before anything else touches the bytes.
            let recv_ns = clock();
            parser.stats.reads += 1;
            self.splitter
                .push(self.buf.get(..n).unwrap_or_default(), recv_ns, &mut parser);
        }
        Ok(())
    }

    /// Asks the next poll that reaches the end of the current file to look
    /// for a newer one.
    pub fn request_rescan(&mut self) {}

    /// The most recent switch to a file that was not yet reported, if any.
    pub fn take_switch(&mut self) -> Option<Switch> {
        None
    }

    /// True when the last [`Self::poll`] stopped at its read budget with
    /// data possibly still waiting: poll again without sleeping.
    #[must_use]
    pub fn backlogged(&self) -> bool {
        self.backlogged
    }

    /// Counters so far.
    #[must_use]
    pub fn stats(&self) -> TailStats {
        TailStats {
            oversized_lines: self.splitter.oversized(),
            ..self.stats
        }
    }

    /// The file currently followed.
    #[must_use]
    pub fn current_file(&self) -> Option<&Path> {
        self.current.as_ref().map(|c| c.path.as_path())
    }
}
