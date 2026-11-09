//! Reference order book for Project Two.
//!
//! This is the simplest order book we could write and still trust: one
//! [`BTreeMap`] per side keyed by integer price ticks, each level a FIFO
//! [`VecDeque`] of resting orders, plus a [`HashMap`] from order id to its
//! location. Prices are [`Px`] ticks and sizes are [`Lots`]; no floats ever
//! enter the book (the workspace `clippy.toml` bans `f32` and `f64`).
//!
//! It is deliberately not fast. It is the baseline that every faster design is
//! diffed against with `book-check`, so clarity beats cleverness here.
//!
//! # Model
//!
//! The book is driven by [`BookOp`]s, the per-order effects a block has on the
//! resting book: an order comes to rest ([`BookOp::Insert`]), leaves
//! ([`BookOp::Cancel`]) or changes size in place ([`BookOp::Resize`], for a
//! partial fill or a size reduction that keeps queue priority). Matching
//! itself (which crossing taker produced which fills) is intentionally outside
//! this crate for now: the recorded dataset carries the resulting book
//! effects, so the checker can run before the matching rules are pinned down.
//!
//! Unverified against Hyperliquid's core: whether a size reduction keeps queue
//! position on Hyperliquid. The model here says it does; the Hyperliquid
//! Reason owns that fact and the dataset will show it either way.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::{Deserialize, Serialize};

/// A price in integer ticks of the asset's price grid.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct Px(pub u64);

/// A size in integer lots of the asset's size grid.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct Lots(pub u64);

/// An order id. Unique across the whole book.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct Oid(pub u64);

/// An opaque owner id for self-trade prevention: the dataset producer maps
/// each account address to one. Two orders self-trade when their owners are
/// equal. Which address Hyperliquid compares (master account vs exact signer)
/// is unverified (`ACTION_ORDERING.md` §3.4); the producer decides the mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Owner(pub u32);

/// Matching rules that are configurable because Hyperliquid's behaviour is
/// only partly verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MatchConfig {
    /// Self-trade prevention: when an incoming order would match a resting
    /// order with the same [`Owner`], the resting order is cancelled with no
    /// fill and the incoming order continues. VERIFIED in Hyperliquid's docs
    /// (trading/self-trade-prevention); default on.
    pub self_trade_prevention: bool,
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            self_trade_prevention: true,
        }
    }
}

/// What [`Book::modify`] did: modify is cancel of the old order plus
/// placement of a new one with a new id (never an in-place resize).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Modified {
    /// The old order was resting and has been removed.
    pub cancelled: bool,
    /// The placement of the new order, attempted whether or not the cancel
    /// succeeded (Hyperliquid's `always_place`; `ACTION_ORDERING.md` §1.1, §5).
    pub placed: Result<Placed, ApplyError>,
}

/// Which side of the book an order rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// Buy side; best bid is the highest price.
    Bid,
    /// Sell side; best ask is the lowest price.
    Ask,
}

impl Side {
    /// The other side.
    #[must_use]
    pub fn opposite(self) -> Side {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }
}

/// A resting order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Order {
    /// Order id.
    pub oid: Oid,
    /// Side it rests on.
    pub side: Side,
    /// Limit price in ticks.
    pub px: Px,
    /// Remaining size in lots. Never zero while resting.
    pub sz: Lots,
}

/// A per-order effect on the resting book.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum BookOp {
    /// An order comes to rest at the back of its price level.
    Insert(Order),
    /// A resting order leaves the book (cancel, full fill, expiry).
    Cancel {
        /// The order that leaves.
        oid: Oid,
    },
    /// An order comes to rest directly in front of `before`, which must rest on
    /// the same side at the same price. Mirrors hl-node's raw book diff
    /// `insertBefore` (needed later for ALO priority).
    InsertBefore {
        /// The resting order the new one goes in front of.
        before: Oid,
        /// The new order.
        order: Order,
    },
    /// A resting order's remaining size changes in place; queue position is kept.
    Resize {
        /// The order that changes.
        oid: Oid,
        /// Its new remaining size. Must be non-zero; use [`BookOp::Cancel`] to remove.
        sz: Lots,
    },
}

