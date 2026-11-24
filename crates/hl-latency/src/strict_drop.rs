//! Leaving out of the pairing what the offline strict pass refused.
//!
//! hl-capture's reader only scans a node line's header before stamping it, so
//! a writer-torn line the scan accepts still becomes an [`Observation`].
//! `hl-strict-pass` reads the same hourly files afterwards and lists every
//! record that fails; for a line the scan accepted (`scan_accepted`) it gives
//! the `block_number` and `block_time_ms` hl-capture recorded. This module
//! reads that output and removes those blocks before [`crate::pair`].
//!
//! Which stamps go depends on what the observations carry:
//!
//! - **By line.** `hl-capture` writes each node observation with its hourly
//!   file and the byte offset of its line (`file_date`, `file_hour`,
//!   `line_offset`), and the strict pass lists a failed line by the same file
//!   and offset. Only the observation of that one line goes. A block that
//!   still has a node line keeps its public-`WebSocket` arrivals and pairs on
//!   the earliest line left; a block that lost its only node line goes with
//!   its `WebSocket` arrivals, so it is neither matched nor missed.
//! - **By block.** Captures made before those fields existed carry no
//!   offset, so the stamp of a torn line cannot be told from the stamp of a
//!   good line of the same block (with `--stream-with-block-info` several
//!   lines can share one block). The whole block goes: every node
//!   observation with the listed height and block time, and every
//!   public-`WebSocket` observation with that block time.
//!
//! A failure the scan did not accept (`scan_accepted` false) has no height
//! or block time. It is either a line hl-capture stamped after its strict
//! parse accepted it (`scan_fallbacks`) or a line it never stamped. By line,
//! the observation at that line, if any, goes under the same block rules and
//! is counted apart; by block it cannot be found and stays.
//!
//! Decided with the Latency Reason: a torn stamp in the percentiles is worse
//! than losing a line or a block. [`StrictFailures::drop_from`] picks the
//! mode and refuses a file that mixes the two.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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
    path: Option<String>,
    offset: Option<u64>,
    scan_accepted: bool,
    block_number: Option<u64>,
    block_time_ms: Option<u64>,
}

#[derive(Deserialize)]
struct FileLine {
    path: Option<String>,
}

#[derive(Deserialize)]
struct TotalLine {
    failures_omitted: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Line {
    Failure(FailureLine),
    File(FileLine),
    Total(TotalLine),
}

/// One listed failure: where its line is and, when the scan accepted the
/// line, the height and block time it read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListedLine {
    path: String,
    offset: u64,
    key: Option<(u64, u64)>,
}

/// The scan-accepted failures of one `hl-strict-pass` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StrictFailures {
    /// `(block_number, block_time_ms)` of every scan-accepted failure.
    keys: BTreeSet<(u64, u64)>,
    /// Every failure, in the order listed.
    lines: Vec<ListedLine>,
    /// Failures the scan did not accept.
    fallback_listed: u64,
    /// The path of every `file` line that has one.
    files: Vec<String>,
    listed: u64,
    omitted: u64,
}

impl StrictFailures {
    /// Reads the JSON Lines `hl-strict-pass` writes to stdout (blank lines
    /// skipped).
    ///
    /// # Errors
    /// A line that is not a strict-pass line, a failure without its path or
    /// offset, a scan-accepted failure without its height or block time,
    /// anything after the `total` line, or no `total` line at all.
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
                Line::Failure(f) => {
                    let key = if f.scan_accepted {
                        let (Some(height), Some(time)) = (f.block_number, f.block_time_ms) else {
                            return Err(bad(
                                "a scan-accepted failure without block_number and block_time_ms"
                                    .to_owned(),
                            ));
                        };
                        Some((height, time))
                    } else {
                        None
                    };
                    let (Some(path), Some(offset)) = (f.path, f.offset) else {
                        return Err(bad("a failure without path and offset".to_owned()));
                    };
                    if let Some(key) = key {
                        out.keys.insert(key);
                        out.listed += 1;
                    } else {
                        out.fallback_listed += 1;
                    }
                    out.lines.push(ListedLine { path, offset, key });
                }
                Line::File(f) => out.files.extend(f.path),
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

    /// Leaves out of `obs` what the strict pass refused, keeping the order
    /// of the rest: by line when the node observations say which line they
    /// are, else by block with [`Self::apply`] (see the module text).
    ///
    /// By line, a listed failure names one line by its path's `{date}/{hour}`
    /// and its offset. The node observation of that line is removed, after
    /// checking that it is the block the failure names when the failure
    /// names one (a scan-accepted failure). A listed line with no
    /// observation is counted in `strict_failures_unmatched`, or in
    /// `strict_fallback_failures_unmatched` when the scan did not accept
    /// it.
    ///
    /// # Errors
    /// Node observations with and without the three fields in one file, or
    /// with only some of them; two node observations of one line; and, by
    /// line only: a listed failure whose path does not end in a date and an
    /// hour, two different paths for one date and hour, or an observation at
    /// a listed line with another height or block time.
    pub fn drop_from(
        &self,
        obs: Vec<Observation>,
    ) -> Result<(Vec<Observation>, StrictDrop), StrictDropError> {
        // Line -> index of the node observation that carries it.
        let mut by_line: BTreeMap<FileLineKey, usize> = BTreeMap::new();
        let mut without = 0u64;
        for (i, o) in obs.iter().enumerate() {
            let line = location(o)?;
            if o.source != Source::Node {
                continue;
            }
            match line {
                None => without += 1,
                Some(line) => {
                    if by_line.insert(line, i).is_some() {
                        return Err(StrictDropError::DuplicateLine {
                            date: line.0,
                            hour: line.1,
                            offset: line.2,
                        });
                    }
                }
            }
        }
        if by_line.is_empty() {
            return Ok(self.apply(obs));
        }
        if without > 0 {
            return Err(StrictDropError::Mixed {
                with: count(by_line.len()),
                without,
            });
        }
        self.apply_by_line(obs, &by_line)
    }

