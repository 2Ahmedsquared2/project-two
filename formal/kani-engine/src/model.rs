//! Observations of a book or engine made through the public API, the
//! properties checked on them, and a reference matcher.

use hl_exec::book::{Book, Node};
use hl_exec::{
    Asset, CancelReq, Cloid, Engine, Fill, Oid, OrderRef, OrderStatus, Px, Side, Sz, User,
    UserAction,
};

/// The one asset the engine harnesses use.
pub const ASSET: Asset = 0;
/// Users the harnesses pick from.
pub const USERS: [User; 2] = [user(1), user(2)];
/// Client order ids the harnesses pick from.
pub const CLOIDS: [Cloid; 2] = [Cloid(1), Cloid(2)];

/// A user whose address starts with byte `i`.
#[must_use]
pub const fn user(i: u8) -> User {
    let mut b = [0u8; 20];
    b[0] = i;
    User(b)
}

/// The public fields of a resting order, in book order. `prio` is kept
/// apart because native diffs do not carry it (see [`same_book_ignoring_prio`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resting {
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
    /// Block time it came to rest.
    pub t_ms: u64,
    /// Priority fee rate.
    pub prio: u64,
}

impl From<&Node> for Resting {
    fn from(n: &Node) -> Self {
        Self {
            oid: n.oid,
            user: n.user,
            cloid: n.cloid,
            side: n.side,
            px: n.px,
            sz: n.sz,
            t_ms: n.t_ms,
            prio: n.prio,
        }
    }
}

/// Every resting order of a side, best level first, front of queue first.
#[must_use]
pub fn side_orders(b: &Book, side: Side) -> Vec<Resting> {
    let mut v = Vec::new();
    for px in b.levels(side) {
        for n in b.queue(side, px) {
            v.push(Resting::from(n));
        }
    }
    v
}

/// Bids then asks, each best first.
#[must_use]
pub fn book_orders(b: &Book) -> Vec<Resting> {
    let mut v = side_orders(b, Side::Bid);
    v.extend(side_orders(b, Side::Ask));
    v
}

/// The incremental L2 view of both sides.
#[must_use]
pub fn l2_view(b: &Book) -> Vec<(Side, Px, Sz, u32)> {
    let mut v = Vec::new();
    for side in [Side::Bid, Side::Ask] {
        for (px, sz, n) in b.l2(side) {
            v.push((side, px, sz, n));
        }
    }
    v
}

/// L2 recomputed from the queues: per level, the sum and number of its orders.
#[must_use]
pub fn l2_recomputed(b: &Book) -> Option<Vec<(Side, Px, Sz, u32)>> {
    let mut v = Vec::new();
    for side in [Side::Bid, Side::Ask] {
        for px in b.levels(side) {
            let mut sum = 0u64;
            let mut cnt = 0u32;
            for n in b.queue(side, px) {
                sum = sum.checked_add(n.sz.0)?;
                cnt = cnt.checked_add(1)?;
            }
            v.push((side, px, Sz(sum), cnt));
        }
    }
    Some(v)
}

/// Property (1): not crossed, no resting order of size zero, every level's
/// total equals the sum of its orders, every listed level is non-empty and
/// holds only orders of its own side and price.
#[must_use]
pub fn book_sound(b: &Book) -> bool {
    if let (Some(bid), Some(ask)) = (b.best(Side::Bid), b.best(Side::Ask)) {
        if bid >= ask {
            return false;
        }
    }
    let mut live = 0usize;
    for side in [Side::Bid, Side::Ask] {
        for px in b.levels(side) {
            let mut sum = 0u64;
            let mut cnt = 0usize;
            for n in b.queue(side, px) {
                if n.sz.0 == 0 || n.side != side || n.px != px {
                    return false;
                }
                let Some(s) = sum.checked_add(n.sz.0) else {
                    return false;
                };
                sum = s;
                cnt += 1;
            }
            if cnt == 0 || b.level_total(side, px) != sum {
                return false;
            }
            live += cnt;
        }
    }
    live == b.order_count()
}

