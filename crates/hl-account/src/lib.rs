//! Account-side order checks, decided from seeded account state and never from
//! native's responses. First piece: the perp cross-margin check that native
//! reports as "Insufficient margin to place order."
//!
//! # Units
//! Perp cash and notionals are micro-USDC (1e-6); a notional is
//! `px_ticks * lots` exactly, since ticks are 10^-(6-szDecimals) and lots
//! 10^-szDecimals. Spot USDC is 1e-8. All arithmetic is integer (`i128`
//! intermediates); there is no floating point.
//!
//! # The rule (fitted on our node's data; see `docs/proofs/account-margin.md`)
//! Account value `AV = usdc + sum(szi * mark)`. Position margin
//! `IM = sum(|szi| * mark / lev)`. Resting-order margin `E` is, per asset, the
//! larger of the two sides' charges divided by the orders' leverage, where the
//! side that grows the position pays its full notional and the side that
//! shrinks it pays only for lots beyond `2 * |szi|`. An order is accepted when
//! it does not raise its asset's charge, or when
//! `AV - IM - E + spot USDC >= the raise`. A position or order with no leverage
//! setting is charged at the default leverage capped at the asset maximum.
use std::collections::{BTreeMap, HashMap};

/// Spot USDC has 8 decimals, perp USDC 6.
const SPOT_PER_MICRO: i128 = 100;

/// Outcome of an account check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The order may rest or fill.
    Accept,
    /// Native's "Insufficient margin to place order."
    InsufficientMargin,
}

/// Per-asset market data.
#[derive(Debug, Clone, Copy)]
pub struct Market {
    /// First-tier maximum leverage.
    pub max_leverage: u32,
    /// Mark price in ticks (`SetGlobalAction` pair element 1).
    pub mark_ticks: u64,
}

/// A perp position.
#[derive(Debug, Clone, Copy)]
pub struct Position {
    /// Signed size in lots.
    pub szi: i64,
    /// Entry notional, micro-USDC.
    pub entry_ntl: i64,
}

/// Resting-order totals for one asset.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrderAgg {
    /// Bid notional, micro-USDC.
    pub bid_ntl: i64,
    /// Ask notional, micro-USDC.
    pub ask_ntl: i64,
    /// Bid lots.
    pub bid_lots: i64,
    /// Ask lots.
    pub ask_lots: i64,
    /// Leverage the orders are margined at.
    pub leverage: u32,
}

/// One user's account.
#[derive(Debug, Clone, Default)]
pub struct Account {
    /// Perp cross USDC, micro.
    pub usdc: i64,
    /// Free spot USDC (total minus hold), 1e-8.
    pub spot_usdc: i64,
    /// Positions by asset.
    pub positions: BTreeMap<u32, Position>,
    /// Leverage settings by asset.
    pub leverage: BTreeMap<u32, u32>,
    /// Resting-order totals by asset.
    pub orders: BTreeMap<u32, OrderAgg>,
}

/// An order to check.
#[derive(Debug, Clone, Copy)]
pub struct OrderReq {
    /// Asset.
    pub asset: u32,
    /// Buy side.
    pub is_buy: bool,
    /// Limit price in ticks.
    pub px_ticks: u64,
    /// Size in lots.
    pub lots: i64,
}

/// The charge, in micro-USDC before dividing by leverage, of one side of an
/// asset's resting orders. A side that shrinks the position is charged only for
/// lots beyond `free_lots` (twice the position), pro rata by notional.
fn side_charge(ntl: i64, lots: i64, free_lots: i64) -> i128 {
    if lots <= free_lots {
        return 0;
    }
    if free_lots <= 0 {
        return i128::from(ntl);
    }
    i128::from(ntl) * i128::from(lots - free_lots) / i128::from(lots)
}