/// Why a [`BookOp`] could not be applied. The book is unchanged after an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ApplyError {
    /// An insert reused an id that is already resting.
    #[error("order id {0:?} is already resting")]
    DuplicateOid(Oid),
    /// A cancel or resize named an id that is not resting.
    #[error("order id {0:?} is not resting")]
    UnknownOid(Oid),
    /// An insert or resize carried a zero size.
    #[error("order id {0:?} has zero size")]
    ZeroSize(Oid),
    /// An `insert_before` anchor rests on another side or price than the new order.
    #[error("order {oid:?} cannot go before {before:?}: different side or price")]
    AnchorMismatch {
        /// The new order.
        oid: Oid,
        /// The anchor it named.
        before: Oid,
    },
    /// An add-liquidity-only order would cross the opposite best price, so it is rejected.
    #[error("ALO order {oid:?} at {px:?} would cross opposite best {opposite_best:?}")]
    AloWouldCross {
        /// The rejected order.
        oid: Oid,
        /// Its price.
        px: Px,
        /// The opposite side's best price at the time.
        opposite_best: Px,
    },
    /// The op would make a price level's total size exceed `u64::MAX` lots,
    /// which no aggregate (L2 size) could then represent.
    #[error("order {oid:?} would make the total size at {px:?} exceed u64::MAX lots")]
    LevelSizeOverflow {
        /// The order inserted or resized.
        oid: Oid,
        /// Its price level.
        px: Px,
    },
    /// A resting insert would cross the opposite best price: the op stream is inconsistent.
    #[error("order {oid:?} at {px:?} crosses opposite best {opposite_best:?}")]
    Crossed {
        /// The offending order.
        oid: Oid,
        /// Its price.
        px: Px,
        /// The opposite side's best price at the time.
        opposite_best: Px,
    },
}

/// One price level as seen from outside: the price and its queue, front first.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Level {
    /// Price in ticks.
    pub px: Px,
    /// Resting orders in queue order (front of queue first) with their remaining sizes.
    pub orders: Vec<(Oid, Lots)>,
}

/// A price level whose total size does not fit in `u64` lots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, thiserror::Error)]
#[error("the {side:?} level at {px:?} totals more than u64::MAX lots")]
pub struct SizeOverflow {
    /// Side of the level.
    pub side: Side,
    /// Price of the level.
    pub px: Px,
}

impl BookSnapshot {
    /// Aggregate to L2, keeping at most `depth` levels per side (all when
    /// `None`). Fails if a kept level's total size overflows `u64`: a wrapped
    /// total could otherwise compare equal to a wrong recorded size.
    pub fn to_l2(&self, depth: Option<usize>) -> Result<L2Snapshot, SizeOverflow> {
        let agg = |side: Side, levels: &[Level]| -> Result<Vec<L2Level>, SizeOverflow> {
            levels
                .iter()
                .take(depth.unwrap_or(usize::MAX))
                .map(|l| L2Level::try_from(l).map_err(|()| SizeOverflow { side, px: l.px }))
                .collect()
        };
        Ok(L2Snapshot {
            bids: agg(Side::Bid, &self.bids)?,
            asks: agg(Side::Ask, &self.asks)?,
        })
    }
}

/// Sum of sizes, `None` if it overflows `u64`.
fn checked_total(sizes: impl IntoIterator<Item = Lots>) -> Option<Lots> {
    sizes
        .into_iter()
        .try_fold(0u64, |acc, sz| acc.checked_add(sz.0))
        .map(Lots)
}

impl Level {
    /// Total resting size at this level, or `None` if it overflows `u64`.
    #[must_use]
    pub fn total(&self) -> Option<Lots> {
        checked_total(self.orders.iter().map(|&(_, sz)| sz))
    }
}

