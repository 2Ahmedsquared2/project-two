//! One asset's book: a preallocated arena of order nodes, FIFO price levels
//! as intrusive doubly linked lists, and per side a sorted ladder of
//! `(price, level)` with the best price last (bids ascending, asks
//! descending), so the best level is `O(1)` and a fill that empties it is a
//! `pop`.
//!
//! Each level keeps `total` (sum of its orders' sizes) and `count`
//! (number of orders) up to date on every change: that is the incremental
//! L2 view. Invariants (checked by [`Book::check_invariants`], Kani in
//! `proofs` and the Lean model `formal/lean/Model/HlBook.lean`):
//!
//! 1. every live level is non-empty and its `total` / `count` equal the sum /
//!    number of its orders;
//! 2. each ladder is strictly sorted (bids ascending, asks descending) and
//!    lists exactly the live levels of its side;
//! 3. the book is not crossed: best bid < best ask;
//! 4. every resting order has a non-zero size.

use crate::types::{Asset, BookDiff, Cloid, DiffKind, Fill, Oid, Px, Side, Sz, User};

/// "No slot".
pub(crate) const NIL: u32 = u32::MAX;

/// A resting order in the arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    /// Order id.
    pub oid: Oid,
    /// Owner.
    pub user: User,
    /// Client order id.
    pub cloid: Option<Cloid>,
    /// Price.
    pub px: Px,
    /// Remaining size (non-zero while live).
    pub sz: Sz,
    /// Side.
    pub side: Side,
    /// Block time (ms) it came to rest.
    pub t_ms: u64,
    /// Order-priority fee rate of the action that placed it (0 = none).
    pub prio: u64,
    level: u32,
    prev: u32,
    next: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LevelNode {
    px: Px,
    total: u64,
    count: u32,
    head: u32,
    tail: u32,
}

/// Why a book operation was refused. The book is unchanged after an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BookError {
    /// Zero size.
    ZeroSize,
    /// The level total would exceed `u64::MAX`.
    LevelOverflow,
    /// A resting order would cross the opposite best.
    Crossed,
    /// The `insert_before` anchor is on another side or price.
    AnchorMismatch,
    /// The slot is not a live order.
    UnknownSlot,
    /// An update's `orig` is not the order's current size.
    OrigMismatch,
    /// More than `u32::MAX - 1` orders or levels.
    CapacityExhausted,
}

/// An order about to rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewOrder {
    /// Order id.
    pub oid: Oid,
    /// Owner.
    pub user: User,
    /// Client order id.
    pub cloid: Option<Cloid>,
    /// Side.
    pub side: Side,
    /// Price.
    pub px: Px,
    /// Size.
    pub sz: Sz,
    /// Block time (ms).
    pub t_ms: u64,
    /// Priority fee rate (0 = none).
    pub prio: u64,
}

/// What [`Book::match_taker`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchResult {
    /// Total filled lots.
    pub filled: u64,
    /// Sum of `px * sz` over the fills. Cannot overflow: the fills sum to at
    /// most the taker size (`< 2^64`) and each price is `< 2^64`.
    pub notional: u128,
    /// Lots left unmatched.
    pub remaining: u64,
    /// Resting orders of the taker's own user removed by self-trade prevention.
    pub stp_cancels: u32,
}

/// The taker side of a match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Taker {
    /// Taker oid.
    pub oid: Oid,
    /// Taker user.
    pub user: User,
    /// Taker side.
    pub side: Side,
    /// Limit price.
    pub px: Px,
    /// Size.
    pub sz: Sz,
    /// Self-trade prevention: a resting order of the same user is removed
    /// instead of filled and matching continues (Hyperliquid docs,
    /// trading/self-trade-prevention; exact-address scope).
    pub stp: bool,
}

/// One journaled change, undone by [`Book::rollback`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Undo {
    Rested {
        slot: u32,
        grew_node: bool,
        grew_level: bool,
    },
    Resized {
        slot: u32,
        old: Sz,
    },
    Removed {
        node: Node,
        slot: u32,
        level_dropped: bool,
    },
}