/// Resting-order margin of one asset, micro-USDC, floored.
fn asset_order_margin(agg: &OrderAgg, szi: i64) -> i128 {
    let bid_free = if szi < 0 { 2 * szi.saturating_neg() } else { 0 };
    let ask_free = if szi > 0 { 2 * szi } else { 0 };
    let bid = side_charge(agg.bid_ntl, agg.bid_lots, bid_free);
    let ask = side_charge(agg.ask_ntl, agg.ask_lots, ask_free);
    bid.max(ask) / i128::from(agg.leverage.max(1))
}

/// All accounts and markets.
#[derive(Debug, Default)]
pub struct Ledger {
    markets: HashMap<u32, Market>,
    users: HashMap<String, Account>,
    default_leverage: u32,
}

impl Ledger {
    /// A ledger over `markets`; positions and orders with no leverage setting
    /// are charged at `default_leverage` capped at the asset maximum.
    #[must_use]
    pub fn new(markets: HashMap<u32, Market>, default_leverage: u32) -> Self {
        Self {
            markets,
            users: HashMap::new(),
            default_leverage,
        }
    }

    /// Sets an asset's mark.
    pub fn set_mark(&mut self, asset: u32, mark_ticks: u64) {
        self.markets
            .entry(asset)
            .or_insert(Market {
                max_leverage: 1,
                mark_ticks,
            })
            .mark_ticks = mark_ticks;
    }

    /// Seeds one account.
    pub fn insert(&mut self, user: String, account: Account) {
        self.users.insert(user, account);
    }

    /// One user's account, if seeded.
    #[must_use]
    pub fn account(&self, user: &str) -> Option<&Account> {
        self.users.get(user)
    }

    fn leverage(&self, acc: &Account, asset: u32) -> u32 {
        if let Some(&l) = acc.leverage.get(&asset) {
            return l.max(1);
        }
        if let Some(o) = acc.orders.get(&asset) {
            return o.leverage.max(1);
        }
        let max = self.markets.get(&asset).map_or(1, |m| m.max_leverage);
        self.default_leverage.min(max).max(1)
    }

    fn mark(&self, asset: u32) -> i128 {
        i128::from(self.markets.get(&asset).map_or(0, |m| m.mark_ticks))
    }

    /// Whether `user` may place `order`. An unknown user has an empty account.
    #[must_use]
    pub fn check_order(&self, user: &str, order: &OrderReq) -> Decision {
        let empty = Account::default();
        let acc = self.users.get(user).unwrap_or(&empty);
        let szi_of = |a: u32| acc.positions.get(&a).map_or(0, |p| p.szi);

        let lev = self.leverage(acc, order.asset);
        let before = acc.orders.get(&order.asset).copied().unwrap_or(OrderAgg {
            leverage: lev,
            ..OrderAgg::default()
        });
        let mut after = before;
        after.leverage = lev;
        let ntl =
            i64::try_from(u128::from(order.px_ticks) * u128::try_from(order.lots).unwrap_or(0))
                .unwrap_or(i64::MAX);
        if order.is_buy {
            after.bid_ntl = after.bid_ntl.saturating_add(ntl);
            after.bid_lots = after.bid_lots.saturating_add(order.lots);
        } else {
            after.ask_ntl = after.ask_ntl.saturating_add(ntl);
            after.ask_lots = after.ask_lots.saturating_add(order.lots);
        }
        let szi = szi_of(order.asset);
        let need = asset_order_margin(&after, szi) - asset_order_margin(&before, szi);
        if need <= 0 {
            return Decision::Accept;
        }

        let mut value = i128::from(acc.usdc);
        let mut position_margin = 0i128;
        for (&a, p) in &acc.positions {
            let ntl = i128::from(p.szi) * self.mark(a);
            value += ntl;
            position_margin += ntl.abs() / i128::from(self.leverage(acc, a));
        }
        let order_margin: i128 = acc
            .orders
            .iter()
            .map(|(&a, o)| asset_order_margin(o, szi_of(a)))
            .sum();
        let free =
            (value - position_margin - order_margin) * SPOT_PER_MICRO + i128::from(acc.spot_usdc);
        if free >= need * SPOT_PER_MICRO {
            Decision::Accept
        } else {
            Decision::InsufficientMargin
        }
    }
}
