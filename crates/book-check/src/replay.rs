//! Replaying raw signed actions (`txs`) through the reference book.
//!
//! This follows `ACTION_ORDERING.md` §7 (`docs/ACTION_ORDERING.md`), the spec of
//! record, with each rule's status:
//!
//! - txs apply in file order (INFERENCE: explorer order = execution order);
//!   a tx with a non-null `error` changes nothing (INFERENCE, §1.5);
//! - legs of one action apply in array order (INFERENCE, §2.1);
//! - trigger orders never touch the book (Phase 1 scope, §2.3);
//! - ALO rests or is rejected if it would cross; GTC matches then rests; IOC
//!   and `FrontendMarket` match then drop (VERIFIED / INFERENCE, §2.3, §3.5);
//! - self-trade prevention cancels the resting order of the same user
//!   (VERIFIED, §3.4; which address counts is UNVERIFIED: here the tx `user`);
//!   [`ReplayConfig::self_trade_prevention`] turns it off;
//! - `cancelByCloid` resolves by (user, cloid); plain `cancel` by oid is never
//!   resolved, because public data does not show the oid a new order gets
//!   (§5.2) and the oids used here are synthetic ([`OidRule`]);
//! - `modify`/`batchModify` = cancel old + place new with a new oid
//!   (INFERENCE, §1.1); the new order is placed even when the old one is not
//!   found;
//! - a leg whose price or size breaks the tick/lot rule is rejected
//!   (INFERENCE: observed in successful txs, so presumably a per-order error);
//! - reduce-only legs are placed like any other (UNVERIFIED clipping, §2.3;
//!   counted in [`ReplayStats::reduce_only_legs`]); margin, liquidations and
//!   funding are not modelled.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use hl_book_ref::{ApplyError, Book, BookOp, Lots, MatchConfig, Oid, Order, Owner, Px, Side, Tif};
use hl_dataset::action::{self, Action, LimitTif, ModifyWire, OrderKind, OrderRef, OrderWire};
use hl_dataset::{AssetMeta, Block, Tx};

/// How the replayer numbers new orders. Hyperliquid's rule is UNVERIFIED
/// (`ACTION_ORDERING.md` §5.2: a global counter, but not exactly one per leg),
/// so these oids are synthetic and never match exchange oids.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OidRule {
    /// One oid, counting from 1, for every leg handed to a book (a limit leg
    /// on a known asset with a valid price and size, in a tx without an
    /// error), whether it then rests, fills or is rejected as a crossing ALO.
    /// A leg rejected before that (tick/lot, unknown asset or tif, trigger;
    /// `ACTION_ORDERING.md` §4.5) takes no oid. Whether Hyperliquid gives a
    /// rejected leg an oid is UNVERIFIED.
    #[default]
    OnePerAcceptedLeg,
}

/// Replay rules that are switchable because they are not fully verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReplayConfig {
    /// Self-trade prevention (default on).
    pub self_trade_prevention: bool,
    /// Oid numbering.
    pub oid_rule: OidRule,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            self_trade_prevention: true,
            oid_rule: OidRule::default(),
        }
    }
}

