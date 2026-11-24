//! Native hl-node's own per-block timing: `node_fast_block_times` lines
//! `{hardfork_version, height, evm_block_num, block_time,
//! begin_block_wall_time, apply_duration}` (field set read from our node's
//! output, heights 1232430001..; strict: an unknown field is an error).
//!
//! `apply_duration` is seconds as a JSON number (seen as `0.00262916` and as
//! `8.9083e-05`). It is parsed from its raw text into integer nanoseconds,
//! truncating below 1 ns, with no float arithmetic. What exactly native times
//! with it (execution only, or more) is not documented: the report names the
//! field, not an interpretation.

use serde::Deserialize;
use serde_json::value::RawValue;

/// One block's native timing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeTime {
    /// Height.
    pub height: u64,
    /// Block time as written.
    pub block_time: String,
    /// `apply_duration`, ns.
    pub apply_ns: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw<'a> {
    #[allow(dead_code)]
    hardfork_version: u64,
    height: u64,
    #[allow(dead_code)]
    evm_block_num: u64,
    block_time: String,
    #[allow(dead_code)]
    begin_block_wall_time: String,
    #[serde(borrow)]
    apply_duration: &'a RawValue,
}

/// Why a line was rejected.
#[derive(Debug, thiserror::Error)]
pub enum NativeTimeError {
    /// Shape.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// `apply_duration` not a non-negative decimal number of seconds.
    #[error("apply_duration {0:?} is not a non-negative number of seconds")]
    Duration(String),
}

/// Parses one `node_fast_block_times` line.
pub fn parse_line(line: &str) -> Result<NativeTime, NativeTimeError> {
    let r: Raw<'_> = serde_json::from_str(line)?;
    let txt = r.apply_duration.get();
    let apply_ns =
        parse_duration_ns(txt).ok_or_else(|| NativeTimeError::Duration(txt.to_owned()))?;
    Ok(NativeTime {
        height: r.height,
        block_time: r.block_time,
        apply_ns,
    })
}

/// Seconds as decimal text (`d+[.d+][e[+-]d+]`) to integer ns, truncated.
#[must_use]
pub fn parse_duration_ns(s: &str) -> Option<u64> {
    let (mant, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], s[i + 1..].parse::<i32>().ok()?),
        None => (s, 0),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    if int.is_empty()
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    // digits * 10^(exp - frac_len) seconds = digits * 10^(9 + exp - frac_len) ns
    let digits = format!("{int}{frac}");
    let digits = digits.trim_start_matches('0');
    let shift = 9 + exp - i32::try_from(frac.len()).ok()?;
    if digits.is_empty() {
        return Some(0);
    }
    if shift >= 0 {
        let v: u64 = digits.parse().ok()?;
        v.checked_mul(10u64.checked_pow(u32::try_from(shift).ok()?)?)
    } else {
        let cut = usize::try_from(-shift).ok()?;
        if cut >= digits.len() {
            return Some(0);
        }
        digits[..digits.len() - cut].parse().ok()
    }
}