/// What [`Book::rollback`] reports for each order it touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restored {
    /// The order was rested during the block and is gone again.
    Unrested(Node),
    /// The order was removed during the block and is back in `slot`.
    Back(Node, u32),
}

/// One asset's book.
#[derive(Clone, Debug)]
pub struct Book {
    asset: Asset,
    nodes: Vec<Node>,
    free_nodes: Vec<u32>,
    levels: Vec<LevelNode>,
    free_levels: Vec<u32>,
    /// Ascending: best bid last.
    bids: Vec<(Px, u32)>,
    /// Descending: best ask last.
    asks: Vec<(Px, u32)>,
    live: usize,
    journal: Vec<Undo>,
}

fn crosses(side: Side, px: Px, opposite_best: Px) -> bool {
    match side {
        Side::Bid => px >= opposite_best,
        Side::Ask => px <= opposite_best,
    }
}

fn idx(slot: u32) -> usize {
    // u32 -> usize is lossless on every supported (>= 32-bit) target.
    usize::try_from(slot).unwrap_or(usize::MAX)
}

impl Book {
    /// An empty book with room for `orders` orders and `levels` levels per side
    /// before anything allocates.
    #[must_use]
    pub fn with_capacity(asset: Asset, orders: usize, levels: usize) -> Self {
        Self {
            asset,
            nodes: Vec::with_capacity(orders),
            free_nodes: Vec::with_capacity(orders),
            levels: Vec::with_capacity(levels.saturating_mul(2)),
            free_levels: Vec::with_capacity(levels.saturating_mul(2)),
            bids: Vec::with_capacity(levels),
            asks: Vec::with_capacity(levels),
            live: 0,
            journal: Vec::with_capacity(orders.min(1 << 16)),
        }
    }

    /// Forget the journal: changes before this point can no longer be
    /// undone. Every change is journaled until the next `begin`.
    pub fn begin(&mut self) {
        self.journal.clear();
    }

    /// Undo every change since [`Book::begin`], newest first, restoring the
    /// exact slots (the free lists are stacks and every push/pop is reversed).
    /// `on` sees each order whose presence changed.
    pub fn rollback(&mut self, mut on: impl FnMut(Restored)) {
        while let Some(u) = self.journal.pop() {
            match u {
                Undo::Resized { slot, old } => {
                    let n = self.nodes[idx(slot)];
                    let lv = &mut self.levels[idx(n.level)];
                    lv.total = lv.total.saturating_sub(n.sz.0).saturating_add(old.0);
                    self.nodes[idx(slot)].sz = old;
                }
                Undo::Rested {
                    slot,
                    grew_node,
                    grew_level,
                } => {
                    let n = self.nodes[idx(slot)];
                    self.unlink(slot);
                    if grew_node {
                        // A grown slot is the newest one: everything after it is undone.
                        let _ = self.free_nodes.pop();
                        self.nodes.pop();
                    }
                    if grew_level && self.levels.get(idx(n.level)).is_some() {
                        let last = u32::try_from(self.levels.len() - 1).unwrap_or(NIL);
                        if last == n.level && self.free_levels.last() == Some(&last) {
                            self.free_levels.pop();
                            self.levels.pop();
                        }
                    }
                    on(Restored::Unrested(n));
                }
                Undo::Removed {
                    node,
                    slot,
                    level_dropped,
                } => {
                    if level_dropped {
                        let l = self.free_levels.pop().unwrap_or(node.level);
                        debug_assert_eq!(l, node.level);
                        self.levels[idx(l)] = LevelNode {
                            px: node.px,
                            total: 0,
                            count: 0,
                            head: NIL,
                            tail: NIL,
                        };
                        if let Err(i) = self.find(node.side, node.px) {
                            self.ladder_mut(node.side).insert(i, (node.px, l));
                        }
                    }
                    let s = self.free_nodes.pop().unwrap_or(slot);
                    debug_assert_eq!(s, slot);
                    self.nodes[idx(slot)] = node;
                    let lvl = node.level;
                    if node.prev == NIL {
                        self.levels[idx(lvl)].head = slot;
                    } else {
                        self.nodes[idx(node.prev)].next = slot;
                    }
                    if node.next == NIL {
                        self.levels[idx(lvl)].tail = slot;
                    } else {
                        self.nodes[idx(node.next)].prev = slot;
                    }
                    let lv = &mut self.levels[idx(lvl)];
                    lv.total = lv.total.saturating_add(node.sz.0);
                    lv.count = lv.count.saturating_add(1);
                    self.live += 1;
                    on(Restored::Back(node, slot));
                }
            }
        }
    }

