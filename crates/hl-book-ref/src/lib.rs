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

impl BookSnapshot {
    /// Aggregate to L2, keeping at most `depth` levels per side (all when `None`).
    #[must_use]
    pub fn to_l2(&self, depth: Option<usize>) -> L2Snapshot {
        let agg = |levels: &[Level]| -> Vec<L2Level> {
            levels
                .iter()
                .take(depth.unwrap_or(usize::MAX))
                .map(L2Level::from)
                .collect()
        };
        L2Snapshot {
            bids: agg(&self.bids),
            asks: agg(&self.asks),
        }
    }
}

impl Level {
    /// Total resting size at this level.
    #[must_use]
    pub fn total(&self) -> Lots {
        Lots(self.orders.iter().map(|(_, sz)| sz.0).sum())
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

impl From<&Level> for L2Level {
    fn from(level: &Level) -> Self {
        Self {
            px: level.px,
            sz: level.total(),
            n: u32::try_from(level.orders.len()).unwrap_or(u32::MAX),
        }
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
}

impl Book {
    /// An empty book.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        Ok(())
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

    /// Place an incoming order: match it against the opposite side (best price
    /// first, FIFO within a level, fills at the resting price), then rest the
    /// remainder for [`Tif::Gtc`], drop it for [`Tif::Ioc`]. A [`Tif::Alo`]
    /// order that would cross is rejected; otherwise it rests without matching.
    ///
    /// Self-trade prevention is not modelled (the Hyperliquid rule is
    /// unverified). On error the book is unchanged.
    pub fn place(&mut self, order: Order, tif: Tif) -> Result<Placed, ApplyError> {
        self.validate_new(order)?;
        if tif == Tif::Alo {
            if let Some(opposite_best) = self.crossing(order) {
                return Err(ApplyError::AloWouldCross {
                    oid: order.oid,
                    px: order.px,
                    opposite_best,
                });
            }
            self.insert(order)?;
            return Ok(Placed {
                fills: Vec::new(),
                rested: Some(order.sz),
            });
        }
        let mut remaining = order.sz.0;
        let mut fills = Vec::new();
        while remaining > 0 {
            let Some(best) = self.crossing(Order {
                sz: Lots(remaining),
                ..order
            }) else {
                break;
            };
            let opposite = order.side.opposite();
            let Some(queue) = self.side_mut(opposite).get_mut(&best) else {
                unreachable!("best price {best:?} has no level");
            };
            let Some(maker) = queue.front_mut() else {
                unreachable!("empty level at {best:?}");
            };
            let take = remaining.min(maker.sz.0);
            fills.push(Fill {
                maker: maker.oid,
                px: best,
                sz: Lots(take),
            });
            remaining -= take;
            maker.sz.0 -= take;
            if maker.sz.0 == 0 {
                let oid = maker.oid;
                queue.pop_front();
                if queue.is_empty() {
                    self.side_mut(opposite).remove(&best);
                }
                self.index.remove(&oid);
            }
        }
        let rested = if remaining > 0 && tif == Tif::Gtc {
            self.insert(Order {
                sz: Lots(remaining),
                ..order
            })?;
            Some(Lots(remaining))
        } else {
            None
        };
        Ok(Placed { fills, rested })
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
        Ok(())
    }

    fn resize(&mut self, oid: Oid, sz: Lots) -> Result<(), ApplyError> {
        // An unknown id is the more telling error, so it is checked before the size.
        let (side, px) = *self.index.get(&oid).ok_or(ApplyError::UnknownOid(oid))?;
        if sz.0 == 0 {
            return Err(ApplyError::ZeroSize(oid));
        }
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

    /// The book aggregated to L2, at most `depth` levels per side (all when `None`).
    #[must_use]
    pub fn l2(&self, depth: Option<usize>) -> L2Snapshot {
        let agg = |it: &mut dyn Iterator<Item = (&Px, &VecDeque<Order>)>| -> Vec<L2Level> {
            it.take(depth.unwrap_or(usize::MAX))
                .map(|(px, q)| L2Level {
                    px: *px,
                    sz: Lots(q.iter().map(|o| o.sz.0).sum()),
                    n: u32::try_from(q.len()).unwrap_or(u32::MAX),
                })
                .collect()
        };
        L2Snapshot {
            bids: agg(&mut self.bids.iter().rev()),
            asks: agg(&mut self.asks.iter()),
        }
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
