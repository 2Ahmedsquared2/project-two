//! Exact decimal-string <-> integer-unit conversion. No floats.
//!
//! Hyperliquid sends prices and sizes as decimal strings (`"100.5"`,
//! `"0.00012"`). The dataset stores them as integers in units of
//! `10^-decimals` (ticks for prices, lots for sizes) and keeps the strings
//! beside them for audit. These two functions are the only place that
//! conversion happens.

/// Why a decimal string could not be converted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, thiserror::Error)]
pub enum DecimalError {
    /// Not a plain non-negative decimal: digits, optionally `.` and more digits.
    /// Signs, exponents, whitespace, `NaN` and bare `.` are all rejected.
    #[error("not a plain non-negative decimal")]
    Invalid,
    /// More significant fractional digits than the grid allows.
    #[error("more decimal places than the grid allows")]
    TooPrecise,
    /// The value does not fit in a `u64` number of units.
    #[error("value does not fit in u64 units")]
    Overflow,
}

/// Parse a decimal string into integer units of `10^-decimals`.
///
/// Trailing fractional zeros beyond `decimals` are accepted (`"1.2340"` with
/// 3 decimals is 1234); any other extra digit is [`DecimalError::TooPrecise`].
pub fn parse_units(s: &str, decimals: u32) -> Result<u64, DecimalError> {
    let (int, frac) = match s.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (s, None),
    };
    let all_digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(int) || frac.is_some_and(|f| !all_digits(f)) {
        return Err(DecimalError::Invalid);
    }
    let frac = frac.unwrap_or("");
    let keep = usize::try_from(decimals).map_err(|_| DecimalError::Overflow)?;
    let (kept, extra) = frac.split_at(frac.len().min(keep));
    if extra.bytes().any(|b| b != b'0') {
        return Err(DecimalError::TooPrecise);
    }
    let scale = 10u64.checked_pow(decimals).ok_or(DecimalError::Overflow)?;
    let int_units = digits_value(int)?
        .checked_mul(scale)
        .ok_or(DecimalError::Overflow)?;
    // `kept` has at most `decimals` digits; pad it on the right to exactly `decimals`.
    let pad = u32::try_from(keep - kept.len()).map_err(|_| DecimalError::Overflow)?;
    let frac_units = if kept.is_empty() {
        0
    } else {
        digits_value(kept)?
            .checked_mul(10u64.checked_pow(pad).ok_or(DecimalError::Overflow)?)
            .ok_or(DecimalError::Overflow)?
    };
    int_units
        .checked_add(frac_units)
        .ok_or(DecimalError::Overflow)
}

fn digits_value(digits: &str) -> Result<u64, DecimalError> {
    digits.bytes().try_fold(0u64, |acc, b| {
        acc.checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(b - b'0')))
            .ok_or(DecimalError::Overflow)
    })
}

/// Format integer units of `10^-decimals` as the shortest decimal string
/// (no trailing fractional zeros, no trailing `.`).
#[must_use]
pub fn format_units(units: u64, decimals: u32) -> String {
    let d = usize::try_from(decimals).unwrap_or(usize::MAX);
    if d == 0 {
        return units.to_string();
    }
    let digits = format!("{units:0>width$}", width = d.saturating_add(1));
    let (int, frac) = digits.split_at(digits.len() - d);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        int.to_owned()
    } else {
        format!("{int}.{frac}")
    }
}
