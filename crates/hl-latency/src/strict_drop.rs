//! Leaving out of the pairing what the offline strict pass refused.
//!
//! hl-capture's reader only scans a node line's header before stamping it, so
//! a writer-torn line the scan accepts still becomes an [`Observation`].
//! `hl-strict-pass` reads the same hourly files afterwards and lists every
//! record that fails; for a line the scan accepted (`scan_accepted`) it gives
//! the `block_number` and `block_time_ms` hl-capture recorded. This module
//! reads that output and removes those blocks before [`crate::pair`].
//!
//! An observation carries no file offset, so the stamp of a torn line cannot
//! be told from the stamp of a good line of the same block (with
//! `--stream-with-block-info` several lines can share one block). The whole
//! block is therefore left out: every node observation with the listed
//! height and block time, and every public-`WebSocket` observation with that
//! block time. Decided with the Latency Reason: a torn stamp in the
//! percentiles is worse than losing a block.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::{Observation, Source};

/// Error reading `hl-strict-pass` output.
#[derive(Debug, thiserror::Error)]
pub enum StrictReadError {
    /// I/O failure.
    #[error("read failed: {0}")]
    Io(#[from] std::io::Error),
    /// A line that is not one of the strict pass's `failure`, `file` or
    /// `total` lines.
    #[error("line {line}: {msg}")]
    Bad {
        /// 1-based line number.
        line: usize,
        /// What is wrong with it.
        msg: String,
    },
    /// The output has no `total` line: it was cut short.
    #[error("no total line: the strict-pass output is incomplete")]
    NoTotal,
}

#[derive(Deserialize)]
struct FailureLine {
    scan_accepted: bool,
    block_number: Option<u64>,
    block_time_ms: Option<u64>,
}

#[derive(Deserialize)]
struct TotalLine {
    failures_omitted: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Line {
    Failure(FailureLine),
    File(IgnoredAny),
    Total(TotalLine),
}

/// The scan-accepted failures of one `hl-strict-pass` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StrictFailures {
    /// `(block_number, block_time_ms)` of every scan-accepted failure.
    keys: BTreeSet<(u64, u64)>,
    listed: u64,
    omitted: u64,
}

impl StrictFailures {
    /// Reads the JSON Lines `hl-strict-pass` writes to stdout (blank lines
    /// skipped). A failure the scan did not accept is ignored: hl-capture
    /// never stamped it.
    ///
    /// # Errors
    /// A line that is not a strict-pass line, a scan-accepted failure without
    /// its height or block time, anything after the `total` line, or no
    /// `total` line at all.
    pub fn read<R: std::io::BufRead>(r: R) -> Result<Self, StrictReadError> {
        let mut out = Self::default();
        let mut total = false;
        for (i, line) in r.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let bad = |msg: String| StrictReadError::Bad { line: i + 1, msg };
            if total {
                return Err(bad("a line after the total line".to_owned()));
            }
            match serde_json::from_str(&line).map_err(|e| bad(e.to_string()))? {
                Line::Failure(f) if f.scan_accepted => {
                    let (Some(height), Some(time)) = (f.block_number, f.block_time_ms) else {
                        return Err(bad(
                            "a scan-accepted failure without block_number and block_time_ms"
                                .to_owned(),
                        ));
                    };
                    out.keys.insert((height, time));
                    out.listed += 1;
                }
                Line::Failure(_) | Line::File(IgnoredAny) => {}
                Line::Total(t) => {
                    out.omitted = t.failures_omitted;
                    total = true;
                }
            }
        }
        if total {
            Ok(out)
        } else {
            Err(StrictReadError::NoTotal)
        }
    }

    /// RED STUB: the whole-block drop, whatever the observations carry.
    ///
    /// # Errors
    /// None yet.
    pub fn drop_from(
        &self,
        obs: Vec<Observation>,
    ) -> Result<(Vec<Observation>, StrictDrop), StrictDropError> {
        Ok(self.apply(obs))
    }

    /// Removes every listed block from `obs`, keeping the order of the rest.
    #[must_use]
    pub fn apply(&self, obs: Vec<Observation>) -> (Vec<Observation>, StrictDrop) {
        // Listed key -> node observations that carry it.
        let mut hits: BTreeMap<(u64, u64), u64> = self.keys.iter().map(|&k| (k, 0)).collect();
        for o in &obs {
            if let Some(n) = node_key(o).and_then(|k| hits.get_mut(&k)) {
                *n += 1;
            }
        }
        let dropped_times: BTreeSet<u64> = hits
            .iter()
            .filter_map(|(&(_, time), &n)| (n > 0).then_some(time))
            .collect();
        let mut report = StrictDrop {
            strict_pass_applied: true,
            strict_dropped_blocks: count(hits.values().filter(|&&n| n > 0).count()),
            strict_failures_listed: self.listed,
            strict_failures_unmatched: count(hits.values().filter(|&&n| n == 0).count()),
            strict_failures_omitted: self.omitted,
            strict_drop_incomplete: self.omitted > 0,
            ..StrictDrop::default()
        };
        let kept = obs
            .into_iter()
            .filter(|o| match o.source {
                Source::Node => {
                    let drop = node_key(o).is_some_and(|k| self.keys.contains(&k));
                    report.strict_dropped_observations += u64::from(drop);
                    !drop
                }
                Source::PublicWs => {
                    let drop = dropped_times.contains(&o.block_time_ms);
                    report.strict_dropped_ws_arrivals += u64::from(drop);
                    !drop
                }
            })
            .collect();
        (kept, report)
    }
}

