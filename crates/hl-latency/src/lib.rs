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
//! Parsing on the tick path does not allocate, with one known exception: the
//! strict parsers ([`parse_node_line`], [`parse_ws_message_into`]) allocate
//! when the input contains a backslash-escaped string, because `serde_json`
//! unescapes it into a buffer it owns. The synthetic node lines and captured
//! `WebSocket` frames under test contain none; how often real hl-node lines
//! do is NOT YET MEASURED and is to be counted once real lines exist.
//! [`scan_node_header`] never allocates.
//!
//! The crate is I/O-free: parsing, pairing and statistics only. The capture
//! side (tailing the node's files, holding the `WebSocket`) is a separate
//! binary that writes [`Observation`]s as JSON Lines.

use std::collections::BTreeMap;

use serde::de::DeserializeSeed as _;
use serde::{Deserialize, Serialize};

mod strict;
mod strict_drop;

pub use strict_drop::{StrictDrop, StrictDropError, StrictFailures, StrictReadError};

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
    /// Date (`YYYYMMDD`) of the hourly node file the line was read from.
    /// With [`Self::file_hour`] and [`Self::line_offset`] it names exactly
    /// one line of the node's output, so an offline check of those files
    /// (`hl-strict-pass`) can be matched to this observation. `hl-capture`
    /// writes all three or none; absent for `WebSocket` arrivals and in
    /// files captured before the fields existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_date: Option<u32>,
    /// Hour (0 to 23) of that hourly file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_hour: Option<u8>,
    /// Byte offset, from 0, of the line's first byte in that file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_offset: Option<u64>,
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
    naive_utc_ms(s.as_bytes()).ok_or_else(|| ParseError::BadTimestamp(s.to_owned()))
}

/// [`parse_naive_utc_ms`] on bytes, `None` for anything it rejects.
fn naive_utc_ms(b: &[u8]) -> Option<u64> {
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
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
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, min, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
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
        _ => return None,
    };
    if year < 1970
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || min > 59
        || sec > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(((days * 24 + hour) * 60 + min) * 60_000 + sec * 1000 + millis)
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
///
/// This is the strict parse: the whole line is validated as JSON, a repeated
/// key takes its last value, and the result (errors and their text included)
/// is what parsing into a `serde_json::Value` and looking the two fields up
/// gives. It builds no tree and does not allocate on the success path, unless
/// the line contains backslash-escaped strings (`serde_json` unescapes those
/// into a buffer it owns; hl-node writes none). Its cost still grows with the
/// line; when only the header is needed, try [`scan_node_header`] first.
pub fn parse_node_line(line: &str, recv_ns: u64) -> Result<Observation, ParseError> {
    let mut de = serde_json::Deserializer::from_str(line);
    let fields = strict::NodeLine
        .deserialize(&mut de)
        .and_then(|fields| de.end().map(|()| fields))
        .map_err(|e| malformed(&e.to_string()))?;
    let block_time = fields
        .block_time
        .ok_or_else(|| malformed("block_time missing or not a string"))?;
    let block_number = fields
        .block_number
        .ok_or_else(|| malformed("block_number missing or not a u64"))?;
    Ok(Observation {
        source: Source::Node,
        block_time_ms: block_time?,
        block_number: Some(block_number),
        recv_ns,
        file_date: None,
        file_hour: None,
        line_offset: None,
    })
}

/// A whole line of the node's output: bytes whose terminating newline the
/// reader has already seen.
///
/// [`scan_node_header`] takes only this type, so "complete lines only" is
/// enforced by the compiler instead of by a comment: a caller holding a
/// partial read has to write [`CompleteLine::assume_newline_seen`] to get
/// one, and that name is the claim. There is no `From<&[u8]>`, `Deref` or
/// other implicit conversion, on purpose.
///
/// Zero cost: a transparent wrapper round the slice, `Copy`, no check at
/// construction (the newline itself may or may not be included; the type
/// records what the caller saw, not what the bytes contain).
///
/// A raw slice is not accepted:
///
/// ```compile_fail,E0308
/// let line: &[u8] = br#"{"local_time":"x","block_time":"2025-07-27T08:50:10.051000","block_number":1}"#;
/// let _ = hl_latency::scan_node_header(line);
/// ```
///
/// The caller says so explicitly:
///
/// ```
/// use hl_latency::{scan_node_header, CompleteLine};
/// let line: &[u8] = br#"{"local_time":"x","block_time":"2025-07-27T08:50:10.051000","block_number":1}"#;
/// let line = CompleteLine::assume_newline_seen(line);
/// assert_eq!(scan_node_header(line), Some((1_753_606_210_051, 1)));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct CompleteLine<'a>(&'a [u8]);

