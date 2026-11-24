//! Exact decimal string to fixed-point integer, no floats, no allocation.

/// Parse a plain non-negative decimal (`digits[.digits]`) into units of
/// `10^-scale`. `None` for anything else: signs, exponents, empty parts,
/// more significant fractional digits than `scale` (trailing zeros beyond it
/// are accepted), or a value that does not fit in `u64`.
#[must_use]
pub fn parse_fixed(s: &[u8], scale: u32) -> Option<u64> {
    let dot = s.iter().position(|&b| b == b'.');
    let (int, frac) = match dot {
        Some(i) => (s.get(..i)?, s.get(i + 1..)?),
        None => (s, &[][..]),
    };
    if int.is_empty() || (dot.is_some() && frac.is_empty()) {
        return None;
    }
    let mut v: u64 = 0;
    for &b in int {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
    }
    let mut left = scale;
    for &b in frac {
        if !b.is_ascii_digit() {
            return None;
        }
        if left == 0 {
            if b != b'0' {
                return None;
            }
            continue;
        }
        v = v.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
        left -= 1;
    }
    while left > 0 {
        v = v.checked_mul(10)?;
        left -= 1;
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::parse_fixed;

    #[test]
    fn known_answers() {
        assert_eq!(parse_fixed(b"0.041673", 8), Some(4_167_300));
        assert_eq!(parse_fixed(b"61480", 8), Some(6_148_000_000_000));
        assert_eq!(parse_fixed(b"2923.0", 1), Some(29230));
        assert_eq!(parse_fixed(b"1.2300", 2), Some(123));
        assert_eq!(parse_fixed(b"1.23", 1), None);
        assert_eq!(parse_fixed(b"", 2), None);
        assert_eq!(parse_fixed(b".5", 2), None);
        assert_eq!(parse_fixed(b"5.", 2), None);
        assert_eq!(parse_fixed(b"-1", 2), None);
        assert_eq!(parse_fixed(b"1e5", 2), None);
        assert_eq!(parse_fixed(b"18446744073709551615", 0), Some(u64::MAX));
        assert_eq!(parse_fixed(b"18446744073709551616", 0), None);
        assert_eq!(parse_fixed(b"184467440737095516.15", 2), Some(u64::MAX));
        assert_eq!(parse_fixed(b"184467440737095516.16", 2), None);
    }
}
