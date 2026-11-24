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
}