    /// Every listed failure with the hourly file its path names, after
    /// checking that each hourly file has one path.
    fn located_lines(&self) -> Result<Vec<Located<'_>>, StrictDropError> {
        // One path per hourly file, or an offset names more than one line.
        let mut paths: BTreeMap<(u32, u8), &str> = BTreeMap::new();
        // Failure lines first: a clash is reported against the failure's
        // path; then the `file` lines of files that had no failure.
        let mut listed = Vec::with_capacity(self.lines.len());
        for l in &self.lines {
            let Some(file) = hourly_file(&l.path) else {
                return Err(StrictDropError::FailureWithoutFile {
                    path: l.path.clone(),
                });
            };
            listed.push((file, l));
        }
        let files = self
            .files
            .iter()
            .filter_map(|p| Some((hourly_file(p)?, p.as_str())));
        for (file, path) in listed
            .iter()
            .map(|(f, l)| (*f, l.path.as_str()))
            .chain(files)
        {
            match paths.entry(file) {
                Entry::Vacant(e) => {
                    e.insert(path);
                }
                Entry::Occupied(e) if *e.get() != path => {
                    return Err(StrictDropError::TwoPathsOneFile {
                        first: (*e.get()).to_owned(),
                        second: path.to_owned(),
                    });
                }
                Entry::Occupied(_) => {}
            }
        }
        Ok(listed)
    }

    fn apply_by_line(
        &self,
        obs: Vec<Observation>,
        by_line: &BTreeMap<FileLineKey, usize>,
    ) -> Result<(Vec<Observation>, StrictDrop), StrictDropError> {
        let listed = self.located_lines()?;

        // Observations to remove: `torn` for scan-accepted failures,
        // `fallback` for the rest. A failure the scan did not accept gives
        // no height or block time, so there is no key to check: the
        // observation at its line, if hl-capture stamped one, goes.
        let mut torn: BTreeSet<usize> = BTreeSet::new();
        let mut fallback: BTreeSet<usize> = BTreeSet::new();
        let mut unmatched: BTreeSet<FileLineKey> = BTreeSet::new();
        let mut fallback_unmatched: BTreeSet<FileLineKey> = BTreeSet::new();
        for ((date, hour), l) in listed {
            let line = (date, hour, l.offset);
            let found = by_line.get(&line).copied();
            match (l.key, found) {
                (Some(_), None) => {
                    unmatched.insert(line);
                }
                (None, None) => {
                    fallback_unmatched.insert(line);
                }
                (Some(key), Some(i)) => {
                    let o = &obs[i];
                    if node_key(o) != Some(key) {
                        return Err(StrictDropError::KeyMismatch {
                            path: l.path.clone(),
                            offset: l.offset,
                            listed_number: key.0,
                            listed_time_ms: key.1,
                            observed_number: o.block_number,
                            observed_time_ms: o.block_time_ms,
                        });
                    }
                    torn.insert(i);
                }
                (None, Some(i)) => {
                    fallback.insert(i);
                }
            }
        }
        // A line listed both ways counts once, as scan-accepted.
        fallback.retain(|i| !torn.contains(i));
        let dropped_fallback = count(fallback.len());
        let dropped_torn = count(torn.len());
        torn.append(&mut fallback);

        // Every block that lost a line: its earliest stamp before the drop
        // and its earliest stamp left, if any.
        let mut blocks: BTreeMap<(u64, u64), (u64, Option<u64>)> = torn
            .iter()
            .filter_map(|&i| Some((node_key(&obs[i])?, (u64::MAX, None))))
            .collect();
        for (i, o) in obs.iter().enumerate() {
            let Some((before, after)) = node_key(o).and_then(|k| blocks.get_mut(&k)) else {
                continue;
            };
            *before = (*before).min(o.recv_ns);
            if !torn.contains(&i) {
                *after = Some(after.map_or(o.recv_ns, |ns| ns.min(o.recv_ns)));
            }
        }
        let dropped_times: BTreeSet<u64> = blocks
            .iter()
            .filter_map(|(&(_, time), &(_, after))| after.is_none().then_some(time))
            .collect();
        let mut report = StrictDrop {
            strict_pass_applied: true,
            strict_dropped_observations: dropped_torn,
            strict_dropped_fallback_lines: dropped_fallback,
            strict_fallback_failures_listed: self.fallback_listed,
            strict_fallback_failures_unmatched: count(fallback_unmatched.len()),
            strict_dropped_blocks: count(blocks.values().filter(|b| b.1.is_none()).count()),
            strict_failures_listed: self.listed,
            strict_failures_unmatched: count(unmatched.len()),
            strict_failures_omitted: self.omitted,
            strict_drop_incomplete: self.omitted > 0,
            strict_drop_by_line: true,
            strict_shifted_first_arrival: count(
                blocks
                    .values()
                    .filter(|&&(before, after)| after.is_some_and(|after| after > before))
                    .count(),
            ),
            ..StrictDrop::default()
        };
        let kept = obs
            .into_iter()
            .enumerate()
            .filter(|(i, o)| match o.source {
                Source::Node => !torn.contains(i),
                Source::PublicWs => {
                    let drop = dropped_times.contains(&o.block_time_ms);
                    report.strict_dropped_ws_arrivals += u64::from(drop);
                    !drop
                }
            })
            .map(|(_, o)| o)
            .collect();
        Ok((kept, report))
    }

    /// Removes every listed block from `obs`, keeping the order of the rest:
    /// the by-block drop, whatever the observations carry. Callers that read
    /// a capture want [`Self::drop_from`].
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
            strict_fallback_failures_listed: self.fallback_listed,
            strict_fallback_lines_unchecked: self.fallback_listed > 0,
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

