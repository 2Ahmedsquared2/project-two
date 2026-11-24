//! RED STATE: a stub under the final tests. `read` and `apply` do nothing.
//!
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

use serde::Serialize;

use crate::Observation;

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

/// The scan-accepted failures of one `hl-strict-pass` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StrictFailures {}

impl StrictFailures {
    /// Reads the JSON Lines `hl-strict-pass` writes to stdout (blank lines
    /// skipped). A failure the scan did not accept is ignored: hl-capture
    /// never stamped it.
    ///
    /// # Errors
    /// A line that is not a strict-pass line, a scan-accepted failure without
    /// its height or block time, anything after the `total` line, or no
    /// `total` line at all.
    pub fn read<R: std::io::BufRead>(_r: R) -> Result<Self, StrictReadError> {
        Ok(Self::default())
    }

    /// Removes every listed block from `obs`, keeping the order of the rest.
    #[must_use]
    pub fn apply(&self, obs: Vec<Observation>) -> (Vec<Observation>, StrictDrop) {
        (obs, StrictDrop::default())
    }
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
