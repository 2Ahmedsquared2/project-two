//! Float-free wire decimals and Hyperliquid's perp tick/lot rules.
//!
//! The SDK sends prices and sizes as strings made by `float_to_wire`: format with
//! 8 decimals, reject if that rounds, then strip trailing zeros (`Decimal.normalize`).
//! [`Decimal8`] stores the value as an integer count of `1e-8` units, so the wire
//! string is exact by construction and no float ever appears.

use std::fmt;

/// Units per whole number: the wire format carries at most 8 decimals.
const SCALE: u64 = 100_000_000;
/// Perp prices may have at most `MAX_DECIMALS_PERP - szDecimals` decimals.
const MAX_DECIMALS_PERP: u8 = 6;
/// Prices may have at most this many significant figures (integers excepted).
const MAX_SIG_FIGS: u32 = 5;

/// A non-negative decimal with at most 8 decimals, stored as `units * 1e-8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Decimal8(u64);

/// Why a decimal string was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseDecimalError {
    /// Empty, signed, exponent form, a stray character or a dangling dot.
    #[error("not a plain non-negative decimal: {0:?}")]
    Syntax(String),
    /// More than 8 decimals: the SDK's `float_to_wire` would reject it too.
    #[error("more than 8 decimals: {0:?}")]
    TooPrecise(String),
    /// Does not fit `u64` units.
    #[error("out of range: {0:?}")]
    Overflow(String),
}

/// A tick/lot rule could not be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PxRuleError {
    /// `szDecimals` above 6 (prices) or 8 (sizes).
    #[error("szDecimals {0} out of range")]
    SzDecimals(u8),
    /// Rounding up left the `u64` range.
    #[error("rounding overflowed")]
    Overflow,
}

/// Room for `u64::MAX` units printed as a decimal (20 digits, a dot, a spare).
pub(crate) struct WireBuf {
    buf: [u8; 24],
    len: usize,
}

impl WireBuf {
    pub(crate) fn as_str(&self) -> &str {
        // Only ASCII digits and '.' are ever written.
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or_default()
    }
}

impl Decimal8 {
    /// From a count of `1e-8` units.
    #[must_use]
    pub const fn from_units(units: u64) -> Self {
        Self(units)
    }

    /// The count of `1e-8` units.
    #[must_use]
    pub const fn units(self) -> u64 {
        self.0
    }