/// One aggregated price level, as in Hyperliquid's public `l2Book`: price,
/// total resting size and number of resting orders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct L2Level {
    /// Price in ticks.
    pub px: Px,
    /// Total resting size in lots.
    pub sz: Lots,
    /// Number of resting orders at the level.
    pub n: u32,
}

impl TryFrom<&Level> for L2Level {
    /// The level's total size overflows `u64`.
    type Error = ();

    fn try_from(level: &Level) -> Result<Self, ()> {
        Ok(Self {
            px: level.px,
            sz: level.total().ok_or(())?,
            n: u32::try_from(level.orders.len()).unwrap_or(u32::MAX),
        })
    }
}

/// An aggregated book: bids best (highest) first, asks best (lowest) first.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct L2Snapshot {
    /// Bid levels, highest price first.
    pub bids: Vec<L2Level>,
    /// Ask levels, lowest price first.
    pub asks: Vec<L2Level>,
}

/// Time in force for [`Book::place`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tif {
    /// Good till cancelled: match, then rest any remainder.
    Gtc,
    /// Immediate or cancel: match, then drop any remainder. Never rests.
    Ioc,
    /// Add liquidity only: rejected if it would cross, otherwise rests without matching.
    Alo,
}

/// One fill against a resting maker, at the maker's price.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fill {
    /// The resting order that was hit.
    pub maker: Oid,
    /// Fill price: the maker's resting price.
    pub px: Px,
    /// Filled size.
    pub sz: Lots,
}

/// What [`Book::place`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Placed {
    /// Fills in execution order.
    pub fills: Vec<Fill>,
    /// Size that came to rest under the incoming order's id, if any.
    pub rested: Option<Lots>,
    /// Resting orders of the same owner removed by self-trade prevention, in
    /// the order they were met.
    pub self_trade_cancels: Vec<Oid>,
}

/// The whole book as plain data: bids best (highest) first, asks best (lowest) first.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct BookSnapshot {
    /// Bid levels, highest price first.
    pub bids: Vec<Level>,
    /// Ask levels, lowest price first.
    pub asks: Vec<Level>,
}

/// The reference order book.
#[derive(Clone, Debug, Default)]
pub struct Book {
    bids: BTreeMap<Px, VecDeque<Order>>,
    asks: BTreeMap<Px, VecDeque<Order>>,
    index: HashMap<Oid, (Side, Px)>,
    owners: HashMap<Oid, Owner>,
    config: MatchConfig,
}

