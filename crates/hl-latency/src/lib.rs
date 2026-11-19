//! Same-block arrival timing: our node's output versus the public `WebSocket`.
//!
//! The question this crate answers (asked by the Latency Reason): for one
//! block, how much earlier does a process on our host learn about it from the
//! node's output files than from `wss://api.hyperliquid.xyz/ws`? Both arrivals
//! are stamped by the same probe with the same host clock, so the difference
//! does not depend on clock sync between machines.
//!
//! Blocks are joined on their block time in milliseconds: the node's
//! `--batch-by-block` / `--stream-with-block-info` lines carry `block_time`,
//! and the public `l2Book` and `trades` messages carry the block time as
//! `time` (OBSERVED in Phase 1: 253 of 253 `l2Book` snapshots had `time` equal
//! to the block's `blockTime`). Blocks are ~70 ms apart, so the millisecond is
//! a unique key in practice; a key the node reports with two heights is
//! counted as ambiguous and excluded, never paired.
//!
//! The crate is I/O-free: parsing, pairing and statistics only. The capture
//! side (tailing the node's files, holding the `WebSocket`) is a separate
//! binary that writes [`Observation`]s as JSON Lines.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where an arrival was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A line in one of our node's `~/hl/data` output files.
    Node,
    /// A message on the public Hyperliquid `WebSocket`.
    PublicWs,
}

/// One arrival of evidence about a block, stamped by the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// Where it arrived.
    pub source: Source,
    /// Block time, Unix milliseconds (the join key).
    pub block_time_ms: u64,
    /// Block height, when the source carries it (the node does, the public
    /// `WebSocket` does not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_number: Option<u64>,
    /// Probe receive time, nanoseconds on the probe's clock.
    pub recv_ns: u64,
}

/// Errors from parsing a source line.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    /// Not valid JSON, or a required field is missing or has the wrong type.
    #[error("malformed line: {0}")]
    Malformed(String),
    /// A timestamp string that is not `YYYY-MM-DDTHH:MM:SS[.fraction]`.
    #[error("bad timestamp: {0:?}")]
    BadTimestamp(String),
}

/// Parses a naive UTC timestamp `YYYY-MM-DDTHH:MM:SS[.f{1,9}]` into Unix
/// milliseconds (fraction truncated, not rounded). This is how `chrono`
/// serialises the node's `NaiveDateTime` fields (source: the `Batch` type in
/// `hyperliquid-dex/order_book_server`, `server/src/types/node_data.rs`, which
/// turns `block_time` into milliseconds with `and_utc().timestamp_millis()`).
/// Years before 1970 are rejected: no block is that old.
pub fn parse_naive_utc_ms(s: &str) -> Result<u64, ParseError> {
    let bad = || ParseError::BadTimestamp(s.to_owned());
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return Err(bad());
    }
    let num = |r: std::ops::Range<usize>| -> Option<u64> {
        let part = &b[r];
        if part.iter().all(u8::is_ascii_digit) {
            Some(
                part.iter()
                    .fold(0, |acc, &c| acc * 10 + u64::from(c - b'0')),
            )
        } else {
            None
        }
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(min), Some(sec)) = (
        num(0..4),
        num(5..7),
        num(8..10),
        num(11..13),
        num(14..16),
        num(17..19),
    ) else {
        return Err(bad());
    };
    let millis = match &b[19..] {
        [] => 0,
        [b'.', frac @ ..]
            if (1..=9).contains(&frac.len()) && frac.iter().all(u8::is_ascii_digit) =>
        {
            // First three fraction digits, right-padded with zeros.
            frac.iter()
                .chain(std::iter::repeat(&b'0'))
                .take(3)
                .fold(0, |acc, &c| acc * 10 + u64::from(c - b'0'))
        }
        _ => return Err(bad()),
    };
    if year < 1970
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || min > 59
        || sec > 59
    {
        return Err(bad());
    }
    let days = days_from_civil(year, month, day);
    Ok(((days * 24 + hour) * 60 + min) * 60_000 + sec * 1000 + millis)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

fn days_in_month(y: u64, m: u64) -> u64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a valid date with year >= 1970 (Howard
/// Hinnant's `days_from_civil`, restricted to non-negative results).
fn days_from_civil(y: u64, m: u64, d: u64) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn malformed(why: &str) -> ParseError {
    ParseError::Malformed(why.to_owned())
}

/// Parses one node output line in the block-info schema
/// `{local_time, block_time, block_number, events}` written with
/// `--batch-by-block` or `--stream-with-block-info` (source: `hyperliquid-dex/node`
/// README, "Reading L1 Data"). Only `block_time` and `block_number` are read.
pub fn parse_node_line(line: &str, recv_ns: u64) -> Result<Observation, ParseError> {
    let v: Value = serde_json::from_str(line).map_err(|e| malformed(&e.to_string()))?;
    let block_time = v
        .get("block_time")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("block_time missing or not a string"))?;
    let block_number = v
        .get("block_number")
        .and_then(Value::as_u64)
        .ok_or_else(|| malformed("block_number missing or not a u64"))?;
    Ok(Observation {
        source: Source::Node,
        block_time_ms: parse_naive_utc_ms(block_time)?,
        block_number: Some(block_number),
        recv_ns,
    })
}

