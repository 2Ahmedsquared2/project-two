//! The three L1 actions this crate signs, and their two encodings: msgpack (what
//! gets hashed) and JSON (what gets posted). Key order and field shapes follow the
//! SDK exactly (`order_request_to_order_wire`, `order_wires_to_order_action`,
//! `Exchange.bulk_cancel`, `Exchange.schedule_cancel`).

use std::io::Write;

use rmp::encode;

use crate::decimal::Decimal8;
use crate::hex::{write_hex, HEX};

/// Time in force of a limit order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tif {
    /// Add liquidity only (post-only): rejected if it would cross.
    Alo,
    /// Good till cancelled.
    Gtc,
    /// Immediate or cancel.
    Ioc,
}

impl Tif {
    /// The wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alo => "Alo",
            Self::Gtc => "Gtc",
            Self::Ioc => "Ioc",
        }
    }
}

/// A 16-byte client order id, sent as `0x` + 32 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cloid(pub [u8; 16]);

impl Cloid {
    fn hex(self) -> [u8; 34] {
        let mut out = [0u8; 34];
        out[0] = b'0';
        out[1] = b'x';
        for (i, b) in self.0.iter().enumerate() {
            out[2 + 2 * i] = HEX[usize::from(b >> 4)];
            out[3 + 2 * i] = HEX[usize::from(b & 0x0f)];
        }
        out
    }
}

/// One limit order. Sent as a single-order `order` action with grouping `"na"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LimitOrder {
    /// Asset index (perps: position in `meta.universe`; spot: 10000 + index).
    pub asset: u32,
    /// Buy (`true`) or sell.
    pub is_buy: bool,
    /// Limit price; see [`Decimal8::round_px_perp`].
    pub px: Decimal8,
    /// Size in coin units; see [`Decimal8::floor_to_lot`].
    pub sz: Decimal8,
    /// Reduce-only flag.
    pub reduce_only: bool,
    /// Time in force.
    pub tif: Tif,
    /// Optional client order id.
    pub cloid: Option<Cloid>,
}

/// An L1 action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Place one limit order.
    Order(LimitOrder),
    /// Cancel one order by exchange order id.
    Cancel {
        /// Asset index.
        asset: u32,
        /// Exchange order id.
        oid: u64,
    },
    /// Dead man's switch: cancel all open orders at `time` (UTC ms, at least 5 s
    /// ahead); `None` clears a scheduled cancel.
    ScheduleCancel {
        /// When to cancel, UTC milliseconds.
        time: Option<u64>,
    },
}

/// The msgpack writer failed (only possible if the sink's `write` fails).
#[derive(Debug, thiserror::Error)]
#[error("msgpack encode: {0}")]
pub struct EncodeError(String);

fn enc<E: std::fmt::Display>(e: E) -> EncodeError {
    EncodeError(e.to_string())
}

