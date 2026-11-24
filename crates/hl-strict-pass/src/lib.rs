//! Offline strict pass over the node's hourly output files (the files
//! `hl-capture` tails). See `docs/LATENCY_PROBE.md`.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde::Serialize;

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

/// A pass over files in order.
#[derive(Debug, Default)]
pub struct StrictPass {
    total: Counts,
    failures: Vec<Failure>,
}

impl StrictPass {
    /// A new pass.
    #[must_use]
    pub fn new(_opts: Options) -> Self {
        Self::default()
    }

    /// Checks one file.
    pub fn check_file<R: BufRead>(
        &mut self,
        name: &str,
        _reader: R,
        _allow_unterminated_tail: bool,
    ) -> std::io::Result<FileReport> {
        Ok(FileReport {
            path: name.to_owned(),
            ..FileReport::default()
        })
    }

    /// Counters over every file so far.
    #[must_use]
    pub fn total(&self) -> Counts {
        self.total
    }

    /// The failures kept in full.
    #[must_use]
    pub fn failures(&self) -> &[Failure] {
        &self.failures
    }

    /// Failures counted but not kept.
    #[must_use]
    pub fn failures_omitted(&self) -> u64 {
        0
    }
}

/// The strict check of one line (without its newline).
pub fn check_record(_line: &[u8]) -> Result<(u64, u64), String> {
    Err("not implemented".to_owned())
}

/// The hourly files under `dir`, oldest first.
pub fn hourly_files(_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    Ok(Vec::new())
}