/// What a replay did. Leg counts are per order leg.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplayStats {
    /// Blocks applied.
    pub blocks: u64,
    /// Transactions seen.
    pub txs: u64,
    /// Transactions skipped because they carry an `error`.
    pub errored_txs_skipped: u64,
    /// Actions whose book-relevant shape failed to parse (skipped).
    pub parse_failures: u64,
    /// Per-order `ops` in the blocks, ignored in replay mode.
    pub ops_ignored: u64,
    /// `order` legs by tif, in non-errored txs.
    pub order_legs: BTreeMap<String, u64>,
    /// New-order legs of modifies by tif, in non-errored txs.
    pub modify_legs: BTreeMap<String, u64>,
    /// Trigger legs left off the book.
    pub trigger_legs_skipped: u64,
    /// Legs with a tif this replayer does not know (skipped).
    pub legs_unknown_tif: u64,
    /// Legs on an asset not in the header (skipped).
    pub legs_unknown_asset: u64,
    /// Legs whose price or size breaks the tick/lot rule (rejected).
    pub legs_invalid_tick_lot: u64,
    /// Reduce-only legs placed as ordinary orders.
    pub reduce_only_legs: u64,
    /// Legs handed to a book that it accepted.
    pub legs_placed: u64,
    /// Of those, legs that left size resting.
    pub legs_rested: u64,
    /// ALO legs rejected because they would cross.
    pub legs_alo_rejected: u64,
    /// Legs the book refused for another reason.
    pub legs_book_rejected: u64,
    /// Fills (one per maker hit).
    pub fills: u64,
    /// Resting orders cancelled by self-trade prevention.
    pub self_trade_cancels: u64,
    /// `cancel` legs by oid: never resolved from public data.
    pub cancel_by_oid_unresolved: u64,
    /// `cancelByCloid` legs that found a resting order and removed it.
    pub cancel_by_cloid_resolved: u64,
    /// `cancelByCloid` legs whose order is not resting in the replay (placed
    /// before the window, already filled or cancelled, or on another asset).
    pub cancel_by_cloid_unresolved: u64,
    /// Modify legs whose old order was found (by cloid) and cancelled.
    pub modify_old_resolved: u64,
    /// Modify legs whose old order was not found (by oid, or cloid not resting).
    pub modify_old_unresolved: u64,
    /// Resting orders placed with a cloid already mapped to a resting order of
    /// the same user (the new one wins the mapping).
    pub duplicate_cloids: u64,
    /// Other action types seen in non-errored txs (no book effect in Phase 1).
    pub other_actions: BTreeMap<String, u64>,
}

impl fmt::Display for ReplayStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let map = |m: &BTreeMap<String, u64>| {
            m.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let rows: [(&str, String); 26] = [
            ("blocks", self.blocks.to_string()),
            ("txs", self.txs.to_string()),
            ("errored_txs_skipped", self.errored_txs_skipped.to_string()),
            ("parse_failures", self.parse_failures.to_string()),
            ("ops_ignored", self.ops_ignored.to_string()),
            ("order_legs", map(&self.order_legs)),
            ("modify_legs", map(&self.modify_legs)),
            (
                "trigger_legs_skipped",
                self.trigger_legs_skipped.to_string(),
            ),
            ("legs_unknown_tif", self.legs_unknown_tif.to_string()),
            ("legs_unknown_asset", self.legs_unknown_asset.to_string()),
            (
                "legs_invalid_tick_lot",
                self.legs_invalid_tick_lot.to_string(),
            ),
            ("reduce_only_legs", self.reduce_only_legs.to_string()),
            ("legs_placed", self.legs_placed.to_string()),
            ("legs_rested", self.legs_rested.to_string()),
            ("legs_alo_rejected", self.legs_alo_rejected.to_string()),
            ("legs_book_rejected", self.legs_book_rejected.to_string()),
            ("fills", self.fills.to_string()),
            ("self_trade_cancels", self.self_trade_cancels.to_string()),
            (
                "cancel_by_oid_unresolved",
                self.cancel_by_oid_unresolved.to_string(),
            ),
            (
                "cancel_by_cloid_resolved",
                self.cancel_by_cloid_resolved.to_string(),
            ),
            (
                "cancel_by_cloid_unresolved",
                self.cancel_by_cloid_unresolved.to_string(),
            ),
            ("modify_old_resolved", self.modify_old_resolved.to_string()),
            (
                "modify_old_unresolved",
                self.modify_old_unresolved.to_string(),
            ),
            ("duplicate_cloids", self.duplicate_cloids.to_string()),
            ("other_actions", map(&self.other_actions)),
            (
                "note",
                "oids are synthetic (OidRule::OnePerAcceptedLeg)".to_owned(),
            ),
        ];
        for (k, v) in rows {
            writeln!(f, "  {k}: {v}")?;
        }
        Ok(())
    }
}

