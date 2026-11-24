//! The engine: every asset's book, the oid and cloid indexes, the oid
//! counter, and `execute_block`.
//!
//! Rules applied, and where each comes from (nothing here is from the
//! hl-node binary):
//!
//! - actions run in the given order; legs of an action in array order. Our
//!   200-block public sample (heights 1232390001..=1232390200) shows native
//!   oids strictly increasing in file order (0 violations), so the driver
//!   passes actions in file order.
//! - every order leg that reaches the book logic takes the next oid, also
//!   when it is then rejected as an ALO that would cross or an IOC that found
//!   nothing (sample: oid gap of exactly 1 per such reject). A leg rejected by
//!   the driver before that (tick size) takes none (sample: gap 0).
//! - ALO: rejected if it would cross (best bid/ask reported), else rests,
//!   in front of younger lower-priority ALOs when the action carries a
//!   priority fee (`ACTION_ORDERING` 3.3; UNVERIFIED against node diffs).
//! - GTC / IOC: match best price first, FIFO, at the resting price
//!   (`ACTION_ORDERING` 3.5 INFERENCE); GTC rests the remainder, IOC drops it
//!   and is rejected when nothing filled (native "Order could not
//!   immediately match against any resting orders").
//! - self-trade: a resting order of the same user is removed instead of
//!   filled (Hyperliquid docs; exact-address scope is an assumption).
//! - cancel / cancelByCloid: the order must rest on that asset and belong to
//!   the user.
//! - modify: target missing -> rejected, nothing placed, no oid (sample:
//!   "Cannot modify canceled or filled order", gap 0); else the old order is
//!   removed and the new one placed with a new oid.
//! - NOT modelled, answered `Unsupported` (the block cannot count): margin,
//!   balances, reduce-only, trigger / TP-SL orders, open-order limits,
//!   scheduleCancel, TWAP, EVM, every non-trading action, and anything native
//!   does at block boundaries (expiry, liquidation, triggers, funding).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use crate::book::{Book, BookError, NewOrder, Restored, Taker};
use crate::types::{
    ActionResponse, Asset, BlockOutcome, BookDiff, CancelReq, Capacity, Cloid, DiffKind, ModifyReq,
    Oid, OrderRef, OrderReq, OrderStatus, RejectReason, RestingOrder, Side, Sz, Tif, User,
    UserAction,
};

/// A small multiplicative hasher (`FxHash` constant): deterministic and fast
/// for integer keys. Not DoS-resistant; keys come from consensus data.
#[derive(Clone, Copy, Default)]
pub struct FxHasher(u64);

const K: u64 = 0x517c_c1b7_2722_0a95;

impl Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(b));
        }
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(K);
    }
    fn write_u32(&mut self, i: u32) {
        self.write_u64(u64::from(i));
    }
    fn write_u128(&mut self, i: u128) {
        // Truncations are intended: the two halves.
        self.write_u64((i & u128::from(u64::MAX)).try_into().unwrap_or(0));
        self.write_u64((i >> 64).try_into().unwrap_or(0));
    }
    fn write_usize(&mut self, i: usize) {
        self.write_u64(u64::try_from(i).unwrap_or(u64::MAX));
    }
}

type Fx = BuildHasherDefault<FxHasher>;

/// Engine settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineConfig {
    /// Self-trade prevention (default on).
    pub stp: bool,
    /// ALO priority tail window in ms (docs: 400). 0 disables priority placement.
    pub alo_priority_window_ms: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            stp: true,
            alo_priority_window_ms: 400,
        }
    }
}

/// Why a snapshot could not be loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// An order on an asset not in the table.
    UnknownAsset(Asset),
    /// An oid listed twice.
    DuplicateOid(Oid),
    /// A book operation failed (zero size, crossed, overflow).
    Book(Oid, BookError),
}

/// Why [`Engine::resync`] could not apply a native diff. The engine is then
/// in an unspecified (but invariant-respecting) state: reload it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResyncError {
    /// The diff's asset is not in the table.
    UnknownAsset(usize, Asset),
    /// A `New` for an oid that already rests.
    DuplicateOid(usize, Oid),
    /// An `Update` / `Remove` for an oid that does not rest.
    UnknownOid(usize, Oid),
    /// A book operation failed.
    Book(usize, Oid, BookError),
}

