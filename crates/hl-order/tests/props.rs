//! Property tests for the wire decimal format and Hyperliquid's perp tick/lot rules:
//! prices have at most 5 significant figures (integers always allowed) and at most
//! `6 - szDecimals` decimals; sizes are multiples of `10^-szDecimals`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use hl_order::{Decimal8, PxRuleError};
use proptest::prelude::*;

/// Units are 1e-8; keep values below 1e10 so rounding never overflows.
fn units() -> impl Strategy<Value = u64> {
    prop_oneof![
        0u64..1_000,
        0u64..100_000_000,
        0u64..1_000_000_000_000_000,
    ]
}

fn sig_figs(units: u64) -> u32 {
    if units == 0 {
        return 0;
    }
    let digits = units.ilog10() + 1;
    let mut tz = 0;
    let mut u = units;
    while u % 10 == 0 {
        u /= 10;
        tz += 1;
    }
    digits - tz
}

proptest! {
    #[test]
    fn wire_round_trips(u in units()) {
        let d = Decimal8::from_units(u);
        let w = d.to_wire();
        prop_assert_eq!(Decimal8::parse(&w).expect("parse"), d);
        if w.contains('.') {
            prop_assert!(!w.ends_with('0') && !w.ends_with('.'), "not normalised: {}", w);
        }
        prop_assert!(!w.starts_with('-') && !w.contains('e'));
    }

    #[test]
    fn rounded_px_is_valid_and_idempotent(u in 1u64..1_000_000_000_000_000, szd in 0u8..=6) {
        let d = Decimal8::from_units(u);
        let r = d.round_px_perp(szd).expect("round");
        prop_assert!(r.is_valid_px_perp(szd), "{} -> {} szd {}", d, r, szd);
        prop_assert_eq!(r.round_px_perp(szd).expect("round"), r);
    }

    #[test]
    fn rounding_error_is_bounded(u in 1u64..1_000_000_000_000_000, szd in 0u8..=6) {
        let d = Decimal8::from_units(u);
        let r = d.round_px_perp(szd).expect("round");
        let digits = u.ilog10() + 1;
        let q1 = 10u64.pow(digits.saturating_sub(5));
        let q2 = 10u64.pow(2 + u32::from(szd));
        let err = u.abs_diff(r.units());
        prop_assert!(err <= q1 / 2 + q2 / 2 + 1, "{} -> {} err {}", d, r, err);
    }

    #[test]
    fn valid_prices_are_unchanged(mantissa in 1u64..100_000, szd in 0u8..=6, extra in 0u32..6) {
        // mantissa (<= 5 sig figs) times a multiple of the tick: always a valid price.
        let tick = 10u64.pow(2 + u32::from(szd));
        let u = mantissa * tick * 10u64.pow(extra);
        prop_assert!(sig_figs(u) <= 5);
        let d = Decimal8::from_units(u);
        prop_assert!(d.is_valid_px_perp(szd));
        prop_assert_eq!(d.round_px_perp(szd).expect("round"), d);
    }

    #[test]
    fn px_validity_follows_the_rules(u in 1u64..1_000_000_000_000_000, szd in 0u8..=6) {
        let d = Decimal8::from_units(u);
        let tick = 10u64.pow(2 + u32::from(szd));
        let decimals_ok = u % tick == 0;
        let is_integer = u % 100_000_000 == 0;
        let want = decimals_ok && (sig_figs(u) <= 5 || is_integer);
        prop_assert_eq!(d.is_valid_px_perp(szd), want, "{} szd {}", d, szd);
    }

    #[test]
    fn floor_to_lot_is_a_valid_size_below_input(u in units(), szd in 0u8..=8) {
        let d = Decimal8::from_units(u);
        let lot = 10u64.pow(8 - u32::from(szd));
        let f = d.floor_to_lot(szd).expect("floor");
        prop_assert!(f.units() <= u);
        prop_assert!(u - f.units() < lot);
        prop_assert!(f.is_valid_sz(szd));
        prop_assert_eq!(d.is_valid_sz(szd), u % lot == 0);
    }
}

#[test]
fn integer_prices_with_many_sig_figs_are_valid() {
    let d = Decimal8::parse("123456").expect("parse");
    assert!(d.is_valid_px_perp(0));
    assert!(!Decimal8::parse("1234.56").expect("parse").is_valid_px_perp(0));
    assert!(Decimal8::parse("1234.5").expect("parse").is_valid_px_perp(0));
    // 6 - szDecimals = 1 decimal for szDecimals 5 (BTC on mainnet).
    assert!(!Decimal8::parse("0.12").expect("parse").is_valid_px_perp(5));
}

#[test]
fn sz_decimals_out_of_range_is_an_error() {
    let d = Decimal8::parse("1").expect("parse");
    assert_eq!(d.round_px_perp(7), Err(PxRuleError::SzDecimals(7)));
    assert_eq!(d.floor_to_lot(9), Err(PxRuleError::SzDecimals(9)));
    assert!(!d.is_valid_px_perp(7));
}

#[test]
fn parse_rejects_bad_input() {
    for bad in ["", ".", "-1", "1e5", "0.000000001", "1.2.3", "abc", " 1", "184467440738"] {
        assert!(Decimal8::parse(bad).is_err(), "accepted {bad:?}");
    }
    assert_eq!(Decimal8::parse("1.").ok(), None);
    assert_eq!(Decimal8::parse("0.5").expect("parse").units(), 50_000_000);
    assert_eq!(Decimal8::parse("007").expect("parse").to_wire(), "7");
}
