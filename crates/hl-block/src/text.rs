//! Integer-only time parsing and message normalisation.

/// A block time that is not `YYYY-MM-DDTHH:MM:SS[.f{1,9}][Z]` from 1970 on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a YYYY-MM-DDTHH:MM:SS[.fffffffff] time from 1970 on")]
pub struct TimeError;

/// Parses `YYYY-MM-DDTHH:MM:SS[.f{1,9}][Z]` (naive UTC) into ns since the
/// Unix epoch. Rejects anything else, including dates before 1970.
#[allow(clippy::many_single_char_names)]
pub fn parse_time_ns(s: &str) -> Result<u64, TimeError> {
    let s = s.strip_suffix('Z').unwrap_or(s);
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return Err(TimeError);
    }
    let num = |r: std::ops::Range<usize>| -> Result<u64, TimeError> {
        let part = s.get(r).ok_or(TimeError)?;
        if part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()) {
            return Err(TimeError);
        }
        part.parse().map_err(|_| TimeError)
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let frac = if b.len() > 19 {
        if b[19] != b'.' {
            return Err(TimeError);
        }
        let digits = &s[20..];
        if digits.is_empty() || digits.len() > 9 {
            return Err(TimeError);
        }
        let v = num(20..s.len())?;
        let pad = 9 - u32::try_from(digits.len()).map_err(|_| TimeError)?;
        v * 10u64.pow(pad)
    } else {
        0
    };
    if !(1..=12).contains(&mo)
        || d == 0
        || d > days_in_month(y, mo)
        || h > 23
        || mi > 59
        || sec > 59
        || y < 1970
    {
        return Err(TimeError);
    }
    let days = days_from_civil(y, mo, d);
    Ok(((days * 24 + h) * 60 + mi) * 60_000_000_000 + sec * 1_000_000_000 + frac)
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

/// Days since 1970-01-01 for a valid date with `y >= 1970`.
fn days_from_civil(y: u64, m: u64, d: u64) -> u64 {
    let mut days = 0;
    for yy in 1970..y {
        days += if is_leap(yy) { 366 } else { 365 };
    }
    for mm in 1..m {
        days += days_in_month(y, mm);
    }
    days + d - 1
}

/// Replaces stand-alone numbers with `<n>` and `0x…` hex with `<hex>` so error
/// messages that differ only in an asset id, size or address count together.
/// A digit glued to letters (`v2`) is kept.
#[must_use]
pub fn normalize_message(msg: &str) -> String {
    let b = msg.as_bytes();
    let mut out = String::with_capacity(msg.len());
    let mut i = 0;
    while i < b.len() {
        let prev_alnum = i > 0 && b[i - 1].is_ascii_alphanumeric();
        if !prev_alnum && b[i] == b'0' && b.get(i + 1) == Some(&b'x') {
            let mut j = i + 2;
            while j < b.len() && b[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j > i + 2 && !b.get(j).is_some_and(u8::is_ascii_alphanumeric) {
                out.push_str("<hex>");
                i = j;
                continue;
            }
        }
        if !prev_alnum && b[i].is_ascii_digit() {
            let mut j = i;
            while j < b.len()
                && (b[j].is_ascii_digit()
                    || (b[j] == b'.' && b.get(j + 1).is_some_and(u8::is_ascii_digit)))
            {
                j += 1;
            }
            if !b.get(j).is_some_and(u8::is_ascii_alphanumeric) {
                out.push_str("<n>");
                i = j;
                continue;
            }
        }
        let ch = msg[i..].chars().next().unwrap_or('?');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}