fn bump(m: &mut BTreeMap<String, u64>, k: &str) {
    *m.entry(k.to_owned()).or_default() += 1;
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// A validated limit leg, ready for its book.
struct Leg {
    asset: u32,
    side: Side,
    px: Px,
    sz: Lots,
    tif: Tif,
}

/// Turns blocks of raw actions into book operations on one reference book per asset.
#[derive(Debug)]
pub struct Replayer {
    books: BTreeMap<u32, Book>,
    metas: HashMap<u32, AssetMeta>,
    owners: HashMap<String, Owner>,
    cloids: HashMap<(Owner, String), (u32, Oid)>,
    next_oid: u64,
    config: ReplayConfig,
    stats: ReplayStats,
}

impl Replayer {
    /// Empty books for every asset in `assets`.
    #[must_use]
    pub fn new(assets: &[AssetMeta], config: ReplayConfig) -> Self {
        let mc = MatchConfig {
            self_trade_prevention: config.self_trade_prevention,
        };
        Self {
            books: assets
                .iter()
                .map(|a| (a.asset_index, Book::with_config(mc)))
                .collect(),
            metas: assets.iter().map(|a| (a.asset_index, a.clone())).collect(),
            owners: HashMap::new(),
            cloids: HashMap::new(),
            next_oid: 1,
            config,
            stats: ReplayStats::default(),
        }
    }

    /// The rules in force.
    #[must_use]
    pub fn config(&self) -> ReplayConfig {
        self.config
    }

    /// Counts so far.
    #[must_use]
    pub fn stats(&self) -> &ReplayStats {
        &self.stats
    }

    /// The book of one asset.
    #[must_use]
    pub fn book(&self, asset: u32) -> Option<&Book> {
        self.books.get(&asset)
    }

    /// Apply every tx of a block in file order. `block.ops` is ignored.
    pub fn apply_block(&mut self, block: &Block) {
        self.stats.blocks += 1;
        self.stats.ops_ignored += count(block.ops.len());
        for tx in &block.txs {
            self.apply_tx(tx);
        }
    }

    fn apply_tx(&mut self, tx: &Tx) {
        self.stats.txs += 1;
        if tx.error.is_some() {
            self.stats.errored_txs_skipped += 1;
            return;
        }
        let Ok(parsed) = action::parse(tx.action.get()) else {
            self.stats.parse_failures += 1;
            return;
        };
        let owner = self.owner(&tx.user);
        match parsed {
            Action::Order { orders } => {
                for leg in &orders {
                    bump(&mut self.stats.order_legs, leg.t.name());
                    self.place_leg(owner, leg, None);
                }
            }
            Action::Cancel { cancels } => {
                self.stats.cancel_by_oid_unresolved += count(cancels.len());
            }
            Action::CancelByCloid { cancels } => {
                for c in &cancels {
                    if self.cancel_by_cloid(owner, c.asset, &c.cloid) {
                        self.stats.cancel_by_cloid_resolved += 1;
                    } else {
                        self.stats.cancel_by_cloid_unresolved += 1;
                    }
                }
            }
            Action::Modify(m) => self.modify(owner, &m),
            Action::BatchModify { modifies } => {
                for m in &modifies {
                    self.modify(owner, m);
                }
            }
            Action::Other(t) => bump(&mut self.stats.other_actions, &t),
        }
    }

    fn owner(&mut self, user: &str) -> Owner {
        let next = Owner(u32::try_from(self.owners.len()).unwrap_or(u32::MAX));
        *self.owners.entry(user.to_owned()).or_insert(next)
    }

    /// The (asset, oid) a user's cloid maps to, if that order still rests.
    fn resolve_cloid(&self, owner: Owner, cloid: &str) -> Option<(u32, Oid)> {
        let &(asset, oid) = self.cloids.get(&(owner, cloid.to_owned()))?;
        self.books.get(&asset)?.get(oid)?;
        Some((asset, oid))
    }

    fn cancel_by_cloid(&mut self, owner: Owner, asset: u32, cloid: &str) -> bool {
        match self.resolve_cloid(owner, cloid) {
            Some((a, oid)) if a == asset => self
                .books
                .get_mut(&a)
                .is_some_and(|b| b.apply(&BookOp::Cancel { oid }).is_ok()),
            _ => false,
        }
    }

    fn modify(&mut self, owner: Owner, m: &ModifyWire) {
        bump(&mut self.stats.modify_legs, m.order.t.name());
        let old = match &m.oid {
            OrderRef::Oid(_) => None,
            OrderRef::Cloid(c) => self.resolve_cloid(owner, c),
        };
        if old.is_some() {
            self.stats.modify_old_resolved += 1;
        } else {
            self.stats.modify_old_unresolved += 1;
        }
        self.place_leg(owner, &m.order, old);
    }

    /// Check a leg against tif, asset and tick/lot rules.
    fn validate(&mut self, leg: &OrderWire) -> Option<Leg> {
        let tif = match &leg.t {
            OrderKind::Trigger => {
                self.stats.trigger_legs_skipped += 1;
                return None;
            }
            OrderKind::Limit(LimitTif::Alo) => Tif::Alo,
            OrderKind::Limit(LimitTif::Gtc) => Tif::Gtc,
            OrderKind::Limit(LimitTif::Ioc | LimitTif::FrontendMarket) => Tif::Ioc,
            OrderKind::Limit(LimitTif::Unknown(_)) => {
                self.stats.legs_unknown_tif += 1;
                return None;
            }
        };
        let Some(meta) = self.metas.get(&leg.a) else {
            self.stats.legs_unknown_asset += 1;
            return None;
        };
        let (Ok(px), Ok(sz)) = (meta.order_px_ticks(&leg.p), meta.sz_lots(&leg.s)) else {
            self.stats.legs_invalid_tick_lot += 1;
            return None;
        };
        if sz == 0 {
            self.stats.legs_invalid_tick_lot += 1;
            return None;
        }
        if leg.r {
            self.stats.reduce_only_legs += 1;
        }
        Some(Leg {
            asset: leg.a,
            side: if leg.b { Side::Bid } else { Side::Ask },
            px: Px(px),
            sz: Lots(sz),
            tif,
        })
    }

    /// Place one leg; `old` is a modify's resolved old order, cancelled first.
    fn place_leg(&mut self, owner: Owner, wire: &OrderWire, old: Option<(u32, Oid)>) {
        let leg = self.validate(wire);
        // A modify whose new leg cannot be placed, or that moves to another
        // asset, still cancels the old order (modify = cancel + place).
        if let Some((old_asset, old_oid)) = old {
            if leg.as_ref().is_none_or(|l| l.asset != old_asset) {
                if let Some(b) = self.books.get_mut(&old_asset) {
                    // The order was just resolved as resting, so this succeeds.
                    let _ = b.apply(&BookOp::Cancel { oid: old_oid });
                }
            }
        }
        let Some(leg) = leg else { return };
        let oid = Oid(self.next_oid);
        self.next_oid += 1;
        let order = Order {
            oid,
            side: leg.side,
            px: leg.px,
            sz: leg.sz,
        };
        let Some(book) = self.books.get_mut(&leg.asset) else {
            unreachable!("validated asset {} has a book", leg.asset);
        };
        let result = match old {
            Some((old_asset, old_oid)) if old_asset == leg.asset => {
                book.modify(old_oid, Some(owner), order, leg.tif).placed
            }
            _ => book.place_owned(owner, order, leg.tif),
        };
        match result {
            Ok(placed) => {
                self.stats.legs_placed += 1;
                self.stats.fills += count(placed.fills.len());
                self.stats.self_trade_cancels += count(placed.self_trade_cancels.len());
                if placed.rested.is_some() {
                    self.stats.legs_rested += 1;
                    if let Some(c) = &wire.c {
                        if self.resolve_cloid(owner, c).is_some() {
                            self.stats.duplicate_cloids += 1;
                        }
                        self.cloids.insert((owner, c.clone()), (leg.asset, oid));
                    }
                }
            }
            Err(ApplyError::AloWouldCross { .. }) => self.stats.legs_alo_rejected += 1,
            Err(_) => self.stats.legs_book_rejected += 1,
        }
    }
}
