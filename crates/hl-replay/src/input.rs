//! The lines a run replays: generated, or loaded from a recording.

use std::fmt::Write as _;
use std::path::Path;

use crate::ReplayError;

/// One synthetic node output line in the block-info schema
/// (`local_time`, `block_time`, `block_number`, `events`), newline included,
/// with `events` order-book diff events and `seq` as its block number.
#[must_use]
pub fn node_line(seq: u64, events: usize) -> String {
    let mut s = format!(
        r#"{{"local_time":"2026-11-20T10:00:00.123456789","block_time":"2026-11-20T10:00:00.051","block_number":{seq},"events":["#
    );
    for i in 0..events {
        if i > 0 {
            s.push(',');
        }
        // Writing to a `String` cannot fail.
        let _ = write!(
            s,
            r#"{{"user":"0x31ca8395cf837de08b24da3f660e77761dfb974b","oid":{},"coin":"BTC","side":"B","px":"67123.5","raw_book_diff":{{"new":{{"sz":"0.01234"}}}}}}"#,
            300_000_000_000_u64.saturating_add(u64::try_from(i).unwrap_or(u64::MAX))
        );
    }
    s.push_str("]}\n");
    s
}

/// `n` generated lines with block numbers `0..n`.
#[must_use]
pub fn generated_lines(n: usize, events: usize) -> Vec<String> {
    (0..n)
        .map(|i| node_line(u64::try_from(i).unwrap_or(u64::MAX), events))
        .collect()
}

/// Loads a recording of node output lines (one JSON object per line; blank
/// lines are skipped). Every line is parsed once here, so a bad recording is
/// refused before a run starts instead of in the middle of one. Each returned
/// line ends with a newline.
pub fn load_lines(path: &Path) -> Result<Vec<String>, ReplayError> {
    let bad = |line: usize, msg: String| ReplayError::BadInput {
        path: path.display().to_string(),
        line,
        msg,
    };
    let text = std::fs::read_to_string(path).map_err(|e| bad(0, e.to_string()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        hl_latency::parse_node_line(line, 0).map_err(|e| bad(i + 1, e.to_string()))?;
        out.push(format!("{line}\n"));
    }
    if out.is_empty() {
        return Err(bad(0, "no lines to replay".to_owned()));
    }
    Ok(out)
}
