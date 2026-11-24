//! Offline strict pass over the node's hourly output files, the files
//! `hl-capture` tails (`docs/LATENCY_PROBE.md`).
//!
//! `hl-capture`'s reader only scans each line's header before stamping it
//! (`hl_latency::scan_node_header`), so a line the node's writer tore that
//! still ends with `}` gets a stamp. This crate is the check that runs
//! afterwards, off every hot path: it reads the same files, puts every line
//! through [`check_record`], lists the lines that fail and counts where the
//! block height does not follow on.
//!
//! Nothing here is built for speed: it allocates and parses each line twice.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use hl_latency::{parse_node_line, scan_node_header, CompleteLine};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

/// Default cap on one line, in bytes (a memory bound, not a format rule).
pub const DEFAULT_MAX_LINE: usize = 64 * 1024 * 1024;
/// Default number of failures listed in full.
pub const DEFAULT_MAX_FAILURES: usize = 100;

/// Limits of a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// A longer line is a failure (`oversized`) and is not parsed.
    pub max_line: usize,
    /// Failures kept in full; the counts are never capped.
    pub max_failures: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_line: DEFAULT_MAX_LINE,
            max_failures: DEFAULT_MAX_FAILURES,
        }
    }
}

/// Counters, per file and in total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    /// Records read.
    pub records: u64,
    /// Records that fail the strict check.
    pub failed: u64,
    /// Failed records `hl_latency::scan_node_header` accepts.
    pub scan_accepted_failed: u64,
    /// Files whose last line has no newline.
    pub unterminated_tail: u64,
    /// Passing records whose height is not the previous one plus one.
    pub height_breaks: u64,
    /// Breaks with the same height as the previous record.
    pub height_repeats: u64,
    /// Breaks that skip forward.
    pub height_gaps: u64,
    /// Breaks that go back.
    pub height_backwards: u64,
    /// Passing records whose block time is below the previous one's.
    pub block_time_backwards: u64,
    /// Passing records whose height was seen before, not counting a repeat
    /// of the previous record's height.
    pub duplicate_block_numbers: u64,
}

/// One file's result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FileReport {
    /// The name given to [`StrictPass::check_file`].
    pub path: String,
    /// This file's counters.
    #[serde(flatten)]
    pub counts: Counts,
    /// Height of the first passing record.
    pub first_block_number: Option<u64>,
    /// Height of the last passing record.
    pub last_block_number: Option<u64>,
    /// Block time of the first passing record.
    pub first_block_time_ms: Option<u64>,
    /// Block time of the last passing record.
    pub last_block_time_ms: Option<u64>,
}

/// One record that failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    /// The file's name.
    pub path: String,
    /// 1-based line number.
    pub line: u64,
    /// Byte offset of the line's first byte.
    pub offset: u64,
    /// Line length in bytes, without its newline.
    pub len: u64,
    /// Why it failed.
    pub reason: String,
    /// Whether `hl_latency::scan_node_header` accepts the line.
    pub scan_accepted: bool,
    /// The scan's height, when it accepts.
    pub block_number: Option<u64>,
    /// The scan's block time, when it accepts.
    pub block_time_ms: Option<u64>,
}

/// Heights seen so far, as sorted, disjoint, inclusive ranges. Heights
/// mostly follow on, so this stays a handful of ranges however long the
/// pass is.
#[derive(Debug, Default)]
struct Seen(Vec<(u64, u64)>);

impl Seen {
    /// Adds `h`; true when it was already there.
    fn insert(&mut self, h: u64) -> bool {
        let i = self.0.partition_point(|r| r.1 < h);
        if self.0.get(i).is_some_and(|r| r.0 <= h) {
            return true;
        }
        let joins_prev = i > 0 && self.0[i - 1].1.checked_add(1) == Some(h);
        let joins_next = self.0.get(i).is_some_and(|r| h.checked_add(1) == Some(r.0));
        match (joins_prev, joins_next) {
            (true, true) => {
                self.0[i - 1].1 = self.0[i].1;
                self.0.remove(i);
            }
            (true, false) => self.0[i - 1].1 = h,
            (false, true) => self.0[i].0 = h,
            (false, false) => self.0.insert(i, (h, h)),
        }
        false
    }
}

