//! Realistic inputs shared by the allocation gates (`tests/alloc.rs`) and the
//! benchmark (`benches/parse.rs`).
//!
//! The node lines follow the block-info schema hl-node writes
//! (`{local_time, block_time, block_number, events}`); the events are
//! synthetic but shaped like order-status / book-diff records (nested
//! objects, hex addresses, decimal strings). The `WebSocket` frames follow the
//! captured fixtures in `tests/fixtures`.
#![allow(dead_code, unreachable_pub)]

use std::fmt::Write as _;

/// A node line for block `height` carrying `events` events.
pub fn node_line(height: u64, events: usize) -> String {
    let ms = height % 1000;
    let sec = height / 1000 % 60;
    let mut s = String::new();
    let _ = write!(
        s,
        r#"{{"local_time":"2025-07-27T08:50:{sec:02}.{:03}741319","block_time":"2025-07-27T08:50:{sec:02}.{ms:03}000","block_number":{height},"events":["#,
        (ms + 283) % 1000
    );
    for i in 0..events {
        if i > 0 {
            s.push(',');
        }
        let oid = height * 1000 + u64::try_from(i).unwrap_or(0);
        let _ = write!(
            s,
            r#"{{"user":"0x31ca8395cf837de08b24da3f660e77761dfb974b","oid":{oid},"coin":"BTC","side":"{}","px":"62{:03}.0","raw_book_diff":{{"new":{{"sz":"0.{:05}"}}}},"status":"open","order":{{"limitPx":"62{:03}.0","origSz":"0.{:05}","timestamp":1753606210{ms:03},"reduceOnly":false,"cloid":null,"children":[]}}}}"#,
            if i % 2 == 0 { "B" } else { "A" },
            i % 1000,
            i % 100_000,
            i % 1000,
            i % 100_000,
        );
    }
    s.push_str("]}");
    s
}

/// An `l2Book` frame with `levels` levels per side at block time `time`.
pub fn l2book_frame(time: u64, levels: usize) -> String {
    let mut s = format!(r#"{{"channel":"l2Book","data":{{"coin":"BTC","time":{time},"levels":["#);
    for side in 0..2 {
        s.push_str(if side == 0 { "[" } else { ",[" });
        for i in 0..levels {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(
                s,
                r#"{{"px":"62{:03}.0","sz":"{}.{:04}","n":{}}}"#,
                200 + i + side * 50,
                i + 1,
                i * 37 % 10_000,
                i % 60 + 1
            );
        }
        s.push(']');
    }
    s.push_str("]}}");
    s
}

/// A `trades` frame with `trades` trades spread over `distinct` block times
/// starting at `time`.
pub fn trades_frame(time: u64, trades: usize, distinct: u64) -> String {
    let mut s = String::from(r#"{"channel":"trades","data":["#);
    for i in 0..trades {
        if i > 0 {
            s.push(',');
        }
        let n = u64::try_from(i).unwrap_or(0);
        let _ = write!(
            s,
            r#"{{"coin":"BTC","side":"{}","px":"62253.0","sz":"0.01844","time":{},"hash":"0x3d40b19f660f94143eba04492ab35e0201e200850102b2e6e1095cf225036dfe","tid":{}}}"#,
            if i % 2 == 0 { "B" } else { "A" },
            time + (n % distinct.max(1)) * 70,
            87_336_502_210_836 + n
        );
    }
    s.push_str("]}");
    s
}
