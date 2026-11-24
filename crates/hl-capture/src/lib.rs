//! Capture side of the same-block latency probe (`docs/LATENCY_PROBE.md`).
//!
//! Two arms produce [`hl_latency::Observation`]s for the same process and
//! clock:
//!
//! - **Node arm** ([`NodeTailer`]): follows the newest hourly file our node
//!   writes under a directory such as `~/hl/data/node_raw_book_diffs`, and
//!   stamps every `read` the moment it returns, before any parsing.
//! - **Public `WebSocket` arm** ([`WsFilter`]): turns raw frames, already
//!   stamped by the socket reader, into observations, dropping the snapshot
//!   frame each subscription sends first.
//!
//! The stamp rule (Latency review): a line's `recv_ns` is the clock value read
//! immediately after the `read` call that delivered the line's final byte (its
//! newline) returned. A line is not usable before it is complete, so a line
//! split across two reads carries the second read's stamp.

mod lines;
mod tail;
mod ws;

pub use lines::{LineSplitter, StampedLine, DEFAULT_MAX_LINE};
pub use tail::{hourly_key, newest_hourly_file, NodeTailer, TailStats, MAX_READS_PER_POLL};
pub use ws::{WsFilter, WsStats};