/// Parses one public `WebSocket` message into the block times it proves the
/// sender had seen: `l2Book` gives one (`data.time`), `trades` one per
/// distinct `time` in message order. Other channels (subscription acks,
/// pongs) give none.
pub fn parse_ws_message(msg: &str, recv_ns: u64) -> Result<Vec<Observation>, ParseError> {
    let v: Value = serde_json::from_str(msg).map_err(|e| malformed(&e.to_string()))?;
    let obs = |t| Observation {
        source: Source::PublicWs,
        block_time_ms: t,
        block_number: None,
        recv_ns,
    };
    let time_of = |d: &Value| {
        d.get("time")
            .and_then(Value::as_u64)
            .ok_or_else(|| malformed("time missing or not a u64"))
    };
    match v.get("channel").and_then(Value::as_str) {
        Some("l2Book") => {
            let data = v
                .get("data")
                .ok_or_else(|| malformed("l2Book without data"))?;
            Ok(vec![obs(time_of(data)?)])
        }
        Some("trades") => {
            let data = v
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| malformed("trades data is not an array"))?;
            let mut out: Vec<Observation> = Vec::new();
            for trade in data {
                let t = time_of(trade)?;
                if !out.iter().any(|o| o.block_time_ms == t) {
                    out.push(obs(t));
                }
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}
/// One block seen by both sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Pair {
    /// Join key.
    pub block_time_ms: u64,
    /// Height from the node line.
    pub block_number: Option<u64>,
    /// First node arrival.
    pub node_ns: u64,
    /// First public-`WebSocket` arrival.
    pub ws_ns: u64,
}

impl Pair {
    /// `ws_ns - node_ns`: positive means the node was earlier.
    #[must_use]
    pub fn node_lead_ns(&self) -> i64 {
        let diff = i128::from(self.ws_ns) - i128::from(self.node_ns);
        // Saturate: a lead beyond ±292 years means a corrupt stamp, not data.
        i64::try_from(diff).unwrap_or(if diff > 0 { i64::MAX } else { i64::MIN })
    }
}

/// Result of joining two arrival streams.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Pairing {
    /// Blocks seen by both, ordered by block time.
    pub pairs: Vec<Pair>,
    /// Blocks seen only on the node.
    pub node_only: u64,
    /// Blocks seen only on the public `WebSocket`.
    pub ws_only: u64,
    /// Node lines whose block time repeated an earlier node line with a
    /// different height (the join key collided).
    pub key_collisions: u64,
    /// Block-time keys the node saw with more than one height. They are left
    /// out of `pairs`, `node_only` and `ws_only`: the public `WebSocket`
    /// carries no height, so its arrival cannot be matched to either block.
    pub ambiguous: u64,
}

/// Joins observations on block time, keeping the first arrival per source.
///
/// A block time the node reports with more than one height is ambiguous: the
/// public `WebSocket` has no height, so its arrival cannot be attributed to one
/// block. Such keys are counted in [`Pairing::ambiguous`] and left out of
/// every other field, so they never reach the lead distribution.
pub fn pair<I: IntoIterator<Item = Observation>>(obs: I) -> Pairing {
    #[derive(Default)]
    struct Seen {
        node: Option<(u64, Option<u64>)>,
        ws: Option<u64>,
        ambiguous: bool,
    }
    let mut by_time: BTreeMap<u64, Seen> = BTreeMap::new();
    let mut key_collisions = 0;
    for o in obs {
        let seen = by_time.entry(o.block_time_ms).or_default();
        match o.source {
            Source::Node => match &mut seen.node {
                None => seen.node = Some((o.recv_ns, o.block_number)),
                Some((ns, height)) => {
                    if *height != o.block_number {
                        key_collisions += 1;
                        seen.ambiguous = true;
                    }
                    *ns = (*ns).min(o.recv_ns);
                }
            },
            Source::PublicWs => seen.ws = Some(seen.ws.map_or(o.recv_ns, |ns| ns.min(o.recv_ns))),
        }
    }
    let mut out = Pairing {
        key_collisions,
        ..Pairing::default()
    };
    for (block_time_ms, seen) in by_time {
        if seen.ambiguous {
            out.ambiguous += 1;
            continue;
        }
        match (seen.node, seen.ws) {
            (Some((node_ns, block_number)), Some(ws_ns)) => out.pairs.push(Pair {
                block_time_ms,
                block_number,
                node_ns,
                ws_ns,
            }),
            (Some(_), None) => out.node_only += 1,
            (None, Some(_)) => out.ws_only += 1,
            (None, None) => {}
        }
    }
    out
}