    /// The asset.
    #[must_use]
    pub fn asset(&self) -> Asset {
        self.asset
    }

    /// Resting orders.
    #[must_use]
    pub fn order_count(&self) -> usize {
        self.live
    }

    fn ladder(&self, side: Side) -> &Vec<(Px, u32)> {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    fn ladder_mut(&mut self, side: Side) -> &mut Vec<(Px, u32)> {
        match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    /// Ladder position of `px` (`Ok`) or where it would go (`Err`).
    fn find(&self, side: Side, px: Px) -> Result<usize, usize> {
        match side {
            Side::Bid => self.bids.binary_search_by(|e| e.0.cmp(&px)),
            Side::Ask => self.asks.binary_search_by(|e| px.cmp(&e.0)),
        }
    }

    /// Best price on a side.
    #[must_use]
    pub fn best(&self, side: Side) -> Option<Px> {
        self.ladder(side).last().map(|e| e.0)
    }

    /// The opposite best price if an order at `px` on `side` would cross it.
    #[must_use]
    pub fn crossing(&self, side: Side, px: Px) -> Option<Px> {
        let ob = self.best(side.opposite())?;
        crosses(side, px, ob).then_some(ob)
    }

    /// A live order by slot.
    #[must_use]
    pub fn node(&self, slot: u32) -> Option<&Node> {
        self.nodes.get(idx(slot)).filter(|n| n.level != NIL)
    }

    /// Current total at `(side, px)` (0 when no level).
    #[must_use]
    pub fn level_total(&self, side: Side, px: Px) -> u64 {
        match self.find(side, px) {
            Ok(i) => self.levels[idx(self.ladder(side)[i].1)].total,
            Err(_) => 0,
        }
    }

    /// Whether `add` more lots fit at `(side, px)`.
    #[must_use]
    pub fn has_room(&self, side: Side, px: Px, add: u64) -> bool {
        self.level_total(side, px).checked_add(add).is_some()
    }

    /// The incremental L2 view of a side, best first: `(px, total, count)`.
    pub fn l2(&self, side: Side) -> impl Iterator<Item = (Px, Sz, u32)> + '_ {
        self.ladder(side).iter().rev().map(move |&(px, l)| {
            let lv = &self.levels[idx(l)];
            (px, Sz(lv.total), lv.count)
        })
    }

    /// Price levels of a side, best first.
    pub fn levels(&self, side: Side) -> impl Iterator<Item = Px> + '_ {
        self.ladder(side).iter().rev().map(|e| e.0)
    }