/// The execution engine.
#[derive(Clone, Debug)]
pub struct Engine {
    books: Vec<Book>,
    by_asset: HashMap<Asset, u32, Fx>,
    oids: HashMap<Oid, (u32, u32), Fx>,
    cloids: HashMap<(User, Cloid), Oid, Fx>,
    next_oid: u64,
    block_start_oid: u64,
    config: EngineConfig,
    out: BlockOutcome,
    touched: Vec<u32>,
    touched_mark: Vec<bool>,
}

fn ix(b: u32) -> usize {
    usize::try_from(b).unwrap_or(usize::MAX)
}

fn status_idx(v: &[OrderStatus]) -> u32 {
    u32::try_from(v.len()).unwrap_or(u32::MAX)
}

impl Engine {
    /// An empty engine for `assets` (Hyperliquid asset indexes).
    #[must_use]
    pub fn new(assets: &[Asset], cap: Capacity, config: EngineConfig) -> Self {
        let n = assets.len().max(1);
        let per_book = (cap.orders / n).max(64);
        let mut by_asset = HashMap::with_capacity_and_hasher(n, Fx::default());
        let mut books = Vec::with_capacity(n);
        for &a in assets {
            if by_asset.contains_key(&a) {
                continue;
            }
            by_asset.insert(a, u32::try_from(books.len()).unwrap_or(u32::MAX));
            books.push(Book::with_capacity(a, per_book, cap.levels_per_side));
        }
        let mut out = BlockOutcome::default();
        out.responses.reserve(cap.actions_per_block);
        out.statuses.reserve(cap.events_per_block);
        out.diffs.reserve(cap.events_per_block);
        out.fills.reserve(cap.events_per_block);
        Self {
            touched: Vec::with_capacity(books.len()),
            touched_mark: vec![false; books.len()],
            books,
            by_asset,
            oids: HashMap::with_capacity_and_hasher(cap.orders, Fx::default()),
            cloids: HashMap::with_capacity_and_hasher(cap.orders, Fx::default()),
            next_oid: 0,
            block_start_oid: 0,
            config,
            out,
        }
    }

    /// Load the resting orders of an `l4Snapshots` (each side in the
    /// snapshot's queue order: front of queue first). `next_oid` is the oid
    /// native will assign next, if known (else one past the largest oid).
    pub fn from_l4_snapshot(
        assets: &[Asset],
        orders: impl IntoIterator<Item = RestingOrder>,
        next_oid: Option<Oid>,
        cap: Capacity,
        config: EngineConfig,
    ) -> Result<Self, LoadError> {
        let mut e = Self::new(assets, cap, config);
        let mut max = 0u64;
        for o in orders {
            let b = *e
                .by_asset
                .get(&o.asset)
                .ok_or(LoadError::UnknownAsset(o.asset))?;
            if e.oids.contains_key(&o.oid) {
                return Err(LoadError::DuplicateOid(o.oid));
            }
            let slot = e.books[ix(b)]
                .rest(
                    NewOrder {
                        oid: o.oid,
                        user: o.user,
                        cloid: o.cloid,
                        side: o.side,
                        px: o.px,
                        sz: o.sz,
                        t_ms: o.t_ms,
                        prio: 0,
                    },
                    None,
                )
                .map_err(|err| LoadError::Book(o.oid, err))?;
            e.oids.insert(o.oid, (b, slot));
            if let Some(c) = o.cloid {
                e.cloids.insert((o.user, c), o.oid);
            }
            max = max.max(o.oid.0);
        }
        for b in &mut e.books {
            b.begin();
        }
        e.next_oid = next_oid.map_or(max.saturating_add(1), |o| o.0);
        e.block_start_oid = e.next_oid;
        Ok(e)
    }

    /// The book of an asset.
    #[must_use]
    pub fn book(&self, asset: Asset) -> Option<&Book> {
        self.by_asset.get(&asset).map(|&b| &self.books[ix(b)])
    }

    /// Every book.
    pub fn books(&self) -> impl Iterator<Item = &Book> {
        self.books.iter()
    }

    /// The oid the engine will assign next.
    #[must_use]
    pub fn next_oid(&self) -> Oid {
        Oid(self.next_oid)
    }

    /// Resting orders across all books.
    #[must_use]
    pub fn order_count(&self) -> usize {
        self.oids.len()
    }

    /// The last block's outcome.
    #[must_use]
    pub fn outcome(&self) -> &BlockOutcome {
        &self.out
    }

    fn touch(&mut self, b: u32) {
        let i = ix(b);
        if !self.touched_mark[i] {
            self.touched_mark[i] = true;
            self.touched.push(b);
            self.books[i].begin();
        }
    }

