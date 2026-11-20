//! The reference parsers: the `serde_json::Value` bodies that `hl-latency`
//! shipped before the zero-allocation rewrite, moved here unchanged.
//!
//! They are the oracle for the equivalence proptests (`tests/parse_equiv.rs`)
//! and the "before" side of the allocation counts (`tests/alloc.rs`) and of
//! the benchmark (`benches/parse.rs`, which includes this file with
//! `#[path]`). Do not optimise them: their value is that they did not change.
#![allow(dead_code, unreachable_pub)]

use hl_latency::{parse_naive_utc_ms, Observation, ParseError, Source};
use serde_json::Value;

fn malformed(why: &str) -> ParseError {
    ParseError::Malformed(why.to_owned())
}

/// `hl_latency::parse_node_line` as it was at commit 00c4d3b.
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

/// `hl_latency::parse_ws_message` as it was at commit 00c4d3b.
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