/// A pass over files in order. Heights, block times and the set of heights
/// seen carry from one file to the next, so give it the files oldest first.
#[derive(Debug)]
pub struct StrictPass {
    opts: Options,
    total: Counts,
    failures: Vec<Failure>,
    omitted: u64,
    /// `(block_time_ms, block_number)` of the last record that passed.
    prev: Option<(u64, u64)>,
    seen: Seen,
}

fn len64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

impl StrictPass {
    /// A new pass.
    #[must_use]
    pub fn new(opts: Options) -> Self {
        Self {
            opts,
            total: Counts::default(),
            failures: Vec::new(),
            omitted: 0,
            prev: None,
            seen: Seen::default(),
        }
    }

    /// Checks one file, read to its end from `reader`; `name` is what the
    /// report and the failures call it.
    ///
    /// Every newline-terminated line is a record. A last line without a
    /// newline is counted in `unterminated_tail`; it is a failed record
    /// (reason `unterminated`, never parsed) unless
    /// `allow_unterminated_tail`, which is for the hour the node is still
    /// writing and leaves that line out of `records`.
    pub fn check_file<R: BufRead>(
        &mut self,
        name: &str,
        mut reader: R,
        allow_unterminated_tail: bool,
    ) -> std::io::Result<FileReport> {
        let mut report = FileReport {
            path: name.to_owned(),
            ..FileReport::default()
        };
        let mut line = Vec::new();
        // Of the line being read: its number, where it starts, its length
        // so far, and whether it has outgrown the cap (then `line` is empty
        // and the rest is skipped).
        let (mut number, mut offset, mut len, mut oversized) = (1u64, 0u64, 0u64, false);
        loop {
            let buf = reader.fill_buf()?;
            if buf.is_empty() {
                break;
            }
            let newline = buf.iter().position(|&b| b == b'\n');
            let chunk = buf.get(..newline.unwrap_or(buf.len())).unwrap_or_default();
            len = len.saturating_add(len64(chunk.len()));
            if !oversized {
                if line.len().saturating_add(chunk.len()) > self.opts.max_line {
                    oversized = true;
                    line = Vec::new();
                } else {
                    line.extend_from_slice(chunk);
                }
            }
            let used = chunk.len() + usize::from(newline.is_some());
            reader.consume(used);
            if newline.is_some() {
                let at = (number, offset, len);
                if oversized {
                    self.fail(&mut report, at, "oversized".to_owned(), None);
                } else {
                    self.record(&mut report, at, &line);
                }
                number = number.saturating_add(1);
                offset = offset.saturating_add(len).saturating_add(1);
                len = 0;
                oversized = false;
                line.clear();
            }
        }
        if len > 0 {
            report.counts.unterminated_tail += 1;
            if !allow_unterminated_tail {
                // hl-capture never stamps a line whose newline it has not
                // seen, so there is no scan result to report.
                self.fail(
                    &mut report,
                    (number, offset, len),
                    "unterminated".to_owned(),
                    None,
                );
            }
        }
        self.total.add(&report.counts);
        Ok(report)
    }

    /// One complete line.
    fn record(&mut self, report: &mut FileReport, at: (u64, u64, u64), line: &[u8]) {
        let (time, height) = match check_record(line) {
            Ok(ok) => ok,
            Err(reason) => {
                let scan = scan_node_header(CompleteLine::assume_newline_seen(line));
                self.fail(report, at, reason, scan);
                return;
            }
        };
        let c = &mut report.counts;
        c.records += 1;
        if let Some((prev_time, prev_height)) = self.prev {
            if height == prev_height {
                c.height_repeats += 1;
            } else if height < prev_height {
                c.height_backwards += 1;
            } else if prev_height.checked_add(1) != Some(height) {
                c.height_gaps += 1;
            }
            c.height_breaks = c.height_repeats + c.height_gaps + c.height_backwards;
            if time < prev_time {
                c.block_time_backwards += 1;
            }
        }
        let repeat = self.prev.is_some_and(|p| p.1 == height);
        if self.seen.insert(height) && !repeat {
            c.duplicate_block_numbers += 1;
        }
        self.prev = Some((time, height));
        report.first_block_number.get_or_insert(height);
        report.first_block_time_ms.get_or_insert(time);
        report.last_block_number = Some(height);
        report.last_block_time_ms = Some(time);
    }