    /// Execute one block's actions in order. Zero heap allocation once the
    /// buffers and indexes have reached their working size.
    pub fn execute_block(
        &mut self,
        block_time_ms: u64,
        actions: &[UserAction<'_>],
    ) -> &BlockOutcome {
        for &b in &self.touched {
            self.touched_mark[ix(b)] = false;
        }
        self.touched.clear();
        self.block_start_oid = self.next_oid;
        self.out.clear(block_time_ms);
        for a in actions {
            let start = status_idx(&self.out.statuses);
            let mut unsupported = false;
            match *a {
                UserAction::Order { user, orders } => {
                    for o in orders {
                        let s = self.place(user, o, block_time_ms);
                        self.push_status(s);
                    }
                }
                UserAction::Cancel { user, cancels } => {
                    for c in cancels {
                        let s = self.cancel(user, c);
                        self.push_status(s);
                    }
                }
                UserAction::Modify { user, modifies } => {
                    for m in modifies {
                        let s = self.modify(user, m, block_time_ms);
                        self.push_status(s);
                    }
                }
                UserAction::Noop => {}
                UserAction::Unsupported(_) => {
                    unsupported = true;
                    self.out.has_unsupported = true;
                }
            }
            let end = status_idx(&self.out.statuses);
            self.out.responses.push(ActionResponse {
                start,
                end,
                unsupported,
            });
        }
        &self.out
    }

    fn push_status(&mut self, s: OrderStatus) {
        if s == OrderStatus::Unsupported {
            self.out.has_unsupported = true;
        }
        self.out.statuses.push(s);
    }

    fn take_oid(&mut self, hint: Option<Oid>) -> Oid {
        let oid = match hint {
            Some(h) => {
                if h.0 != self.next_oid {
                    self.out.oid_mismatches = self.out.oid_mismatches.saturating_add(1);
                }
                h
            }
            None => Oid(self.next_oid),
        };
        self.next_oid = oid.0.saturating_add(1);
        oid
    }

    fn place(&mut self, user: User, o: &OrderReq, now: u64) -> OrderStatus {
        if let Some(r) = o.pre_reject {
            return OrderStatus::Rejected(r);
        }
        if o.reduce_only || o.tif == Tif::Other {
            // Consumes an oid on native; keep the counter in step when known.
            if let Some(h) = o.oid_hint {
                self.next_oid = h.0.saturating_add(1);
            }
            return OrderStatus::Unsupported;
        }
        let Some(&b) = self.by_asset.get(&o.asset) else {
            return OrderStatus::Unsupported;
        };
        if o.sz.0 == 0 {
            return OrderStatus::Rejected(RejectReason::ZeroSize);
        }
        if o.px.0 == 0 {
            return OrderStatus::Rejected(RejectReason::ZeroPrice);
        }
        self.touch(b);
        let oid = self.take_oid(o.oid_hint);
        if self.oids.contains_key(&oid) {
            return OrderStatus::Rejected(RejectReason::DuplicateOid);
        }
        let book = &mut self.books[ix(b)];
        let new = NewOrder {
            oid,
            user,
            cloid: o.cloid,
            side: o.side,
            px: o.px,
            sz: o.sz,
            t_ms: now,
            prio: o.priority,
        };
        if o.tif == Tif::Alo {
            return self.place_alo(b, new, user, o, now);
        }
        if o.tif == Tif::Gtc && !book.has_room(o.side, o.px, o.sz.0) {
            return OrderStatus::Rejected(RejectReason::LevelOverflow);
        }
        let d0 = self.out.diffs.len();
        let m = book.match_taker(
            Taker {
                oid,
                user,
                side: o.side,
                px: o.px,
                sz: o.sz,
                stp: self.config.stp,
            },
            &mut self.out.diffs,
            &mut self.out.fills,
        );
        // Drop index entries of makers that left the book.
        for i in d0..self.out.diffs.len() {
            let d = self.out.diffs[i];
            if d.kind == DiffKind::Remove {
                self.unindex(d.oid, d.user);
            }
        }
        let book = &mut self.books[ix(b)];
        let mut rested = Sz(0);
        if o.tif == Tif::Gtc && m.remaining > 0 {
            let rest = NewOrder {
                sz: Sz(m.remaining),
                ..new
            };
            match book.rest(rest, None) {
                Ok(slot) => {
                    self.out.diffs.push(book.diff_of(
                        slot,
                        DiffKind::New {
                            sz: Sz(m.remaining),
                            insert_before: None,
                        },
                    ));
                    self.index_rest(b, slot, user, o.cloid, oid);
                    rested = Sz(m.remaining);
                }
                // Room was checked and the book is never crossed after matching.
                Err(e) => return OrderStatus::Rejected(book_reject(e)),
            }
        }
        if o.tif == Tif::Ioc && m.filled == 0 {
            return OrderStatus::Rejected(RejectReason::IocNoMatch);
        }
        OrderStatus::Placed {
            oid,
            filled: Sz(m.filled),
            notional: m.notional,
            rested,
        }
    }

    fn place_alo(
        &mut self,
        b: u32,
        new: NewOrder,
        user: User,
        o: &OrderReq,
        now: u64,
    ) -> OrderStatus {
        let oid = new.oid;
        let book = &mut self.books[ix(b)];
        if book.crossing(o.side, o.px).is_some() {
            return OrderStatus::Rejected(RejectReason::AloWouldCross {
                bid: book.best(Side::Bid),
                ask: book.best(Side::Ask),
            });
        }
        let anchor = book.priority_anchor(
            o.side,
            o.px,
            o.priority,
            now,
            self.config.alo_priority_window_ms,
        );
        match book.rest(new, anchor) {
            Ok(slot) => {
                let before = anchor.map(|a| book.raw_node(a).oid);
                self.out.diffs.push(book.diff_of(
                    slot,
                    DiffKind::New {
                        sz: o.sz,
                        insert_before: before,
                    },
                ));
                self.index_rest(b, slot, user, o.cloid, oid);
                OrderStatus::Placed {
                    oid,
                    filled: Sz(0),
                    notional: 0,
                    rested: o.sz,
                }
            }
            Err(e) => OrderStatus::Rejected(book_reject(e)),
        }
    }

    fn index_rest(&mut self, b: u32, slot: u32, user: User, cloid: Option<Cloid>, oid: Oid) {
        self.oids.insert(oid, (b, slot));
        if let Some(c) = cloid {
            self.cloids.insert((user, c), oid);
        }
    }

    fn unindex(&mut self, oid: Oid, user: User) {
        if let Some((b, slot)) = self.oids.remove(&oid) {
            let n = self.books[ix(b)].raw_node(slot);
            if let Some(c) = n.cloid {
                if self.cloids.get(&(user, c)) == Some(&oid) {
                    self.cloids.remove(&(user, c));
                }
            }
        }
    }

    fn resolve(&self, user: User, asset: Option<Asset>, r: OrderRef) -> Option<(u32, u32)> {
        let oid = match r {
            OrderRef::Oid(o) => o,
            OrderRef::Cloid(c) => *self.cloids.get(&(user, c))?,
        };
        let (b, slot) = *self.oids.get(&oid)?;
        let book = &self.books[ix(b)];
        if asset.is_some_and(|a| a != book.asset()) {
            return None;
        }
        (book.node(slot)?.user == user).then_some((b, slot))
    }

    fn remove_resting(&mut self, b: u32, slot: u32) {
        self.touch(b);
        let book = &mut self.books[ix(b)];
        let d = book.diff_of(slot, DiffKind::Remove);
        if book.remove(slot).is_ok() {
            self.out.diffs.push(d);
            self.unindex(d.oid, d.user);
        }
    }

    fn cancel(&mut self, user: User, c: &CancelReq) -> OrderStatus {
        if !self.by_asset.contains_key(&c.asset) {
            return OrderStatus::Unsupported;
        }
        match self.resolve(user, Some(c.asset), c.target) {
            Some((b, slot)) => {
                self.remove_resting(b, slot);
                OrderStatus::Cancelled
            }
            None => OrderStatus::Rejected(RejectReason::UnknownOrder),
        }
    }

    fn modify(&mut self, user: User, m: &ModifyReq, now: u64) -> OrderStatus {
        if !self.by_asset.contains_key(&m.order.asset) {
            return OrderStatus::Unsupported;
        }
        match self.resolve(user, None, m.target) {
            Some((b, slot)) => {
                self.remove_resting(b, slot);
                self.place(user, &m.order, now)
            }
            None => OrderStatus::Rejected(RejectReason::ModifyUnknownOrder),
        }
    }

    /// Undo the last block's book changes and apply `native` (the block's
    /// native raw book diffs, in order) instead, so a replay can continue
    /// from native's book after a mismatch. The oid counter is set past the
    /// largest new oid in `native` (or restored to the block start).
    pub fn resync(&mut self, native: &[BookDiff]) -> Result<(), ResyncError> {
        let Self {
            books,
            touched,
            oids,
            cloids,
            ..
        } = self;
        for &b in touched.iter() {
            books[ix(b)].rollback(|r| match r {
                Restored::Unrested(n) => {
                    oids.remove(&n.oid);
                    if let Some(c) = n.cloid {
                        if cloids.get(&(n.user, c)) == Some(&n.oid) {
                            cloids.remove(&(n.user, c));
                        }
                    }
                }
                Restored::Back(n, slot) => {
                    oids.insert(n.oid, (b, slot));
                    if let Some(c) = n.cloid {
                        cloids.insert((n.user, c), n.oid);
                    }
                }
            });
        }
        self.next_oid = self.block_start_oid;
        for (i, d) in native.iter().enumerate() {
            let b = *self
                .by_asset
                .get(&d.asset)
                .ok_or(ResyncError::UnknownAsset(i, d.asset))?;
            self.touch(b);
            match d.kind {
                DiffKind::New { sz, insert_before } => {
                    if self.oids.contains_key(&d.oid) {
                        return Err(ResyncError::DuplicateOid(i, d.oid));
                    }
                    let anchor = match insert_before {
                        Some(a) => Some(
                            self.oids
                                .get(&a)
                                .filter(|&&(ab, _)| ab == b)
                                .map(|&(_, s)| s)
                                .ok_or(ResyncError::UnknownOid(i, a))?,
                        ),
                        None => None,
                    };
                    let slot = self.books[ix(b)]
                        .rest(
                            NewOrder {
                                oid: d.oid,
                                user: d.user,
                                cloid: d.cloid,
                                side: d.side,
                                px: d.px,
                                sz,
                                t_ms: self.out.block_time_ms,
                                prio: 0,
                            },
                            anchor,
                        )
                        .map_err(|e| ResyncError::Book(i, d.oid, e))?;
                    self.index_rest(b, slot, d.user, d.cloid, d.oid);
                    self.next_oid = self.next_oid.max(d.oid.0.saturating_add(1));
                }
                DiffKind::Update { new, .. } => {
                    let &(ob, slot) = self
                        .oids
                        .get(&d.oid)
                        .filter(|&&(ob, _)| ob == b)
                        .ok_or(ResyncError::UnknownOid(i, d.oid))?;
                    self.books[ix(ob)]
                        .resize(slot, new)
                        .map_err(|e| ResyncError::Book(i, d.oid, e))?;
                }
                DiffKind::Remove => {
                    let &(ob, slot) = self
                        .oids
                        .get(&d.oid)
                        .filter(|&&(ob, _)| ob == b)
                        .ok_or(ResyncError::UnknownOid(i, d.oid))?;
                    let n = *self.books[ix(ob)].raw_node(slot);
                    self.unindex(d.oid, n.user);
                    self.books[ix(ob)]
                        .remove(slot)
                        .map_err(|e| ResyncError::Book(i, d.oid, e))?;
                }
            }
        }
        Ok(())
    }

    /// Set the oid counter (e.g. from native's next response after a resync
    /// where native assigned oids to orders that did not rest).
    pub fn set_next_oid(&mut self, oid: Oid) {
        self.next_oid = oid.0;
    }

    /// Check every book's invariants and that the indexes match the books.
    #[must_use]
    pub fn check_invariants(&self) -> bool {
        if !self.books.iter().all(Book::check_invariants) {
            return false;
        }
        let live: usize = self.books.iter().map(Book::order_count).sum();
        if live != self.oids.len() {
            return false;
        }
        self.oids.iter().all(|(oid, &(b, slot))| {
            self.books
                .get(ix(b))
                .and_then(|bk| bk.node(slot))
                .is_some_and(|n| n.oid == *oid)
        })
    }
}

fn book_reject(e: BookError) -> RejectReason {
    match e {
        BookError::ZeroSize => RejectReason::ZeroSize,
        BookError::LevelOverflow => RejectReason::LevelOverflow,
        BookError::CapacityExhausted => RejectReason::CapacityExhausted,
        // Unreachable from `place`: no anchor mismatch (anchor found on the
        // same level), no crossing (checked / matched away), slot is live.
        BookError::Crossed
        | BookError::AnchorMismatch
        | BookError::UnknownSlot
        | BookError::OrigMismatch => RejectReason::DuplicateOid,
    }
}