    /// Parse a plain decimal such as `"2501.5"` or `"0.001"`. No sign, no exponent,
    /// at most 8 decimals, digits required on both sides of a dot.
    pub fn parse(s: &str) -> Result<Self, ParseDecimalError> {
        let syntax = || ParseDecimalError::Syntax(s.to_owned());
        let (int, frac) = match s.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (s, None),
        };
        if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
            return Err(syntax());
        }
        let mut units: u64 = 0;
        for b in int.bytes() {
            units = units
                .checked_mul(10)
                .and_then(|u| u.checked_add(u64::from(b - b'0')))
                .ok_or_else(|| ParseDecimalError::Overflow(s.to_owned()))?;
        }
        units = units
            .checked_mul(SCALE)
            .ok_or_else(|| ParseDecimalError::Overflow(s.to_owned()))?;
        if let Some(frac) = frac {
            if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
                return Err(syntax());
            }
            if frac.len() > 8 {
                return Err(ParseDecimalError::TooPrecise(s.to_owned()));
            }
            let mut place = SCALE / 10;
            let mut f: u64 = 0;
            for b in frac.bytes() {
                f += u64::from(b - b'0') * place;
                place /= 10;
            }
            units = units
                .checked_add(f)
                .ok_or_else(|| ParseDecimalError::Overflow(s.to_owned()))?;
        }
        Ok(Self(units))
    }

    /// The SDK's `float_to_wire` string, written without allocating.
    pub(crate) fn wire(self) -> WireBuf {
        let mut out = WireBuf {
            buf: [0; 24],
            len: 0,
        };
        let int = self.0 / SCALE;
        let mut frac = self.0 % SCALE;
        // Integer part, most significant digit first.
        let mut digits = [0u8; 20];
        let mut n = 0;
        let mut i = int;
        loop {
            digits[n] = b'0' + digit(i % 10);
            n += 1;
            i /= 10;
            if i == 0 {
                break;
            }
        }
        for d in digits[..n].iter().rev() {
            out.buf[out.len] = *d;
            out.len += 1;
        }
        if frac != 0 {
            out.buf[out.len] = b'.';
            out.len += 1;
            let mut place = SCALE / 10;
            while frac != 0 {
                out.buf[out.len] = b'0' + digit(frac / place);
                out.len += 1;
                frac %= place;
                place /= 10;
            }
        }
        out
    }

    /// The SDK's `float_to_wire` string.
    #[must_use]
    pub fn to_wire(self) -> String {
        self.wire().as_str().to_owned()
    }

    /// Round a perp price the way the SDK examples do,
    /// `round(float(f"{px:.5g}"), 6 - szDecimals)`, but on the exact decimal:
    /// first to 5 significant figures, then to `6 - szDecimals` decimals, each
    /// step half-to-even. (On exact ties the float version depends on binary
    /// representation; the goldens exclude ties.)
    pub fn round_px_perp(self, sz_decimals: u8) -> Result<Self, PxRuleError> {
        let tick = perp_tick(sz_decimals)?;
        if self.0 == 0 {
            return Ok(self);
        }
        let digits = self.0.ilog10() + 1;
        let sig_quantum = 10u64.pow(digits.saturating_sub(MAX_SIG_FIGS));
        let step1 = round_half_even(self.0, sig_quantum)?;
        Ok(Self(round_half_even(step1, tick)?))
    }

    /// Whether Hyperliquid accepts this as a perp price: at most `6 - szDecimals`
    /// decimals, and at most 5 significant figures unless it is an integer.
    #[must_use]
    pub fn is_valid_px_perp(self, sz_decimals: u8) -> bool {
        let Ok(tick) = perp_tick(sz_decimals) else {
            return false;
        };
        self.0.is_multiple_of(tick)
            && (self.0.is_multiple_of(SCALE) || sig_figs(self.0) <= MAX_SIG_FIGS)
    }

    /// Round a size down to the lot `10^-szDecimals` (down, so a notional target
    /// is never exceeded; the probe sizes for ~$12 so it stays above $10).
    pub fn floor_to_lot(self, sz_decimals: u8) -> Result<Self, PxRuleError> {
        let lot = lot(sz_decimals)?;
        Ok(Self(self.0 - self.0 % lot))
    }

    /// Whether this size is a whole number of lots `10^-szDecimals`.
    #[must_use]
    pub fn is_valid_sz(self, sz_decimals: u8) -> bool {
        lot(sz_decimals).is_ok_and(|lot| self.0.is_multiple_of(lot))
    }
}

impl fmt::Display for Decimal8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wire().as_str())
    }
}

fn digit(d: u64) -> u8 {
    // d < 10 by construction.
    u8::try_from(d).unwrap_or(0)
}

/// Tick in units for a perp with `sz_decimals`: `10^-(6 - szDecimals)`.
fn perp_tick(sz_decimals: u8) -> Result<u64, PxRuleError> {
    if sz_decimals > MAX_DECIMALS_PERP {
        return Err(PxRuleError::SzDecimals(sz_decimals));
    }
    Ok(10u64.pow(2 + u32::from(sz_decimals)))
}

/// Lot in units: `10^-szDecimals`.
fn lot(sz_decimals: u8) -> Result<u64, PxRuleError> {
    if sz_decimals > 8 {
        return Err(PxRuleError::SzDecimals(sz_decimals));
    }
    Ok(10u64.pow(8 - u32::from(sz_decimals)))
}

fn sig_figs(mut units: u64) -> u32 {
    if units == 0 {
        return 0;
    }
    let digits = units.ilog10() + 1;
    let mut trailing = 0;
    while units.is_multiple_of(10) {
        units /= 10;
        trailing += 1;
    }
    digits - trailing
}

fn round_half_even(x: u64, q: u64) -> Result<u64, PxRuleError> {
    let down = x - x % q;
    let rem = x % q;
    let half = q / 2;
    // q is 1 or a power of ten, so q is even whenever q > 1.
    let up = rem > half || (rem == half && q > 1 && (down / q) % 2 == 1);
    if up {
        down.checked_add(q).ok_or(PxRuleError::Overflow)
    } else {
        Ok(down)
    }
}