/// The key the strict pass lists, for a node observation that has a height.
fn node_key(o: &Observation) -> Option<(u64, u64)> {
    match o.source {
        Source::Node => Some((o.block_number?, o.block_time_ms)),
        Source::PublicWs => None,
    }
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// What [`StrictFailures::apply`] left out; printed beside the summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StrictDrop {
    /// Whether strict-pass output was given at all. `false` means nothing
    /// below was checked, not that nothing was torn.
    pub strict_pass_applied: bool,
    /// Node observations removed.
    pub strict_dropped_observations: u64,
    /// Listed blocks that had at least one node observation, all removed.
    pub strict_dropped_blocks: u64,
    /// Public-`WebSocket` observations removed because their block was.
    pub strict_dropped_ws_arrivals: u64,
    /// Scan-accepted failure lines read.
    pub strict_failures_listed: u64,
    /// Distinct listed blocks with no node observation (for example a file
    /// the capture did not cover).
    pub strict_failures_unmatched: u64,
    /// Failures the strict pass counted but did not list (`--max-failures`).
    pub strict_failures_omitted: u64,
    /// `strict_failures_omitted > 0`: some torn stamps may still be in the
    /// distribution.
    pub strict_drop_incomplete: bool,
    /// Whether the drop was by line (the observations carry their file and
    /// offset) and not by whole block.
    pub strict_drop_by_line: bool,
    /// Blocks that lost their earliest node line to the drop and kept a
    /// later one, so they pair on a later stamp. By-line drop only.
    pub strict_shifted_first_arrival: u64,
}

/// Why observations and `hl-strict-pass` output cannot be matched by line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StrictDropError {
    /// Some node observations carry their file and offset and some do not.
    #[error(
        "{with} node observations carry file_date, file_hour and line_offset and {without} do not: a mixed observation file cannot be matched to the strict pass"
    )]
    Mixed {
        /// Node observations with all three fields.
        with: u64,
        /// Node observations with none.
        without: u64,
    },
    /// An observation with only some of the three fields.
    #[error(
        "an observation (block_time_ms {block_time_ms}, recv_ns {recv_ns}) has only some of file_date, file_hour and line_offset"
    )]
    PartialLocation {
        /// Its block time.
        block_time_ms: u64,
        /// Its stamp.
        recv_ns: u64,
    },
    /// Two node observations name the same line.
    #[error("two node observations claim the same line: file {date}/{hour}, offset {offset}")]
    DuplicateLine {
        /// `file_date`.
        date: u32,
        /// `file_hour`.
        hour: u8,
        /// `line_offset`.
        offset: u64,
    },
    /// A scan-accepted failure whose path does not end in a date and hour.
    #[error(
        "strict-pass failure in {path:?}: the path does not end in {{YYYYMMDD}}/{{hour}}, so its line cannot be matched to an observation"
    )]
    FailureWithoutFile {
        /// The path as the strict pass printed it.
        path: String,
    },
    /// Two different paths in the strict-pass output are the same date and
    /// hour: more than one root, so an offset names more than one line.
    #[error(
        "strict-pass output covers {first:?} and {second:?}, the same date and hour: run it over one root"
    )]
    TwoPathsOneFile {
        /// The path seen first.
        first: String,
        /// The other one.
        second: String,
    },
    /// The observation at a listed line is not the block the failure names.
    #[error(
        "{path} offset {offset}: the strict pass lists block {listed_number} at {listed_time_ms} ms, but the observation of that line is block {observed_number:?} at {observed_time_ms} ms: wrong file or offset"
    )]
    KeyMismatch {
        /// The failure's path.
        path: String,
        /// The failure's offset.
        offset: u64,
        /// The failure's `block_number`.
        listed_number: u64,
        /// The failure's `block_time_ms`.
        listed_time_ms: u64,
        /// The observation's `block_number`.
        observed_number: Option<u64>,
        /// The observation's `block_time_ms`.
        observed_time_ms: u64,
    },
}