impl Book {
    /// An empty book.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty book with the given matching rules.
    #[must_use]
    pub fn with_config(config: MatchConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// The matching rules in force.
    #[must_use]
    pub fn config(&self) -> MatchConfig {
        self.config
    }

    /// The owner of a resting order, if it was placed with one.
    #[must_use]
    pub fn owner(&self, oid: Oid) -> Option<Owner> {
        self.owners.get(&oid).copied()
    }

    /// Rebuild a book from a snapshot, preserving queue order within each level.
    ///
    /// Fails if the snapshot repeats an order id, holds a zero size, or has its
    /// sides crossed.
    pub fn from_snapshot(snap: &BookSnapshot) -> Result<Self, ApplyError> {
        let mut book = Self::new();
        for (side, levels) in [(Side::Bid, &snap.bids), (Side::Ask, &snap.asks)] {
            for level in levels {
                for &(oid, sz) in &level.orders {
                    book.apply(&BookOp::Insert(Order {
                        oid,
                        side,
                        px: level.px,
                        sz,
                    }))?;
                }
            }
        }
        Ok(book)
    }

    /// Apply one op. On error the book is unchanged.
    pub fn apply(&mut self, op: &BookOp) -> Result<(), ApplyError> {
        match *op {
            BookOp::Insert(order) => self.insert(order),
            BookOp::InsertBefore { before, order } => self.insert_before(before, order),
            BookOp::Cancel { oid } => self.cancel(oid),
            BookOp::Resize { oid, sz } => self.resize(oid, sz),
        }
    }

    /// Size and id checks shared by every way an order enters the book.
    fn validate_new(&self, order: Order) -> Result<(), ApplyError> {
        if order.sz.0 == 0 {
            return Err(ApplyError::ZeroSize(order.oid));
        }
        if self.index.contains_key(&order.oid) {
            return Err(ApplyError::DuplicateOid(order.oid));
        }
        Ok(())
    }

    /// The opposite best price if `order` would cross it.
    fn crossing(&self, order: Order) -> Option<Px> {
        let opposite_best = self.best(order.side.opposite())?;
        let crosses = match order.side {
            Side::Bid => order.px >= opposite_best,
            Side::Ask => order.px <= opposite_best,
        };
        crosses.then_some(opposite_best)
    }

    fn check_resting(&self, order: Order) -> Result<(), ApplyError> {
        self.validate_new(order)?;
        if let Some(opposite_best) = self.crossing(order) {
            return Err(ApplyError::Crossed {
                oid: order.oid,
                px: order.px,
                opposite_best,
            });
        }
        self.check_room(order.side, order.px, order.oid, Lots(0), order.sz)
    }

    /// Fail unless the level at `px` can hold `add` more lots after `remove`
    /// lots leave it, so every level total stays representable in `u64`.
    fn check_room(
        &self,
        side: Side,
        px: Px,
        oid: Oid,
        remove: Lots,
        add: Lots,
    ) -> Result<(), ApplyError> {
        let overflow = ApplyError::LevelSizeOverflow { oid, px };
        let current = match self.side_ref(side).get(&px) {
            Some(q) => checked_total(q.iter().map(|o| o.sz)).ok_or(overflow)?,
            None => Lots(0),
        };
        current
            .0
            .checked_sub(remove.0)
            .and_then(|t| t.checked_add(add.0))
            .map(|_| ())
            .ok_or(overflow)
    }

    fn insert(&mut self, order: Order) -> Result<(), ApplyError> {
        self.check_resting(order)?;
        self.side_mut(order.side)
            .entry(order.px)
            .or_default()
            .push_back(order);
        self.index.insert(order.oid, (order.side, order.px));
        Ok(())
    }

    fn insert_before(&mut self, before: Oid, order: Order) -> Result<(), ApplyError> {
        let (side, px) = *self
            .index
            .get(&before)
            .ok_or(ApplyError::UnknownOid(before))?;
        if side != order.side || px != order.px {
            return Err(ApplyError::AnchorMismatch {
                oid: order.oid,
                before,
            });
        }
        self.check_resting(order)?;
        let Some(queue) = self.side_mut(side).get_mut(&px) else {
            unreachable!("index points at a missing level for {before:?}");
        };
        let Some(pos) = queue.iter().position(|o| o.oid == before) else {
            unreachable!("index points at a level that lacks {before:?}");
        };
        queue.insert(pos, order);
        self.index.insert(order.oid, (side, px));
        Ok(())
    }

    /// Place an incoming order with no owner: see [`Book::place_owned`].
    /// Without an owner self-trade prevention never applies.
    pub fn place(&mut self, order: Order, tif: Tif) -> Result<Placed, ApplyError> {
        self.place_inner(None, order, tif)
    }

    /// Place an incoming order owned by `owner`: match it against the
    /// opposite side (best price first, FIFO within a level, fills at the
    /// resting price), then rest the remainder for [`Tif::Gtc`], drop it for
    /// [`Tif::Ioc`]. A [`Tif::Alo`] order that would cross is rejected;
    /// otherwise it rests without matching. With
    /// [`MatchConfig::self_trade_prevention`] on, a resting order of the same
    /// owner is cancelled instead of filled and matching continues.
    ///
    /// Fill-at-resting-price is INFERENCE (`ACTION_ORDERING.md` §3.5). On error
    /// the book is unchanged.
    pub fn place_owned(
        &mut self,
        owner: Owner,
        order: Order,
        tif: Tif,
    ) -> Result<Placed, ApplyError> {
        self.place_inner(Some(owner), order, tif)
    }

    /// Modify = cancel `old` (if it rests) then place `new` as a new order
    /// with its own id at the back of its level. The new order is placed even
    /// if the cancel found nothing. Hyperliquid categorises a modify by the
    /// new order it places and documents `always_place`; that a smaller
    /// modify loses queue position is INFERENCE (`ACTION_ORDERING.md` §1.1).
    pub fn modify(&mut self, old: Oid, owner: Option<Owner>, new: Order, tif: Tif) -> Modified {
        let cancelled = self.cancel(old).is_ok();
        Modified {
            cancelled,
            placed: self.place_inner(owner, new, tif),
        }
    }

    fn place_inner(
        &mut self,
        owner: Option<Owner>,
        order: Order,
        tif: Tif,
    ) -> Result<Placed, ApplyError> {
        self.validate_new(order)?;
        if tif == Tif::Alo {
            if let Some(opposite_best) = self.crossing(order) {
                return Err(ApplyError::AloWouldCross {
                    oid: order.oid,
                    px: order.px,
                    opposite_best,
                });
            }
            self.rest(owner, order)?;
            return Ok(Placed {
                rested: Some(order.sz),
                ..Placed::default()
            });
        }
        if tif == Tif::Gtc {
            // Check before any fill so an error leaves the book unchanged. A
            // crossing order has no own-side level at its price (the book is
            // never crossed), so this only bites when it would rest whole.
            self.check_room(order.side, order.px, order.oid, Lots(0), order.sz)?;
        }
        // TODO(UNVERIFIED-§3.5-matching): best price first, FIFO, fills at the
        // resting price is INFERENCE with no primary statement.
        let stp_owner = owner.filter(|_| self.config.self_trade_prevention);
        let mut remaining = order.sz.0;
        let mut placed = Placed::default();
        while remaining > 0 {
            let Some(best) = self.crossing(Order {
                sz: Lots(remaining),
                ..order
            }) else {
                break;
            };
            let opposite = order.side.opposite();
            let Some(maker) = self
                .side_ref(opposite)
                .get(&best)
                .and_then(VecDeque::front)
                .copied()
            else {
                unreachable!("best price {best:?} has no order");
            };
            if stp_owner.is_some() && self.owners.get(&maker.oid).copied() == stp_owner {
                self.cancel(maker.oid)?;
                placed.self_trade_cancels.push(maker.oid);
                continue;
            }
            let take = remaining.min(maker.sz.0);
            placed.fills.push(Fill {
                maker: maker.oid,
                px: best,
                sz: Lots(take),
            });
            remaining -= take;
            if take == maker.sz.0 {
                self.cancel(maker.oid)?;
            } else {
                self.resize(maker.oid, Lots(maker.sz.0 - take))?;
            }
        }
        if remaining > 0 && tif == Tif::Gtc {
            self.rest(
                owner,
                Order {
                    sz: Lots(remaining),
                    ..order
                },
            )?;
            placed.rested = Some(Lots(remaining));
        }
        Ok(placed)
    }

    fn rest(&mut self, owner: Option<Owner>, order: Order) -> Result<(), ApplyError> {
        self.insert(order)?;
        if let Some(owner) = owner {
            self.owners.insert(order.oid, owner);
        }
        Ok(())
    }

    fn cancel(&mut self, oid: Oid) -> Result<(), ApplyError> {
        let (side, px) = *self.index.get(&oid).ok_or(ApplyError::UnknownOid(oid))?;
        let levels = self.side_mut(side);
        let Some(queue) = levels.get_mut(&px) else {
            // The index and the levels disagree: a bug in this crate, not in the input.
            unreachable!("index points at a missing level for {oid:?}");
        };
        let Some(pos) = queue.iter().position(|o| o.oid == oid) else {
            unreachable!("index points at a level that lacks {oid:?}");
        };
        queue.remove(pos);
        if queue.is_empty() {
            levels.remove(&px);
        }
        self.index.remove(&oid);
        self.owners.remove(&oid);
        Ok(())
    }

    fn resize(&mut self, oid: Oid, sz: Lots) -> Result<(), ApplyError> {
        // An unknown id is the more telling error, so it is checked before the size.
        let (side, px) = *self.index.get(&oid).ok_or(ApplyError::UnknownOid(oid))?;
        if sz.0 == 0 {
            return Err(ApplyError::ZeroSize(oid));
        }
        let old = self.get(oid).map_or(Lots(0), |o| o.sz);
        self.check_room(side, px, oid, old, sz)?;
        let Some(order) = self
            .side_mut(side)
            .get_mut(&px)
            .and_then(|q| q.iter_mut().find(|o| o.oid == oid))
        else {
            unreachable!("index points at a level that lacks {oid:?}");
        };
        order.sz = sz;
        Ok(())
    }

    fn side_mut(&mut self, side: Side) -> &mut BTreeMap<Px, VecDeque<Order>> {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    fn side_ref(&self, side: Side) -> &BTreeMap<Px, VecDeque<Order>> {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    /// Best price on a side: highest bid or lowest ask.
    #[must_use]
    pub fn best(&self, side: Side) -> Option<Px> {
        match side {
            Side::Bid => self.bids.keys().next_back().copied(),
            Side::Ask => self.asks.keys().next().copied(),
        }
    }

    /// Highest bid price, if any bids rest.
    #[must_use]
    pub fn best_bid(&self) -> Option<Px> {
        self.best(Side::Bid)
    }

    /// Lowest ask price, if any asks rest.
    #[must_use]
    pub fn best_ask(&self) -> Option<Px> {
        self.best(Side::Ask)
    }

    /// Number of resting orders on both sides.
    #[must_use]
    pub fn order_count(&self) -> usize {
        self.index.len()
    }

    /// A resting order by id.
    #[must_use]
    pub fn get(&self, oid: Oid) -> Option<&Order> {
        let (side, px) = *self.index.get(&oid)?;
        self.side_ref(side).get(&px)?.iter().find(|o| o.oid == oid)
    }

    /// The level at `px` on `side`, if any order rests there.
    #[must_use]
    pub fn level(&self, side: Side, px: Px) -> Option<Level> {
        let queue = self.side_ref(side).get(&px)?;
        Some(Level {
            px,
            orders: queue.iter().map(|o| (o.oid, o.sz)).collect(),
        })
    }

    /// The book aggregated to L2, at most `depth` levels per side (all when
    /// `None`). Sums are checked; this book never holds a level whose total
    /// overflows (inserts and resizes that would are rejected), so the error
    /// is unreachable here but kept typed for callers.
    pub fn l2(&self, depth: Option<usize>) -> Result<L2Snapshot, SizeOverflow> {
        let agg = |side: Side,
                   it: &mut dyn Iterator<Item = (&Px, &VecDeque<Order>)>|
         -> Result<Vec<L2Level>, SizeOverflow> {
            it.take(depth.unwrap_or(usize::MAX))
                .map(|(px, q)| {
                    Ok(L2Level {
                        px: *px,
                        sz: checked_total(q.iter().map(|o| o.sz))
                            .ok_or(SizeOverflow { side, px: *px })?,
                        n: u32::try_from(q.len()).unwrap_or(u32::MAX),
                    })
                })
                .collect()
        };
        Ok(L2Snapshot {
            bids: agg(Side::Bid, &mut self.bids.iter().rev())?,
            asks: agg(Side::Ask, &mut self.asks.iter())?,
        })
    }

    /// The whole book as plain data, bids highest first and asks lowest first.
    #[must_use]
    pub fn snapshot(&self) -> BookSnapshot {
        let to_level = |(px, queue): (&Px, &VecDeque<Order>)| Level {
            px: *px,
            orders: queue.iter().map(|o| (o.oid, o.sz)).collect(),
        };
        BookSnapshot {
            bids: self.bids.iter().rev().map(to_level).collect(),
            asks: self.asks.iter().map(to_level).collect(),
        }
    }
}