/// Distribution of the node's lead over the public `WebSocket`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// Blocks seen by both.
    pub matched: u64,
    /// Blocks seen only on the node.
    pub node_only: u64,
    /// Blocks seen only on the public `WebSocket`.
    pub ws_only: u64,
    /// Join-key collisions.
    pub key_collisions: u64,
    /// Ambiguous block-time keys, excluded from the distribution.
    pub ambiguous: u64,
    /// Matched blocks where the node arrived strictly first.
    pub node_first: u64,
    /// Nearest-rank percentiles of the node lead in ns; `None` when empty.
    pub min_ns: Option<i64>,
    /// p50.
    pub p50_ns: Option<i64>,
    /// p90.
    pub p90_ns: Option<i64>,
    /// p99.
    pub p99_ns: Option<i64>,
    /// p99.9.
    pub p999_ns: Option<i64>,
    /// Maximum.
    pub max_ns: Option<i64>,
    /// Matched blocks grouped by the UTC hour of their block time, ascending;
    /// hours with no matched block are omitted. Catches drift over a run.
    pub per_hour: Vec<HourBucket>,
}

/// The node lead for blocks whose block time falls in one UTC hour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HourBucket {
    /// Start of the hour, Unix milliseconds.
    pub hour_start_ms: u64,
    /// Sample count for this hour.
    pub matched: u64,
    /// Nearest-rank median lead, ns.
    pub p50_ns: i64,
    /// p99 lead, ns (weak below ~100 samples).
    pub p99_ns: i64,
    /// Maximum lead, ns.
    pub max_ns: i64,
}

/// Summarises a pairing.
#[must_use]
pub fn summarize(p: &Pairing) -> Summary {
    let mut leads: Vec<i64> = p.pairs.iter().map(Pair::node_lead_ns).collect();
    leads.sort_unstable();
    let q = |per_mille| percentile_sorted(&leads, per_mille);
    Summary {
        matched: u64::try_from(leads.len()).unwrap_or(u64::MAX),
        node_only: p.node_only,
        ws_only: p.ws_only,
        key_collisions: p.key_collisions,
        ambiguous: p.ambiguous,
        node_first: u64::try_from(leads.iter().filter(|&&l| l > 0).count()).unwrap_or(u64::MAX),
        min_ns: leads.first().copied(),
        p50_ns: q(500),
        p90_ns: q(900),
        p99_ns: q(990),
        p999_ns: q(999),
        max_ns: leads.last().copied(),
        per_hour: per_hour(&p.pairs),
    }
}

const HOUR_MS: u64 = 3_600_000;

fn per_hour(pairs: &[Pair]) -> Vec<HourBucket> {
    let mut by_hour: BTreeMap<u64, Vec<i64>> = BTreeMap::new();
    for p in pairs {
        by_hour
            .entry(p.block_time_ms - p.block_time_ms % HOUR_MS)
            .or_default()
            .push(p.node_lead_ns());
    }
    by_hour
        .into_iter()
        .filter_map(|(hour_start_ms, mut v)| {
            v.sort_unstable();
            Some(HourBucket {
                hour_start_ms,
                matched: u64::try_from(v.len()).unwrap_or(u64::MAX),
                p50_ns: percentile_sorted(&v, 500)?,
                p99_ns: percentile_sorted(&v, 990)?,
                max_ns: *v.last()?,
            })
        })
        .collect()
}

/// Nearest-rank percentile of an ascending-sorted slice, `per_mille` in
/// 0..=1000 (500 = median, 999 = p99.9). `None` for an empty slice.
#[must_use]
pub fn percentile_sorted(sorted: &[i64], per_mille: u32) -> Option<i64> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    // Nearest rank: ceil(q * n / 1000), at least 1, at most n.
    let q = usize::try_from(per_mille.min(1000)).unwrap_or(1000);
    let rank = (q * n).div_ceil(1000).clamp(1, n);
    sorted.get(rank - 1).copied()
}

/// Error reading an observation file.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// I/O failure.
    #[error("read failed: {0}")]
    Io(#[from] std::io::Error),
    /// A line that is not an [`Observation`].
    #[error("line {line}: {msg}")]
    Bad {
        /// 1-based line number.
        line: usize,
        /// Parser message.
        msg: String,
    },
}

/// Reads [`Observation`]s from JSON Lines (blank lines skipped).
pub fn read_observations<R: std::io::BufRead>(r: R) -> Result<Vec<Observation>, ReadError> {
    let mut out = Vec::new();
    for (i, line) in r.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let o = serde_json::from_str(&line).map_err(|e| ReadError::Bad {
            line: i + 1,
            msg: e.to_string(),
        })?;
        out.push(o);
    }
    Ok(out)
}