/// Property (6): the incremental L2 view equals the one recomputed from the
/// queues, level by level (price, total, count), best first, and each side's
/// prices are strictly best-first (bids descending, asks ascending).
#[must_use]
pub fn l2_consistent(b: &Book) -> bool {
    let inc = l2_view(b);
    if Some(&inc) != l2_recomputed(b).as_ref() {
        return false;
    }
    inc.windows(2).all(|w| {
        w[0].0 != w[1].0
            || match w[0].0 {
                Side::Bid => w[0].1 > w[1].1,
                Side::Ask => w[0].1 < w[1].1,
            }
    })
}

/// What a taker should do against `opp` (the opposite side's orders, best
/// first, FIFO), per the documented rule: best price first, queue head first,
/// each fill at the resting price, stop when the price no longer crosses or
/// the size is used up; with self-trade prevention a resting order of the
/// taker's own user is removed instead of filled. Returns the fills as
/// `(maker oid, px, sz)`, the oids removed by STP and the remaining size.
#[must_use]
pub fn reference_match(
    opp: &[Resting],
    side: Side,
    px: Px,
    sz: u64,
    user: User,
    stp: bool,
) -> (Vec<(Oid, Px, Sz)>, Vec<Oid>, u64) {
    let mut fills = Vec::new();
    let mut stp_removed = Vec::new();
    let mut rem = sz;
    for o in opp {
        if rem == 0 {
            break;
        }
        let crosses = match side {
            Side::Bid => px >= o.px,
            Side::Ask => px <= o.px,
        };
        if !crosses {
            break;
        }
        if stp && o.user == user {
            stp_removed.push(o.oid);
            continue;
        }
        let take = rem.min(o.sz.0);
        fills.push((o.oid, o.px, Sz(take)));
        rem -= take;
    }
    (fills, stp_removed, rem)
}

/// The fills as `(maker oid, px, sz)`.
#[must_use]
pub fn fill_triples(fills: &[Fill]) -> Vec<(Oid, Px, Sz)> {
    fills.iter().map(|f| (f.maker, f.px, f.sz)).collect()
}

/// Every resting order of the engine's books, in book order.
#[must_use]
pub fn engine_orders(e: &Engine) -> Vec<Resting> {
    let mut v = Vec::new();
    for b in e.books() {
        v.extend(book_orders(b));
    }
    v
}

/// The engine's L2 view (all books).
#[must_use]
pub fn engine_l2(e: &Engine) -> Vec<(Side, Px, Sz, u32)> {
    let mut v = Vec::new();
    for b in e.books() {
        v.extend(l2_view(b));
    }
    v
}

/// Which resting oid `cancelByCloid(cloid)` from `user` resolves to, found
/// by running that cancel on a clone (the cloid index is private).
#[must_use]
pub fn cloid_resolves(e: &Engine, user: User, cloid: Cloid) -> Option<Oid> {
    let mut c = e.clone();
    let legs = [CancelReq {
        asset: ASSET,
        target: OrderRef::Cloid(cloid),
    }];
    let out = c.execute_block(
        0,
        &[UserAction::Cancel {
            user,
            cancels: &legs,
        }],
    );
    match out.statuses.first() {
        Some(OrderStatus::Cancelled) => out.diffs.first().map(|d| d.oid),
        _ => None,
    }
}

/// `cloid_resolves` for every (user, cloid) pair of the harness domain.
#[must_use]
pub fn cloid_index(e: &Engine) -> [Option<Oid>; 4] {
    [
        cloid_resolves(e, USERS[0], CLOIDS[0]),
        cloid_resolves(e, USERS[0], CLOIDS[1]),
        cloid_resolves(e, USERS[1], CLOIDS[0]),
        cloid_resolves(e, USERS[1], CLOIDS[1]),
    ]
}

