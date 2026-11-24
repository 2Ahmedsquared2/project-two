//! Real-data margin cases from our node's blocks 1232430001..1232440000, seeded
//! from our node's translated state at 1232430000. Native's outcome is the answer
//! key only; the decision uses the seeded account and the marks in force.
//!
//! Built by `tools/build_margin_fixture.py` (inputs pinned by sha256 in the file).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_account::{Abstraction, Account, Decision, Ledger, Market, OrderAgg, OrderReq, Position};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    default_leverage: u32,
    markets: Vec<FMarket>,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct FMarket {
    asset: u32,
    max_leverage: u32,
}
#[derive(Deserialize)]
struct Case {
    height: u64,
    user: String,
    account: FAccount,
    marks: Vec<(u32, u64)>,
    order: FOrder,
    native: String,
}
#[derive(Deserialize)]
struct FAccount {
    abstraction: Option<String>,
    usdc: i64,
    spot_usdc: i64,
    positions: Vec<FPos>,
    leverage: Vec<(u32, u32)>,
    orders: Vec<FAgg>,
}
#[derive(Deserialize)]
struct FPos {
    asset: u32,
    szi: i64,
    entry_ntl: i64,
}
#[derive(Deserialize)]
struct FAgg {
    asset: u32,
    bid_ntl: i64,
    ask_ntl: i64,
    bid_lots: i64,
    ask_lots: i64,
    leverage: u32,
}
#[derive(Deserialize)]
struct FOrder {
    asset: u32,
    is_buy: bool,
    px_ticks: u64,
    lots: i64,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/margin_1232430000.json")).unwrap()
}

fn decide(f: &Fixture, c: &Case) -> Decision {
    let markets = f
        .markets
        .iter()
        .map(|m| {
            (
                m.asset,
                Market {
                    max_leverage: m.max_leverage,
                    mark_ticks: 0,
                },
            )
        })
        .collect();
    let mut ledger = Ledger::new(markets, f.default_leverage);
    for &(a, px) in &c.marks {
        ledger.set_mark(a, px);
    }
    let acc = Account {
        abstraction: Abstraction::from_state(c.account.abstraction.as_deref()),
        usdc: c.account.usdc,
        spot_usdc: c.account.spot_usdc,
        positions: c
            .account
            .positions
            .iter()
            .map(|p| {
                (
                    p.asset,
                    Position {
                        szi: p.szi,
                        entry_ntl: p.entry_ntl,
                    },
                )
            })
            .collect(),
        leverage: c.account.leverage.iter().copied().collect(),
        orders: c
            .account
            .orders
            .iter()
            .map(|o| {
                (
                    o.asset,
                    OrderAgg {
                        bid_ntl: o.bid_ntl,
                        ask_ntl: o.ask_ntl,
                        bid_lots: o.bid_lots,
                        ask_lots: o.ask_lots,
                        leverage: o.leverage,
                    },
                )
            })
            .collect(),
        ..Account::default()
    };
    ledger.insert(c.user.clone(), acc);
    ledger.check_order(
        &c.user,
        &OrderReq {
            asset: c.order.asset,
            is_buy: c.order.is_buy,
            px_ticks: c.order.px_ticks,
            lots: c.order.lots,
        },
    )
}

fn native(c: &Case) -> Decision {
    match c.native.as_str() {
        "accepted" => Decision::Accept,
        "insufficient_margin" => Decision::InsufficientMargin,
        other => panic!("unknown native outcome {other}"),
    }
}

fn case<'a>(f: &'a Fixture, height: u64, user_prefix: &str) -> &'a Case {
    f.cases
        .iter()
        .find(|c| c.height == height && c.user.starts_with(user_prefix))
        .unwrap()
}

/// Native rejection: perp free margin plus spot USDC (468.44) is short of the
/// order's 499.4 USDC margin. Fails if spot or resting-order margin is ignored.
#[test]
fn rejects_when_perp_free_plus_spot_is_short() {
    let f = fixture();
    let c = case(&f, 1_232_430_435, "0x6a23260e");
    assert_eq!(native(c), Decision::InsufficientMargin);
    assert_eq!(decide(&f, c), Decision::InsufficientMargin);
}

/// Native acceptance: resting orders use 99% of perp value, spot USDC covers.
#[test]
fn accepts_when_spot_usdc_covers_the_order() {
    let f = fixture();
    let c = case(&f, 1_232_430_008, "0xc09b7082");
    assert_eq!(native(c), Decision::Accept);
    assert_eq!(decide(&f, c), Decision::Accept);
}

/// Native rejection with no spot: resting-order margin leaves 0.50 USDC free.
#[test]
fn rejects_when_resting_orders_use_the_margin() {
    let f = fixture();
    let c = case(&f, 1_232_430_192, "0xccabdd2c");
    assert_eq!(native(c), Decision::InsufficientMargin);
    assert_eq!(decide(&f, c), Decision::InsufficientMargin);
}

/// Native rejection: BTC and ETH positions with no leverage setting are charged
/// at the default 20x (not the 40x/25x maximum), which uses all account value.
#[test]
fn unset_leverage_charges_the_default_not_the_maximum() {
    let f = fixture();
    let c = case(&f, 1_232_430_063, "0xa658d9dc");
    assert_eq!(native(c), Decision::InsufficientMargin);
    assert_eq!(decide(&f, c), Decision::InsufficientMargin);
}

/// Native rejection although spot USDC would cover: the account is not unified
/// (`ust.a = "i"`), so spot USDC is not perp collateral. Was the open
/// contradiction to "spot counts for everyone".
#[test]
fn independent_account_does_not_count_spot() {
    let f = fixture();
    let c = case(&f, 1_232_430_014, "0xd839e870");
    assert_eq!(c.account.abstraction.as_deref(), Some("i"));
    assert_eq!(native(c), Decision::InsufficientMargin);
    assert_eq!(decide(&f, c), Decision::InsufficientMargin);
}

/// Whole fixture: the count is pinned, and every miss is a named, known case.
#[test]
fn reproduces_the_clean_sample() {
    let f = fixture();
    let known_misses: [(u64, &str); 5] = [
        (1_232_431_007, "0x955c35df"), // portfolio margin (ust.a = p): not modelled
        (1_232_435_181, "0x5de8fab8"), // out of sample, portfolio margin
        (1_232_435_831, "0xc42a7d06"), // out of sample, portfolio margin
        (1_232_436_969, "0x07925188"), // out of sample, portfolio margin
        (1_232_438_533, "0x89bec994"), // out of sample, unified: 95.90 USDC short, native accepts
    ];
    let mut matched = 0;
    let mut misses = Vec::new();
    for c in &f.cases {
        if decide(&f, c) == native(c) {
            matched += 1;
        } else {
            misses.push((c.height, c.user[..10].to_string()));
        }
    }
    let expected: Vec<_> = known_misses
        .iter()
        .map(|&(h, u)| (h, u.to_string()))
        .collect();
    assert_eq!(misses, expected);
    assert_eq!((matched, f.cases.len()), (282, 287));
}