    /// Orders of the level at `(side, px)`, front of queue first.
    pub fn queue(&self, side: Side, px: Px) -> impl Iterator<Item = &Node> + '_ {
        let mut cur = match self.find(side, px) {
            Ok(i) => self.levels[idx(self.ladder(side)[i].1)].head,
            Err(_) => NIL,
        };
        std::iter::from_fn(move || {
            if cur == NIL {
                return None;
            }
            let n = &self.nodes[idx(cur)];
            cur = n.next;
            Some(n)
        })
    }

    /// ALO priority placement (`ACTION_ORDERING` 3.3, VERIFIED in Hyperliquid's
    /// docs, UNVERIFIED against node diffs): orders placed within the last
    /// `window_ms` form the tail of a level; a new ALO with priority `prio`
    /// goes in front of the frontmost tail order of a run (counted from the
    /// back) whose priority is lower. Returns that order's slot, or `None`
    /// for the back of the level.
    #[must_use]
    pub fn priority_anchor(
        &self,
        side: Side,
        px: Px,
        prio: u64,
        now_ms: u64,
        window_ms: u64,
    ) -> Option<u32> {
        if prio == 0 {
            return None;
        }
        let Ok(i) = self.find(side, px) else {
            return None;
        };
        let mut cur = self.levels[idx(self.ladder(side)[i].1)].tail;
        let mut anchor = None;
        while cur != NIL {
            let n = &self.nodes[idx(cur)];
            if n.t_ms.saturating_add(window_ms) <= now_ms || n.prio >= prio {
                break;
            }
            anchor = Some(cur);
            cur = n.prev;
        }
        anchor
    }

    fn alloc_node(&mut self, n: Node) -> Result<u32, BookError> {
        if let Some(s) = self.free_nodes.pop() {
            self.nodes[idx(s)] = n;
            return Ok(s);
        }
        let s = u32::try_from(self.nodes.len())
            .ok()
            .filter(|&s| s != NIL)
            .ok_or(BookError::CapacityExhausted)?;
        self.nodes.push(n);
        Ok(s)
    }

    fn alloc_level(&mut self, px: Px) -> Result<u32, BookError> {
        let l = LevelNode {
            px,
            total: 0,
            count: 0,
            head: NIL,
            tail: NIL,
        };
        if let Some(s) = self.free_levels.pop() {
            self.levels[idx(s)] = l;
            return Ok(s);
        }
        let s = u32::try_from(self.levels.len())
            .ok()
            .filter(|&s| s != NIL)
            .ok_or(BookError::CapacityExhausted)?;
        self.levels.push(l);
        Ok(s)
    }

    /// Rest an order at the back of its level, or directly in front of the
    /// live order in slot `before` (same side and price). Checks, in this
    /// order: anchor, zero size, crossing, level overflow. Returns its slot.
    pub fn rest(&mut self, o: NewOrder, before: Option<u32>) -> Result<u32, BookError> {
        let NewOrder {
            oid,
            user,
            cloid,
            side,
            px,
            sz,
            t_ms,
            prio,
        } = o;
        if let Some(b) = before {
            let a = self.node(b).ok_or(BookError::UnknownSlot)?;
            if a.side != side || a.px != px {
                return Err(BookError::AnchorMismatch);
            }
        }
        if sz.0 == 0 {
            return Err(BookError::ZeroSize);
        }
        if self.crossing(side, px).is_some() {
            return Err(BookError::Crossed);
        }
        let pos = self.find(side, px);
        if let Ok(i) = pos {
            let t = self.levels[idx(self.ladder(side)[i].1)].total;
            if t.checked_add(sz.0).is_none() {
                return Err(BookError::LevelOverflow);
            }
        }
        let grew_node = self.free_nodes.is_empty();
        let grew_level = pos.is_err() && self.free_levels.is_empty();
        let slot = self.alloc_node(Node {
            oid,
            user,
            cloid,
            px,
            sz,
            side,
            t_ms,
            prio,
            level: NIL,
            prev: NIL,
            next: NIL,
        })?;
        let lvl = match pos {
            Ok(i) => self.ladder(side)[i].1,
            Err(i) => match self.alloc_level(px) {
                Ok(l) => {
                    self.ladder_mut(side).insert(i, (px, l));
                    l
                }
                Err(e) => {
                    self.nodes[idx(slot)].level = NIL;
                    self.free_nodes.push(slot);
                    return Err(e);
                }
            },
        };
        let lv = self.levels[idx(lvl)];
        let (prev, next) = match before {
            Some(b) => (self.nodes[idx(b)].prev, b),
            None => (lv.tail, NIL),
        };
        {
            let n = &mut self.nodes[idx(slot)];
            n.level = lvl;
            n.prev = prev;
            n.next = next;
        }
        if prev == NIL {
            self.levels[idx(lvl)].head = slot;
        } else {
            self.nodes[idx(prev)].next = slot;
        }
        if next == NIL {
            self.levels[idx(lvl)].tail = slot;
        } else {
            self.nodes[idx(next)].prev = slot;
        }
        let lv = &mut self.levels[idx(lvl)];
        // Checked above for an existing level; a new level starts at 0.
        lv.total = lv.total.saturating_add(sz.0);
        lv.count = lv.count.saturating_add(1);
        self.live += 1;
        {
            self.journal.push(Undo::Rested {
                slot,
                grew_node,
                grew_level,
            });
        }
        Ok(slot)
    }

    /// Remove a live order; drops its level when it was the last one.
    pub fn remove(&mut self, slot: u32) -> Result<Node, BookError> {
        let n = *self.node(slot).ok_or(BookError::UnknownSlot)?;
        let level_dropped = self.unlink(slot);
        {
            self.journal.push(Undo::Removed {
                node: n,
                slot,
                level_dropped,
            });
        }
        Ok(n)
    }

    /// Unlink a live order and free its slot (and its level if emptied).
    /// Returns whether the level was dropped.
    fn unlink(&mut self, slot: u32) -> bool {
        let n = self.nodes[idx(slot)];
        if n.prev == NIL {
            self.levels[idx(n.level)].head = n.next;
        } else {
            self.nodes[idx(n.prev)].next = n.next;
        }
        if n.next == NIL {
            self.levels[idx(n.level)].tail = n.prev;
        } else {
            self.nodes[idx(n.next)].prev = n.prev;
        }
        let lv = &mut self.levels[idx(n.level)];
        // Invariant 1: total >= this order's size and count >= 1.
        lv.total = lv.total.saturating_sub(n.sz.0);
        lv.count = lv.count.saturating_sub(1);
        let dropped = lv.count == 0;
        if dropped {
            if let Ok(i) = self.find(n.side, n.px) {
                self.ladder_mut(n.side).remove(i);
            }
            self.free_levels.push(n.level);
        }
        self.nodes[idx(slot)].level = NIL;
        self.free_nodes.push(slot);
        self.live -= 1;
        dropped
    }

    /// Change a live order's size in place (queue position kept). Returns
    /// the old size. Checks, in this order: zero size, overflow.
    pub fn resize(&mut self, slot: u32, sz: Sz) -> Result<Sz, BookError> {
        let n = *self.node(slot).ok_or(BookError::UnknownSlot)?;
        if sz.0 == 0 {
            return Err(BookError::ZeroSize);
        }
        let lv = self.levels[idx(n.level)];
        let total = lv
            .total
            .checked_sub(n.sz.0)
            .and_then(|t| t.checked_add(sz.0))
            .ok_or(BookError::LevelOverflow)?;
        self.levels[idx(n.level)].total = total;
        self.nodes[idx(slot)].sz = sz;
        {
            self.journal.push(Undo::Resized { slot, old: n.sz });
        }
        Ok(n.sz)
    }

    fn diff(&self, n: &Node, kind: DiffKind) -> BookDiff {
        BookDiff {
            asset: self.asset,
            user: n.user,
            oid: n.oid,
            side: n.side,
            px: n.px,
            kind,
            cloid: match kind {
                DiffKind::New { .. } => n.cloid,
                _ => None,
            },
        }
    }

    /// The diff for a live or just-freed slot.
    pub(crate) fn diff_of(&self, slot: u32, kind: DiffKind) -> BookDiff {
        self.diff(&self.nodes[idx(slot)], kind)
    }

    /// A slot's node even after it was freed (valid until the slot is reused).
    pub(crate) fn raw_node(&self, slot: u32) -> &Node {
        &self.nodes[idx(slot)]
    }

    /// Match a taker against the opposite side: best price first, FIFO
    /// within a level, each fill at the resting price, until the taker's
    /// price no longer crosses or its size is used up. Appends the makers'
    /// diffs in native's shape: an `Update` per fill (to 0 for a full fill;
    /// a self-trade cancel is a `Remove` in place), then a `Remove` for each
    /// fully filled maker in fill order. Appends the fills. Never rests
    /// anything.
    pub fn match_taker(
        &mut self,
        t: Taker,
        diffs: &mut Vec<BookDiff>,
        fills: &mut Vec<Fill>,
    ) -> MatchResult {
        let opp = t.side.opposite();
        let d0 = diffs.len();
        let mut r = MatchResult {
            remaining: t.sz.0,
            ..MatchResult::default()
        };
        while r.remaining > 0 {
            let Some(&(best, lvl)) = self.ladder(opp).last() else {
                break;
            };
            if !crosses(t.side, t.px, best) {
                break;
            }
            let slot = self.levels[idx(lvl)].head;
            let maker = self.nodes[idx(slot)];
            if t.stp && maker.user == t.user {
                diffs.push(self.diff(&maker, DiffKind::Remove));
                let _ = self.remove(slot);
                r.stp_cancels = r.stp_cancels.saturating_add(1);
                continue;
            }
            let take = r.remaining.min(maker.sz.0);
            fills.push(Fill {
                asset: self.asset,
                taker: t.oid,
                taker_user: t.user,
                taker_side: t.side,
                maker: maker.oid,
                maker_user: maker.user,
                px: best,
                sz: Sz(take),
            });
            r.remaining -= take;
            r.filled += take;
            r.notional = r
                .notional
                .saturating_add(u128::from(best.0) * u128::from(take));
            if take == maker.sz.0 {
                // Native: `update` to 0 now, `remove` after the taker's run.
                diffs.push(self.diff(
                    &maker,
                    DiffKind::Update {
                        orig: maker.sz,
                        new: Sz(0),
                    },
                ));
                let _ = self.remove(slot);
            } else {
                let new = Sz(maker.sz.0 - take);
                diffs.push(self.diff(
                    &maker,
                    DiffKind::Update {
                        orig: maker.sz,
                        new,
                    },
                ));
                let _ = self.resize(slot, new);
            }
        }
        // Then the fully filled makers' removes, in fill order (no allocation:
        // indexes into the diffs appended above).
        for i in d0..diffs.len() {
            let d = diffs[i];
            if matches!(d.kind, DiffKind::Update { new: Sz(0), .. }) {
                diffs.push(BookDiff {
                    kind: DiffKind::Remove,
                    ..d
                });
            }
        }
        r
    }

    /// Check every invariant in the module docs by walking the whole book.
    /// For tests and proofs; `O(orders)`.
    #[must_use]
    pub fn check_invariants(&self) -> bool {
        let mut seen = 0usize;
        for side in [Side::Bid, Side::Ask] {
            let lad = self.ladder(side);
            for w in lad.windows(2) {
                let ok = match side {
                    Side::Bid => w[0].0 < w[1].0,
                    Side::Ask => w[0].0 > w[1].0,
                };
                if !ok {
                    return false;
                }
            }
            for &(px, l) in lad {
                let Some(lv) = self.levels.get(idx(l)) else {
                    return false;
                };
                if lv.px != px || lv.count == 0 {
                    return false;
                }
                let mut cur = lv.head;
                let mut prev = NIL;
                let mut sum = 0u64;
                let mut cnt = 0u32;
                while cur != NIL {
                    let Some(n) = self.nodes.get(idx(cur)) else {
                        return false;
                    };
                    if n.level != l || n.prev != prev || n.px != px || n.side != side || n.sz.0 == 0
                    {
                        return false;
                    }
                    let Some(s) = sum.checked_add(n.sz.0) else {
                        return false;
                    };
                    sum = s;
                    cnt = cnt.saturating_add(1);
                    if cnt > lv.count {
                        return false;
                    }
                    prev = cur;
                    cur = n.next;
                }
                if prev != lv.tail || sum != lv.total || cnt != lv.count {
                    return false;
                }
                seen += idx(cnt);
            }
        }
        if seen != self.live {
            return false;
        }
        match (self.best(Side::Bid), self.best(Side::Ask)) {
            (Some(b), Some(a)) => b < a,
            _ => true,
        }
    }
}