impl Action {
    /// Write the action exactly as Python's `msgpack.packb(action)` would.
    pub fn write_msgpack<W: Write>(&self, w: &mut W) -> Result<(), EncodeError> {
        match *self {
            Self::Order(o) => {
                encode::write_map_len(w, 3).map_err(enc)?;
                encode::write_str(w, "type").map_err(enc)?;
                encode::write_str(w, "order").map_err(enc)?;
                encode::write_str(w, "orders").map_err(enc)?;
                encode::write_array_len(w, 1).map_err(enc)?;
                encode::write_map_len(w, if o.cloid.is_some() { 7 } else { 6 }).map_err(enc)?;
                encode::write_str(w, "a").map_err(enc)?;
                encode::write_uint(w, u64::from(o.asset)).map_err(enc)?;
                encode::write_str(w, "b").map_err(enc)?;
                encode::write_bool(w, o.is_buy).map_err(enc)?;
                encode::write_str(w, "p").map_err(enc)?;
                encode::write_str(w, o.px.wire().as_str()).map_err(enc)?;
                encode::write_str(w, "s").map_err(enc)?;
                encode::write_str(w, o.sz.wire().as_str()).map_err(enc)?;
                encode::write_str(w, "r").map_err(enc)?;
                encode::write_bool(w, o.reduce_only).map_err(enc)?;
                encode::write_str(w, "t").map_err(enc)?;
                encode::write_map_len(w, 1).map_err(enc)?;
                encode::write_str(w, "limit").map_err(enc)?;
                encode::write_map_len(w, 1).map_err(enc)?;
                encode::write_str(w, "tif").map_err(enc)?;
                encode::write_str(w, o.tif.as_str()).map_err(enc)?;
                if let Some(c) = o.cloid {
                    let hex = c.hex();
                    encode::write_str(w, "c").map_err(enc)?;
                    encode::write_str(w, std::str::from_utf8(&hex).map_err(enc)?).map_err(enc)?;
                }
                encode::write_str(w, "grouping").map_err(enc)?;
                encode::write_str(w, "na").map_err(enc)?;
            }
            Self::Cancel { asset, oid } => {
                encode::write_map_len(w, 2).map_err(enc)?;
                encode::write_str(w, "type").map_err(enc)?;
                encode::write_str(w, "cancel").map_err(enc)?;
                encode::write_str(w, "cancels").map_err(enc)?;
                encode::write_array_len(w, 1).map_err(enc)?;
                encode::write_map_len(w, 2).map_err(enc)?;
                encode::write_str(w, "a").map_err(enc)?;
                encode::write_uint(w, u64::from(asset)).map_err(enc)?;
                encode::write_str(w, "o").map_err(enc)?;
                encode::write_uint(w, oid).map_err(enc)?;
            }
            Self::ScheduleCancel { time } => {
                encode::write_map_len(w, if time.is_some() { 2 } else { 1 }).map_err(enc)?;
                encode::write_str(w, "type").map_err(enc)?;
                encode::write_str(w, "scheduleCancel").map_err(enc)?;
                if let Some(t) = time {
                    encode::write_str(w, "time").map_err(enc)?;
                    encode::write_uint(w, t).map_err(enc)?;
                }
            }
        }
        Ok(())
    }

    /// Append the action as compact JSON, same key order as the SDK.
    pub(crate) fn write_json(&self, out: &mut String) {
        use std::fmt::Write as _;
        // Writing to a String cannot fail.
        match *self {
            Self::Order(o) => {
                let _ = write!(
                    out,
                    r#"{{"type":"order","orders":[{{"a":{},"b":{},"p":"{}","s":"{}","r":{},"t":{{"limit":{{"tif":"{}"}}}}"#,
                    o.asset,
                    o.is_buy,
                    o.px.wire().as_str(),
                    o.sz.wire().as_str(),
                    o.reduce_only,
                    o.tif.as_str(),
                );
                if let Some(c) = o.cloid {
                    out.push_str(r#","c":""#);
                    out.push_str(std::str::from_utf8(&c.hex()).unwrap_or_default());
                    out.push('"');
                }
                out.push_str(r#"}],"grouping":"na"}"#);
            }
            Self::Cancel { asset, oid } => {
                let _ = write!(
                    out,
                    r#"{{"type":"cancel","cancels":[{{"a":{asset},"o":{oid}}}]}}"#
                );
            }
            Self::ScheduleCancel { time: None } => out.push_str(r#"{"type":"scheduleCancel"}"#),
            Self::ScheduleCancel { time: Some(t) } => {
                let _ = write!(out, r#"{{"type":"scheduleCancel","time":{t}}}"#);
            }
        }
    }
}

/// Append a 20-byte address as `"0x…"` lowercase hex (no quotes).
pub(crate) fn write_address(out: &mut String, addr: &[u8; 20]) {
    out.push_str("0x");
    write_hex(out, addr);
}
