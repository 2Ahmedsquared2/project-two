//! The evolving-state interface on our node's data: four real accounts seeded
//! from the translated state at 1232430000, driven only through
//! [`Ledger::fill`] with their fills from `node_fills_by_block`
//! (1232430001..1232440000), must land exactly on the translated state at
//! 1232440000 (cash and every position). The accounts are clean cross accounts
//! with only main-perp fills and no other action in the window (297 of 703 such
//! accounts reproduce exactly; the rest are named in `docs/proofs/account-margin.md`).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::collections::HashMap;

use hl_account::{Abstraction, Account, Decision, Ledger, Market, OrderReq, Position};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    users: Vec<User>,
}
#[derive(Deserialize)]
struct User {
    #[serde(rename = "user")]
    addr: String,
    seed: Snap,
    /// `[height, asset, is_buy, px_ticks, lots, fee_micro]`
    fills: Vec<(u64, u32, bool, u64, i64, i64)>,
    end: Snap,
}
#[derive(Deserialize)]
struct Snap {
    usdc: i64,
    positions: Vec<(u32, i64)>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/evolve_1232430000_1232440000.json")).unwrap()
}

fn ledger() -> Ledger {
    let markets: HashMap<u32, Market> = (0..300)
        .map(|a| {
            (
                a,
                Market {
                    max_leverage: 40,
                    mark_ticks: 0,
                },
            )
        })
        .collect();
    Ledger::new(markets, 20)
}

fn seeded(u: &User) -> Ledger {
    let mut l = ledger();
    l.insert(
        u.addr.clone(),
        Account {
            usdc: u.seed.usdc,
            positions: u
                .seed
                .positions
                .iter()
                .map(|&(a, szi)| (a, Position { szi, entry_ntl: 0 }))
                .collect(),
            ..Account::default()
        },
    );
    l
}

#[test]
fn fills_reproduce_the_end_state_exactly() {
    let f = fixture();
    assert_eq!(f.users.len(), 4);
    for u in &f.users {
        let mut l = seeded(u);
        for &(_, asset, is_buy, px, lots, fee) in &u.fills {
            l.fill(&u.addr, asset, is_buy, px, lots, fee);
        }
        let acc = l.account(&u.addr).unwrap();
        assert_eq!(acc.usdc, u.end.usdc, "{} cash", u.addr);
        let mut got: Vec<(u32, i64)> = acc.positions.iter().map(|(&a, p)| (a, p.szi)).collect();
        got.sort_unstable();
        let mut want = u.end.positions.clone();
        want.sort_unstable();
        assert_eq!(got, want, "{} positions", u.addr);
        for &(a, szi) in &want {
            assert_eq!(l.position(&u.addr, a), szi);
        }
    }
}

/// A resting order's margin follows rest/unrest, and a maker fill releases it.
#[test]
fn rest_unrest_and_maker_fill_move_the_order_margin() {
    let mut l = ledger();
    l.set_mark(0, 610_000);
    l.insert(
        "a".into(),
        Account {
            usdc: 1_000_000_000, // 1,000 USDC
            abstraction: Abstraction::Independent,
            ..Account::default()
        },
    );
    l.set_leverage("a", 0, 10, true);
    // 0.15 BTC at 61,000 (610,000 ticks) = 9,150 USDC notional, 915 at 10x: fits in 1,000.
    let o = OrderReq {
        asset: 0,
        is_buy: true,
        px_ticks: 610_000,
        lots: 15_000,
    };
    assert_eq!(l.check_order("a", &o), Decision::Accept);
    l.rest("a", 0, true, 610_000, 15_000);
    // The same again would need 1,830 in all.
    assert_eq!(l.check_order("a", &o), Decision::InsufficientMargin);
    // Half of the resting bid fills as maker: the caller unrests the filled lots
    // and books the fill; a position opens and the order margin halves.
    l.fill("a", 0, true, 610_000, 7_500, 0);
    l.unrest("a", 0, true, 610_000, 7_500);
    assert_eq!(l.position("a", 0), 7_500);
    assert_eq!(l.account("a").unwrap().orders[&0].bid_lots, 7_500);
    l.unrest("a", 0, true, 610_000, 7_500);
    assert!(l.account("a").unwrap().orders.is_empty());
}

/// Spot USDC is collateral only for a unified account; a class transfer moves
/// cash between the two balances.
#[test]
fn spot_counts_only_when_unified() {
    let mut l = ledger();
    l.set_mark(0, 61_000);
    let acc = Account {
        usdc: 0,
        spot_usdc: 100_000_000_000, // 1,000 USDC at 1e-8
        ..Account::default()
    };
    l.insert("u".into(), acc.clone());
    l.insert("i".into(), acc);
    l.set_abstraction("u", Abstraction::Unified);
    l.set_abstraction("i", Abstraction::Independent);
    let o = OrderReq {
        asset: 0,
        is_buy: true,
        px_ticks: 61_000,
        lots: 1_000,
    };
    assert_eq!(l.check_order("u", &o), Decision::Accept);
    assert_eq!(l.check_order("i", &o), Decision::InsufficientMargin);
    l.usd_class_transfer("i", 100_000_000, true); // 100 USDC to perp
    assert_eq!(l.account("i").unwrap().usdc, 100_000_000);
    assert_eq!(l.account("i").unwrap().spot_usdc, 90_000_000_000);
    assert_eq!(l.check_order("i", &o), Decision::Accept);
}