    /// One failed record. `scan` is what the hot path's header scan said
    /// about the line, when it was asked.
    fn fail(
        &mut self,
        report: &mut FileReport,
        (line, offset, len): (u64, u64, u64),
        reason: String,
        scan: Option<(u64, u64)>,
    ) {
        report.counts.records += 1;
        report.counts.failed += 1;
        report.counts.scan_accepted_failed += u64::from(scan.is_some());
        if self.failures.len() >= self.opts.max_failures {
            self.omitted += 1;
            return;
        }
        self.failures.push(Failure {
            path: report.path.clone(),
            line,
            offset,
            len,
            reason,
            scan_accepted: scan.is_some(),
            block_number: scan.map(|s| s.1),
            block_time_ms: scan.map(|s| s.0),
        });
    }

    /// Counters over every file so far.
    #[must_use]
    pub fn total(&self) -> Counts {
        self.total
    }

    /// The failures kept in full, in the order read (at most
    /// [`Options::max_failures`]).
    #[must_use]
    pub fn failures(&self) -> &[Failure] {
        &self.failures
    }

    /// Failures counted but not kept.
    #[must_use]
    pub fn failures_omitted(&self) -> u64 {
        self.omitted
    }
}

impl Counts {
    fn add(&mut self, o: &Self) {
        self.records += o.records;
        self.failed += o.failed;
        self.scan_accepted_failed += o.scan_accepted_failed;
        self.unterminated_tail += o.unterminated_tail;
        self.height_breaks += o.height_breaks;
        self.height_repeats += o.height_repeats;
        self.height_gaps += o.height_gaps;
        self.height_backwards += o.height_backwards;
        self.block_time_backwards += o.block_time_backwards;
        self.duplicate_block_numbers += o.duplicate_block_numbers;
    }
}

/// The top level of a node line, each documented key at most once
/// (`serde`'s derive refuses a repeated field). Other keys are allowed.
#[derive(Deserialize)]
struct Top {
    #[serde(rename = "local_time")]
    _local_time: String,
    #[serde(rename = "block_time")]
    _block_time: String,
    #[serde(rename = "block_number")]
    _block_number: u64,
    /// Elements are not looked at here: [`parse_node_line`] has already
    /// validated the whole line as JSON, and what an event holds depends on
    /// the stream.
    #[serde(rename = "events")]
    _events: Vec<IgnoredAny>,
}

/// The strict check of one line (without its newline; a trailing `\r` is
/// JSON whitespace and passes). `Ok((block_time_ms, block_number))`, or why
/// the line is not a record:
///
/// 1. it is valid UTF-8;
/// 2. [`hl_latency::parse_node_line`] accepts it: the whole line is one
///    JSON value, an object whose `block_time` is a timestamp string and
///    whose `block_number` is a `u64`;
/// 3. it has the documented top level `{local_time, block_time,
///    block_number, events}` (`docs/NODE_SETUP.md`), each key once,
///    `local_time` a string and `events` an array. What the events hold is
///    not checked;
/// 4. if [`hl_latency::scan_node_header`] answers for it, it gives the same
///    block time and height.
pub fn check_record(line: &[u8]) -> Result<(u64, u64), String> {
    let text = std::str::from_utf8(line).map_err(|e| format!("invalid UTF-8: {e}"))?;
    let obs = parse_node_line(text, 0).map_err(|e| e.to_string())?;
    let strict = (
        obs.block_time_ms,
        obs.block_number
            .ok_or_else(|| "block_number missing".to_owned())?,
    );
    serde_json::from_str::<Top>(text).map_err(|e| format!("schema: {e}"))?;
    match scan_node_header(CompleteLine::assume_newline_seen(line)) {
        Some(scan) if scan != strict => Err(format!(
            "header scan reads block {} at {} ms, strict parse block {} at {} ms",
            scan.1, scan.0, strict.1, strict.0
        )),
        _ => Ok(strict),
    }
}

/// Deepest directory level searched below the root, as in `hl-capture`.
const MAX_DEPTH: usize = 4;

/// The hourly files under `dir` (`.../{YYYYMMDD}/{H}`, as
/// [`hl_capture::hourly_key`] reads them), oldest first by date then hour,
/// numerically. Other files are ignored.
pub fn hourly_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<((u32, u8), PathBuf)>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            // Follows symlinks: an archived hour may be linked in.
            let meta = std::fs::metadata(&path)?;
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    walk(&path, depth + 1, out)?;
                }
            } else if let Some(key) = hl_capture::hourly_key(&path) {
                out.push((key, path));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, 1, &mut out)?;
    out.sort();
    Ok(out.into_iter().map(|(_, p)| p).collect())
}
