//! Public `WebSocket` frame filter.

use std::collections::HashSet;

use hl_latency::{parse_ws_message, Observation};
use serde_json::Value;

/// Counters for the `WebSocket` arm.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WsStats {
    /// Frames seen.
    pub frames: u64,
    /// Snapshot frames dropped (first per subscription since the last reset).
    pub snapshots_dropped: u64,
    /// Frames `hl_latency::parse_ws_message` rejected, and `l2Book`/`trades`
    /// frames without one non-empty coin.
    pub bad_frames: u64,
}

/// Turns stamped frames into observations, dropping the first `l2Book` or
/// `trades` frame per coin after each (re)subscribe: it is a snapshot and
/// can be seconds old (observed 2026-11-19: ~15 s for `trades`).
#[derive(Debug, Default)]
pub struct WsFilter {
    seen: HashSet<(String, String)>,
    stats: WsStats,
}

/// Identity of a frame: `Data(channel, coin)` for `l2Book` and `trades`,
/// `Other` for channels that carry no observations, `Invalid` for a data frame
/// whose coin is missing, not a string, empty, or (for `trades`) not the same
/// in every trade.
enum Identity {
    Data(String, String),
    Other,
    Invalid,
}

fn coin_of(d: &Value) -> Option<&str> {
    d.get("coin")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
}

fn identity(frame: &str) -> Identity {
    let Ok(v) = serde_json::from_str::<Value>(frame) else {
        return Identity::Invalid;
    };
    let (channel, coin) = match v.get("channel").and_then(Value::as_str) {
        Some(ch @ "l2Book") => (ch, v.get("data").and_then(coin_of)),
        Some(ch @ "trades") => {
            let trades = v.get("data").and_then(Value::as_array);
            let first = trades.and_then(|t| t.first()).and_then(coin_of);
            let same = trades.is_some_and(|t| t.iter().all(|x| coin_of(x) == first));
            (ch, first.filter(|_| same))
        }
        _ => return Identity::Other,
    };
    match coin {
        Some(c) => Identity::Data(channel.to_owned(), c.to_owned()),
        None => Identity::Invalid,
    }
}

impl WsFilter {
    /// A filter with no subscription seen yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Call on every (re)connect: the next frame of each subscription is a
    /// snapshot again.
    pub fn reset(&mut self) {
        self.seen.clear();
    }

    /// Handles one frame stamped `recv_ns` at socket read.
    pub fn on_frame(&mut self, frame: &str, recv_ns: u64) -> Vec<Observation> {
        self.stats.frames += 1;
        let Ok(obs) = parse_ws_message(frame, recv_ns) else {
            self.stats.bad_frames += 1;
            return Vec::new();
        };
        match identity(frame) {
            Identity::Invalid => {
                self.stats.bad_frames += 1;
                Vec::new()
            }
            Identity::Data(channel, coin) => {
                if self.seen.insert((channel, coin)) {
                    self.stats.snapshots_dropped += 1;
                    Vec::new()
                } else {
                    obs
                }
            }
            Identity::Other => obs,
        }
    }

    /// Counters so far.
    #[must_use]
    pub fn stats(&self) -> WsStats {
        self.stats
    }
}