/// A listed failure with the `(date, hour)` its path names.
type Located<'a> = ((u32, u8), &'a ListedLine);

/// One line of the node's output: `(file_date, file_hour, line_offset)`.
type FileLineKey = (u32, u8, u64);

/// The line an observation says it is. `None` when it says nothing.
fn location(o: &Observation) -> Result<Option<FileLineKey>, StrictDropError> {
    match (o.file_date, o.file_hour, o.line_offset) {
        (Some(date), Some(hour), Some(offset)) => Ok(Some((date, hour, offset))),
        (None, None, None) => Ok(None),
        _ => Err(StrictDropError::PartialLocation {
            block_time_ms: o.block_time_ms,
            recv_ns: o.recv_ns,
        }),
    }
}

/// `(date, hour)` of a path that ends in `{YYYYMMDD}/{H}`, the hour one or
/// two digits and at most 23: the rule of `hl_capture::hourly_key`, which
/// `hl-strict-pass` finds its files by. Kept here because hl-capture depends
/// on this crate, not the other way round; an hl-capture test holds the two
/// together.
fn hourly_file(path: &str) -> Option<(u32, u8)> {
    let path = Path::new(path);
    let hour = path.file_name()?.to_str()?;
    let date = path.parent()?.file_name()?.to_str()?;
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !(1..=2).contains(&hour.len()) || date.len() != 8 || !digits(hour) || !digits(date) {
        return None;
    }
    let hour: u8 = hour.parse().ok()?;
    (hour <= 23).then_some((date.parse().ok()?, hour))
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// What [`StrictFailures::drop_from`] left out; printed beside the summary.
// Four independent yes/no facts of a flat JSON summary, each printed under
// its own name; they are not states of one machine, so an enum would not fit.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StrictDrop {
    /// Whether strict-pass output was given at all. `false` means nothing
    /// below was checked, not that nothing was torn.
    pub strict_pass_applied: bool,
    /// Node observations removed.
    pub strict_dropped_observations: u64,
    /// Blocks left with no node observation. By block that is every listed
    /// block the capture observed; by line, only a block whose every node
    /// line was removed. Such a block is neither matched, node-only nor
    /// ws-only in the summary.
    pub strict_dropped_blocks: u64,
    /// Public-`WebSocket` observations removed because their block was.
    pub strict_dropped_ws_arrivals: u64,
    /// Scan-accepted failure lines read.
    pub strict_failures_listed: u64,
    /// Scan-accepted failures the capture has no observation for (for example a
    /// file it did not cover): distinct blocks when dropping by block,
    /// distinct lines when dropping by line.
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
    /// Failure lines read that the scan did not accept. Each is either a
    /// line hl-capture stamped after its strict parse accepted it (its
    /// `scan_fallbacks`) or a line it never stamped (its `bad_lines`); the
    /// strict pass does not say which.
    pub strict_fallback_failures_listed: u64,
    /// Of those, the distinct lines with no node observation: lines
    /// hl-capture did not stamp. Over the files a capture covered this is
    /// expected to equal its `bad_lines` plus `oversized_lines`; a
    /// difference is worth a look, and is not an error (the two can cover
    /// different spans). By-line drop only.
    pub strict_fallback_failures_unmatched: u64,
    /// Node observations removed because the strict pass failed their line
    /// and the scan had not accepted it: stamped scan-fallback lines. Not in
    /// `strict_dropped_observations`. By-line drop only.
    pub strict_dropped_fallback_lines: u64,
    /// The drop was by block and the strict pass listed failures the scan
    /// did not accept: a stamped one cannot be found without its offset, so
    /// its stamp may still be in the distribution.
    pub strict_fallback_lines_unchecked: bool,
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