/// Property (7), index half: the oid index is a bijection onto the resting
/// orders (`Engine::check_invariants`: one entry per live order, each naming
/// a live slot holding that oid), and for every (user, cloid) of the domain
/// `cancelByCloid` resolves to exactly the resting order with that user and
/// cloid, or to nothing when none rests.
#[must_use]
pub fn index_consistent(e: &Engine) -> bool {
    if !e.check_invariants() {
        return false;
    }
    let orders = engine_orders(e);
    if orders.len() != e.order_count() {
        return false;
    }
    let idx = cloid_index(e);
    let mut k = 0;
    for u in USERS {
        for c in CLOIDS {
            let mut it = orders
                .iter()
                .filter(|o| o.user == u && o.cloid == Some(c))
                .map(|o| o.oid);
            let want = it.next();
            // Two live orders with the same (user, cloid): one of them is
            // necessarily unreachable through cancelByCloid.
            if it.next().is_some() || idx[k] != want {
                return false;
            }
            k += 1;
        }
    }
    true
}

/// Everything observable about an engine's state through the public API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineView {
    /// Resting orders, book order.
    pub orders: Vec<Resting>,
    /// L2 view.
    pub l2: Vec<(Side, Px, Sz, u32)>,
    /// Next oid.
    pub next_oid: Oid,
    /// Size of the oid index.
    pub indexed: usize,
    /// cancelByCloid resolution for the domain.
    pub cloids: [Option<Oid>; 4],
}

/// Observe an engine.
#[must_use]
pub fn engine_view(e: &Engine) -> EngineView {
    EngineView {
        orders: engine_orders(e),
        l2: engine_l2(e),
        next_oid: e.next_oid(),
        indexed: e.order_count(),
        cloids: cloid_index(e),
    }
}

/// The same view with every order's `prio` cleared (native diffs do not
/// carry it, so `resync` rests with priority 0).
#[must_use]
pub fn same_book_ignoring_prio(a: &EngineView, b: &EngineView) -> bool {
    let strip = |v: &EngineView| {
        let mut v = v.clone();
        for o in &mut v.orders {
            o.prio = 0;
        }
        v
    };
    strip(a) == strip(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_exec::{Capacity, EngineConfig, OrderReq, RestingOrder, Tif};

    fn cap() -> Capacity {
        Capacity {
            orders: 8,
            levels_per_side: 4,
            actions_per_block: 4,
            events_per_block: 16,
        }
    }

    fn req(side: Side, px: u64, sz: u64, cloid: Option<Cloid>) -> OrderReq {
        OrderReq {
            asset: ASSET,
            side,
            px: Px(px),
            sz: Sz(sz),
            reduce_only: false,
            tif: Tif::Gtc,
            cloid,
            priority: 0,
            pre_reject: None,
            oid_hint: None,
        }
    }

    #[test]
    fn reference_match_fifo_and_stp() {
        let r = |oid, u, px, sz| Resting {
            oid: Oid(oid),
            user: user(u),
            cloid: None,
            side: Side::Ask,
            px: Px(px),
            sz: Sz(sz),
            t_ms: 0,
            prio: 0,
        };
        let opp = [r(1, 1, 2, 1), r(2, 2, 2, 2), r(3, 2, 3, 5)];
        let (f, s, rem) = reference_match(&opp, Side::Bid, Px(3), 4, user(1), true);
        assert_eq!(f, vec![(Oid(2), Px(2), Sz(2)), (Oid(3), Px(3), Sz(2))]);
        assert_eq!(s, vec![Oid(1)]);
        assert_eq!(rem, 0);
    }

    #[test]
    fn views_on_a_concrete_block() {
        let snap = [RestingOrder {
            asset: ASSET,
            user: USERS[0],
            oid: Oid(1),
            side: Side::Ask,
            px: Px(3),
            sz: Sz(2),
            cloid: Some(CLOIDS[0]),
            t_ms: 0,
        }];
        let mut e = Engine::from_l4_snapshot(
            &[ASSET],
            snap,
            Some(Oid(10)),
            cap(),
            EngineConfig::default(),
        )
        .unwrap();
        assert!(index_consistent(&e));
        let pre = engine_view(&e);
        let legs = [req(Side::Bid, 3, 1, Some(CLOIDS[1]))];
        e.execute_block(
            5,
            &[UserAction::Order {
                user: USERS[1],
                orders: &legs,
            }],
        );
        assert!(index_consistent(&e));
        assert!(e.books().all(book_sound) && e.books().all(l2_consistent));
        e.resync(&[]).unwrap();
        assert_eq!(engine_view(&e), pre);
    }
}
