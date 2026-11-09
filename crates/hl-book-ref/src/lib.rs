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

impl Level {
    /// Total resting size at this level.
    #[must_use]
    pub fn total(&self) -> Lots {
        Lots(self.orders.iter().map(|(_, sz)| sz.0).sum())
    }
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
            BookOp::Cancel { oid } => self.cancel(oid),
            BookOp::Resize { oid, sz } => self.resize(oid, sz),
        }
    }

    fn insert(&mut self, order: Order) -> Result<(), ApplyError> {
        if order.sz.0 == 0 {
            return Err(ApplyError::ZeroSize(order.oid));
        }
        if self.index.contains_key(&order.oid) {
            return Err(ApplyError::DuplicateOid(order.oid));
        }
        if let Some(opposite_best) = self.best(order.side.opposite()) {
            let crosses = match order.side {
                Side::Bid => order.px >= opposite_best,
                Side::Ask => order.px <= opposite_best,
            };
            if crosses {
                return Err(ApplyError::Crossed {
                    oid: order.oid,
                    px: order.px,
                    opposite_best,
                });
            }
        }
        self.side_mut(order.side)
            .entry(order.px)
            .or_default()
            .push_back(order);
        self.index.insert(order.oid, (order.side, order.px));
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
