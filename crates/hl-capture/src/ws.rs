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
    /// Frames `hl_latency::parse_ws_message` rejected.
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

/// `(channel, coin)` of a data frame; `None` for other channels.
fn subscription(frame: &str) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(frame).ok()?;
    let channel = v.get("channel")?.as_str()?;
    let data = v.get("data")?;
    let coin = match channel {
        "l2Book" => data.get("coin"),
        "trades" => data.get(0).and_then(|t| t.get("coin")),
        _ => return None,
    }
    .and_then(Value::as_str)
    .unwrap_or_default();
    Some((channel.to_owned(), coin.to_owned()))
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
        if subscription(frame).is_some_and(|key| self.seen.insert(key)) {
            self.stats.snapshots_dropped += 1;
            return Vec::new();
        }
        obs
    }

    /// Counters so far.
    #[must_use]
    pub fn stats(&self) -> WsStats {
        self.stats
    }
}