impl<'a> CompleteLine<'a> {
    /// Wraps `bytes` as a complete line.
    ///
    /// By calling this the caller states that it has **seen the newline
    /// that terminates this line** (or the end of a finished file): `bytes`
    /// is everything the writer wrote for the line, not the front of a read
    /// still in progress. Nothing is checked here and nothing can be; a
    /// partial read passed through this constructor can make
    /// [`scan_node_header`] answer for a line that was cut right after a
    /// nested `}`.
    ///
    /// `bytes` may include the trailing `\n` (and a `\r` before it) or not.
    #[must_use]
    pub const fn assume_newline_seen(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    /// The line's bytes, as given to [`CompleteLine::assume_newline_seen`].
    #[must_use]
    pub const fn as_bytes(self) -> &'a [u8] {
        self.0
    }
}

/// Reads `(block_time_ms, block_number)` from the start of a node line
/// without parsing the rest: the fast path for the node feed, constant cost
/// however many events the line carries, no allocation.
///
/// **For stamping and routing only.** Use the result to timestamp an arrival
/// and to decide where a line goes. Anything a strategy acts on (prices,
/// sizes, fills) must come from the strict parse: [`parse_node_line`] is the
/// validator, and it runs off the tick path.
///
/// It answers only for a complete line that starts with the exact prefix
/// hl-node writes, byte for byte, with no whitespace:
///
/// ```text
/// {"local_time":"<no escapes>","block_time":"<timestamp>","block_number":<digits>
/// ```
///
/// followed by `,` or `}`. The timestamp must be one [`parse_naive_utc_ms`]
/// accepts and the digits a `u64` without a leading zero. The line's last
/// byte, not counting trailing JSON whitespace (space, tab, `\r`, `\n`),
/// must be `}`. For anything else it returns `None`, and the caller falls
/// back to [`parse_node_line`], which either parses the line or says what is
/// wrong with it.
///
/// # Contract
///
/// The argument is a [`CompleteLine`]: the type is how "call this after the
/// reader has seen the line's newline, never on a partial read" is
/// enforced. A `Some` is a statement about the prefix and the last byte,
/// nothing in between:
///
/// - End of line: a line cut short gives `None`, unless the cut lands right
///   after a nested `}` (the end of an event object) or inside a string
///   whose last byte is `}`, which looks like a complete line. The caller's
///   [`CompleteLine`] claim rules out a partial read.
/// - What the claim does not rule out is a line the *writer* tore: a torn
///   event followed by a newline, or a torn line with the next line joined
///   onto it (the scan then reports the torn line's header and the next
///   block's is lost). In every such case [`parse_node_line`] returns an
///   error, so a consumer that pairs on a scan stamp must drop it when the
///   strict parse of the same line fails, and count how often that happens.
/// - The tail between the header and the closing `}` is not read. A line
///   corrupt after `block_number` that still ends with `}` gives `Some`,
///   where [`parse_node_line`] gives an error.
/// - The first occurrence of each key is trusted. If a later duplicate
///   `block_time` or `block_number` follows, this function reports the first
///   and [`parse_node_line`] the last (or an error, if the duplicate is not
///   valid).
/// - `local_time` is skipped, not validated.
///
/// For a line that is a valid JSON object with unique keys, `Some((t, n))`
/// means [`parse_node_line`] returns `Ok` with the same block time and
/// height. Each case above is pinned by a test in `tests/parse_equiv.rs`.
///
/// ```
/// use hl_latency::{scan_node_header, CompleteLine};
/// let line = br#"{"local_time":"2025-07-27T08:50:10.334741319","block_time":"2025-07-27T08:50:10.051000","block_number":676607012,"events":[]}"#;
/// let scan = |b| scan_node_header(CompleteLine::assume_newline_seen(b));
/// assert_eq!(scan(line), Some((1_753_606_210_051, 676_607_012)));
/// // Not a node line, and a node line cut short: both fall back.
/// assert_eq!(scan(br#"{"block_number":1}"#), None);
/// assert_eq!(scan(&line[..line.len() - 1]), None);
/// ```
#[must_use]
pub fn scan_node_header(line: CompleteLine<'_>) -> Option<(u64, u64)> {
    /// Splits at the closing quote of a string with no escapes.
    fn plain_string(b: &[u8]) -> Option<(&[u8], &[u8])> {
        let end = b.iter().position(|&c| c == b'"' || c == b'\\')?;
        if b[end] == b'\\' {
            return None;
        }
        Some((&b[..end], &b[end + 1..]))
    }
    let line = line.as_bytes();
    let rest = line.strip_prefix(br#"{"local_time":""#)?;
    let (_local_time, rest) = plain_string(rest)?;
    let rest = rest.strip_prefix(br#","block_time":""#)?;
    let (block_time, rest) = plain_string(rest)?;
    let block_time_ms = naive_utc_ms(block_time)?;
    let rest = rest.strip_prefix(br#","block_number":"#)?;
    let len = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    let (digits, rest) = rest.split_at(len);
    // The terminator proves the number was not cut short and is an integer
    // (not followed by `.`, `e` or more digits).
    if !matches!(rest.first(), Some(b',' | b'}')) {
        return None;
    }
    if digits.is_empty() || (digits.len() > 1 && digits[0] == b'0') {
        return None;
    }
    let block_number = digits.iter().try_fold(0u64, |acc, &c| {
        acc.checked_mul(10)?.checked_add(u64::from(c - b'0'))
    })?;
    // End-of-line check: a complete line closes its object. Only trailing
    // JSON whitespace (the newline, a `\r` before it) may follow.
    let last = line
        .iter()
        .rev()
        .find(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n'))?;
    if *last != b'}' {
        return None;
    }
    Some((block_time_ms, block_number))
}

/// Parses one public `WebSocket` message into the block times it proves the
/// sender had seen: `l2Book` gives one (`data.time`), `trades` one per
/// distinct `time` in message order. Other channels (subscription acks,
/// pongs) give none.
///
/// Allocates the returned `Vec`; on a hot path use
/// [`parse_ws_message_into`] with a reused buffer.
pub fn parse_ws_message(msg: &str, recv_ns: u64) -> Result<Vec<Observation>, ParseError> {
    let mut out = Vec::new();
    parse_ws_message_into(msg, recv_ns, &mut out)?;
    Ok(out)
}

/// [`parse_ws_message`] into a buffer the caller keeps: `out` is cleared,
/// then filled with the message's observations. On an error `out` is left
/// empty.
///
/// Once `out` has grown to the largest message seen, the success path does
/// not allocate (unless the frame contains backslash-escaped strings, see
/// [`parse_node_line`]). The whole frame is validated as JSON; `channel` and
/// `data` may come in either order, and a repeated key takes its last value.
pub fn parse_ws_message_into(
    msg: &str,
    recv_ns: u64,
    out: &mut Vec<Observation>,
) -> Result<(), ParseError> {
    use strict::{Channel, Data};
    out.clear();
    let result = (|| {
        let mut de = serde_json::Deserializer::from_str(msg);
        let (channel, data) = strict::WsFrame {
            out: &mut *out,
            recv_ns,
        }
        .deserialize(&mut de)
        .and_then(|frame| de.end().map(|()| frame))
        .map_err(|e| malformed(&e.to_string()))?;
        let no_time = || malformed("time missing or not a u64");
        match channel {
            Channel::L2Book => match data {
                None => Err(malformed("l2Book without data")),
                Some(Data::Object(Some(time))) => {
                    out.clear();
                    out.push(Observation {
                        source: Source::PublicWs,
                        block_time_ms: time,
                        block_number: None,
                        recv_ns,
                        file_date: None,
                        file_hour: None,
                        line_offset: None,
                    });
                    Ok(())
                }
                Some(_) => Err(no_time()),
            },
            Channel::Trades => match data {
                // The distinct trade times are already in `out`.
                Some(Data::Array { all_timed: true }) => Ok(()),
                Some(Data::Array { all_timed: false }) => Err(no_time()),
                _ => Err(malformed("trades data is not an array")),
            },
            Channel::Other => {
                out.clear();
                Ok(())
            }
        }
    })();
    if result.is_err() {
        out.clear();
    }
    result
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

/// Reads [`Observation`]s from JSON Lines (blank lines skipped). A line with
/// only some of `file_date`, `file_hour` and `line_offset` is refused:
/// together they name one line of the node's output, apart they name none.
pub fn read_observations<R: std::io::BufRead>(r: R) -> Result<Vec<Observation>, ReadError> {
    let mut out = Vec::new();
    for (i, line) in r.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let bad = |msg: String| ReadError::Bad { line: i + 1, msg };
        let o: Observation = serde_json::from_str(&line).map_err(|e| bad(e.to_string()))?;
        let located = [
            o.file_date.is_some(),
            o.file_hour.is_some(),
            o.line_offset.is_some(),
        ];
        if located != [located[0]; 3] {
            return Err(bad(
                "file_date, file_hour and line_offset must all be present or all absent".to_owned(),
            ));
        }
        out.push(o);
    }
    Ok(out)
}
