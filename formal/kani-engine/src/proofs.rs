//! The Kani harnesses. Bounds are on each harness; value domains are the
//! generators below. Run: `cargo kani -p kani-engine --harness <name>`.

use hl_exec::book::{Book, BookError, NewOrder, Taker};
use hl_exec::{
    BookDiff, CancelReq, Capacity, Cloid, DiffKind, Engine, EngineConfig, ModifyReq, Oid, OrderRef,
    OrderReq, OrderStatus, Px, RejectReason, RestingOrder, Side, Sz, Tif, User, UserAction,
};

use crate::model::{
    book_orders, book_sound, engine_view, fill_triples, index_consistent, l2_consistent, l2_view,
    reference_match, same_book_ignoring_prio, side_orders, ASSET, CLOIDS, USERS,
};

// ---------------------------------------------------------------- domains

fn side() -> Side {
    if kani::any() {
        Side::Bid
    } else {
        Side::Ask
    }
}

/// Price in 1..=3 (three possible levels per side).
fn px() -> Px {
    let p: u64 = kani::any();
    kani::assume((1..=3).contains(&p));
    Px(p)
}

/// Size in 1..=3.
fn sz() -> Sz {
    let s: u64 = kani::any();
    kani::assume((1..=3).contains(&s));
    Sz(s)
}

fn usr() -> User {
    if kani::any() {
        USERS[0]
    } else {
        USERS[1]
    }
}

fn cloid() -> Option<Cloid> {
    let k: u8 = kani::any();
    kani::assume(k < 3);
    match k {
        0 => None,
        1 => Some(CLOIDS[0]),
        _ => Some(CLOIDS[1]),
    }
}

/// Slot in 0..=3 (may name a free or never-used slot).
fn slot() -> u32 {
    let s: u32 = kani::any();
    kani::assume(s <= 3);
    s
}

fn new_order(oid: u64) -> NewOrder {
    NewOrder {
        oid: Oid(oid),
        user: usr(),
        cloid: None,
        side: side(),
        px: px(),
        sz: sz(),
        t_ms: 0,
        prio: 0,
    }
}

fn taker(oid: u64, stp: bool) -> Taker {
    Taker {
        oid: Oid(oid),
        user: usr(),
        side: side(),
        px: px(),
        sz: sz(),
        stp,
    }
}

/// A book holding up to `n` resting orders (oids 1..=n), each a successful
/// `rest` of an arbitrary order (2 sides x 3 prices x 3 sizes x 2 users).
fn book_with(n: u64) -> Book {
    let mut b = Book::with_capacity(ASSET, 4, 3);
    let k: u64 = kani::any();
    kani::assume(k <= n);
    for i in 1..=n {
        if i <= k {
            let r = b.rest(new_order(i), None);
            kani::assume(r.is_ok());
        }
    }
    b
}

/// One arbitrary book operation: rest (at the back or in front of an
/// arbitrary slot), remove, resize, or a taker (matched, then the remainder
/// rested as a GTC would). Errors are allowed: the book must stay sound.
fn book_op(b: &mut Book, oid: u64) {
    let mut diffs = Vec::new();
    let mut fills = Vec::new();
    let k: u8 = kani::any();
    kani::assume(k < 4);
    match k {
        0 => {
            let before = if kani::any() { Some(slot()) } else { None };
            let _ = b.rest(new_order(oid), before);
        }
        1 => {
            let _ = b.remove(slot());
        }
        2 => {
            let s: u64 = kani::any();
            kani::assume(s <= 3);
            let _ = b.resize(slot(), Sz(s));
        }
        _ => {
            let t = taker(oid, kani::any());
            let m = b.match_taker(t, &mut diffs, &mut fills);
            if m.remaining > 0 && kani::any() {
                let _ = b.rest(
                    NewOrder {
                        oid: t.oid,
                        user: t.user,
                        cloid: None,
                        side: t.side,
                        px: t.px,
                        sz: Sz(m.remaining),
                        t_ms: 0,
                        prio: 0,
                    },
                    None,
                );
            }
        }
    }
}

// ------------------------------------------------------ book-level proofs

/// (1) Bounds: <= 3 operations from an empty book (so <= 3 resting orders,
/// <= 3 levels per side), prices 1..=3, sizes 0..=3, slots 0..=3, 2 users.
/// After any sequence the book is not crossed, no resting order has size 0,
/// and each level's total equals the sum of its orders.
#[kani::proof]
#[kani::unwind(6)]
fn book_ops_keep_book_sound() {
    let mut b = Book::with_capacity(ASSET, 4, 3);
    for oid in 1..=3 {
        book_op(&mut b, oid);
    }
    assert!(book_sound(&b));
    assert!(b.check_invariants());
    kani::cover!(
        b.order_count() == 2
            && b.levels(Side::Bid).count() == 1
            && b.levels(Side::Ask).count() == 1,
        "two-sided book"
    );
    kani::cover!(
        b.levels(Side::Bid).count() == 1
            && b.order_count() == 2
            && b.levels(Side::Ask).count() == 0,
        "two orders on one level"
    );
}

/// (6) Same bounds as (1). The incremental L2 view (price, total, count per
/// level, best first) equals the one recomputed from the queues.
#[kani::proof]
#[kani::unwind(6)]
fn book_l2_incremental_equals_recomputed() {
    let mut b = Book::with_capacity(ASSET, 4, 3);
    for oid in 1..=3 {
        book_op(&mut b, oid);
    }
    assert!(l2_consistent(&b));
    kani::cover!(l2_view(&b).len() == 3, "three levels");
    kani::cover!(
        l2_view(&b).iter().any(|l| l.3 == 2),
        "a level with two orders"
    );
}

/// (2) Bounds: <= 3 resting orders (prices 1..=3, sizes 1..=3, 2 users),
/// one taker (size 1..=3), STP on or off. Quantity is conserved: filled +
/// remaining = taker size; the fills sum to `filled` and `notional` to sum
/// px*sz; each maker lost exactly what it filled; a resting order that left
/// without a fill is the taker's own under STP; the taker's side is
/// untouched; the opposite side's size dropped by filled + STP-removed size.
#[kani::proof]
#[kani::unwind(6)]
fn match_conserves_quantity() {
    let mut b = book_with(3);
    let t = taker(9, kani::any());
    let pre = book_orders(&b);
    let pre_same = side_orders(&b, t.side);
    let mut diffs = Vec::new();
    let mut fills = Vec::new();
    let m = b.match_taker(t, &mut diffs, &mut fills);
    let post = book_orders(&b);

    assert!(m.filled + m.remaining == t.sz.0);
    assert!(fills.iter().map(|f| f.sz.0).sum::<u64>() == m.filled);
    assert!(
        fills
            .iter()
            .map(|f| u128::from(f.px.0) * u128::from(f.sz.0))
            .sum::<u128>()
            == m.notional
    );
    assert!(fills
        .iter()
        .all(|f| f.sz.0 > 0 && f.taker == t.oid && f.taker_side == t.side));
    assert!(side_orders(&b, t.side) == pre_same);
    let mut stp_sz = 0u64;
    let mut stp_n = 0u32;
    for o in &pre {
        let after = post.iter().find(|p| p.oid == o.oid).map_or(0, |p| p.sz.0);
        let filled: u64 = fills
            .iter()
            .filter(|f| f.maker == o.oid)
            .map(|f| f.sz.0)
            .sum();
        if filled == 0 && after == 0 {
            // Left without a fill: only STP may do that.
            assert!(t.stp && o.user == t.user && o.side != t.side);
            stp_sz += o.sz.0;
            stp_n += 1;
        } else {
            assert!(o.sz.0 == after + filled);
        }
    }
    assert!(stp_n == m.stp_cancels);
    let opp = t.side.opposite();
    let before: u64 = pre.iter().filter(|o| o.side == opp).map(|o| o.sz.0).sum();
    let after: u64 = post.iter().filter(|o| o.side == opp).map(|o| o.sz.0).sum();
    assert!(before == after + m.filled + stp_sz);
    kani::cover!(
        m.stp_cancels > 0 && m.filled > 0,
        "STP removal and a fill in one match"
    );
    kani::cover!(
        fills.len() == 2 && m.remaining == 0,
        "two fills, taker used up"
    );
}

/// (3) Same bounds as (2). The fills are exactly the reference walk of the
/// opposite side: best price first, queue head first within a level, each at
/// the resting price; STP removes own orders in that same order.
#[kani::proof]
#[kani::unwind(6)]
fn match_price_time_priority() {
    let mut b = book_with(3);
    let t = taker(9, kani::any());
    let opp = side_orders(&b, t.side.opposite());
    let mut diffs = Vec::new();
    let mut fills = Vec::new();
    let m = b.match_taker(t, &mut diffs, &mut fills);
    let (want, want_stp, rem) = reference_match(&opp, t.side, t.px, t.sz.0, t.user, t.stp);
    assert!(fill_triples(&fills) == want);
    assert!(m.remaining == rem);
    let removed_stp: Vec<Oid> = diffs
        .iter()
        .filter(|d| d.kind == DiffKind::Remove && !fills.iter().any(|f| f.maker == d.oid))
        .map(|d| d.oid)
        .collect();
    assert!(removed_stp == want_stp);
    kani::cover!(
        fills.len() == 2 && fills[0].px != fills[1].px,
        "fills across two levels"
    );
    kani::cover!(
        fills.len() == 2 && fills[0].px == fills[1].px,
        "two fills in one queue"
    );
}

/// (5) Bounds: <= 2 rests, then one resize, one taker and one remove, with
/// every field unconstrained (any u64 price and size, any slot, any user
/// byte, STP on or off). No panic and no arithmetic overflow anywhere (Kani
/// checks every operation); the book stays sound.
#[kani::proof]
#[kani::unwind(5)]
fn book_no_panic_any_input() {
    let mut b = Book::with_capacity(ASSET, 4, 3);
    let any_order = |oid: u64| NewOrder {
        oid: Oid(oid),
        user: User([kani::any(); 20]),
        cloid: None,
        side: side(),
        px: Px(kani::any()),
        sz: Sz(kani::any()),
        t_ms: kani::any(),
        prio: kani::any(),
    };
    let mut rested = 0;
    for oid in 1..=2 {
        let before = if kani::any() { Some(kani::any()) } else { None };
        if b.rest(any_order(oid), before).is_ok() {
            rested += 1;
        }
    }
    let _ = b.resize(kani::any(), Sz(kani::any()));
    let mut diffs = Vec::new();
    let mut fills = Vec::new();
    let t = Taker {
        oid: Oid(3),
        user: User([kani::any(); 20]),
        side: side(),
        px: Px(kani::any()),
        sz: Sz(kani::any()),
        stp: kani::any(),
    };
    let m = b.match_taker(t, &mut diffs, &mut fills);
    let _ = b.remove(kani::any());
    assert!(b.check_invariants());
    kani::cover!(rested == 2 && m.filled > 0, "a fill on a two-order book");
    kani::cover!(m.notional > u128::from(u64::MAX), "notional above u64::MAX");
}

/// Book errors leave it unchanged. Bounds: <= 3 resting orders (as in (2)),
/// then one rest (any anchor slot 0..=3, size 0..=3 or u64::MAX), resize
/// (slot 0..=3, size 0..=3 or u64::MAX) or remove (slot 0..=3). When the call
/// returns an error, every resting order (all public fields, queue order)
/// and the L2 view are as before, and rollback still restores the pre-op book.
#[kani::proof]
#[kani::unwind(6)]
fn book_error_leaves_book_unchanged() {
    let mut b = book_with(3);
    b.begin();
    let pre = book_orders(&b);
    let pre_l2 = l2_view(&b);
    let any_sz = || {
        if kani::any() {
            Sz(u64::MAX)
        } else {
            let s: u64 = kani::any();
            kani::assume(s <= 3);
            Sz(s)
        }
    };
    let k: u8 = kani::any();
    kani::assume(k < 3);
    let err: Option<BookError> = match k {
        0 => {
            let mut o = new_order(9);
            o.sz = any_sz();
            let before = if kani::any() { Some(slot()) } else { None };
            b.rest(o, before).err()
        }
        1 => b.resize(slot(), any_sz()).err(),
        _ => b.remove(slot()).err(),
    };
    if let Some(e) = err {
        assert!(book_orders(&b) == pre);
        assert!(l2_view(&b) == pre_l2);
        assert!(b.check_invariants());
        kani::cover!(e == BookError::Crossed, "crossed rest refused");
        kani::cover!(e == BookError::LevelOverflow, "overflowing resize refused");
        kani::cover!(e == BookError::AnchorMismatch, "anchor mismatch refused");
        b.rollback(|_| {});
        assert!(book_orders(&b) == pre);
    }
}

/// begin/rollback restores the book exactly. Bounds: <= 2 resting orders,
/// then begin, then <= 2 arbitrary operations (as in (1)). After rollback
/// every slot 0..=3 holds the same node (all fields, including its private
/// queue links, via `Node: PartialEq`), and the queues and L2 are as before.
#[kani::proof]
#[kani::unwind(6)]
fn book_rollback_exact() {
    let mut b = book_with(2);
    b.begin();
    let slots = |b: &Book| {
        [
            b.node(0).copied(),
            b.node(1).copied(),
            b.node(2).copied(),
            b.node(3).copied(),
        ]
    };
    let pre_slots = slots(&b);
    let pre = book_orders(&b);
    let pre_l2 = l2_view(&b);
    book_op(&mut b, 5);
    book_op(&mut b, 6);
    let changed = book_orders(&b) != pre;
    b.rollback(|_| {});
    assert!(slots(&b) == pre_slots);
    assert!(book_orders(&b) == pre);
    assert!(l2_view(&b) == pre_l2);
    assert!(b.check_invariants());
    kani::cover!(changed && pre.len() == 2, "rollback of a real change");
}

// ---------------------------------------------------- engine-level proofs

fn cap() -> Capacity {
    Capacity {
        orders: 4,
        levels_per_side: 3,
        actions_per_block: 2,
        events_per_block: 8,
    }
}

/// An engine loaded from a snapshot of <= `n` resting orders (oids 1..=n,
/// fields from the small domains, cloid none/1/2). Next oid is 10.
fn engine_with(n: u64, stp: bool) -> Engine {
    let k: u64 = kani::any();
    kani::assume(k <= n);
    let mut v = Vec::new();
    for i in 1..=n {
        if i <= k {
            v.push(RestingOrder {
                asset: ASSET,
                user: usr(),
                oid: Oid(i),
                side: side(),
                px: px(),
                sz: sz(),
                cloid: cloid(),
                t_ms: 0,
            });
        }
    }
    // A snapshot never lists two resting orders of one user with one cloid.
    for (i, a) in v.iter().enumerate() {
        for b in &v[i + 1..] {
            kani::assume(a.cloid.is_none() || a.cloid != b.cloid || a.user != b.user);
        }
    }
    let cfg = EngineConfig {
        stp,
        alo_priority_window_ms: 400,
    };
    let e = Engine::from_l4_snapshot(&[ASSET], v, Some(Oid(10)), cap(), cfg);
    kani::assume(e.is_ok());
    e.unwrap_or_else(|_| unreachable!())
}

/// Order leg: GTC / ALO / IOC, prices 1..=3, sizes 1..=3, priority 0 or 1.
fn order_req() -> OrderReq {
    let k: u8 = kani::any();
    kani::assume(k < 3);
    OrderReq {
        asset: ASSET,
        side: side(),
        px: px(),
        sz: sz(),
        reduce_only: false,
        tif: [Tif::Gtc, Tif::Alo, Tif::Ioc][usize::from(k)],
        cloid: cloid(),
        priority: u64::from(kani::any::<bool>()),
        pre_reject: None,
        oid_hint: None,
    }
}

/// Target: oid 1, 2 (snapshot), 10, 11 (placed in the block) or cloid 1, 2.
fn order_ref() -> OrderRef {
    let k: u8 = kani::any();
    kani::assume(k < 6);
    match k {
        0 => OrderRef::Oid(Oid(1)),
        1 => OrderRef::Oid(Oid(2)),
        2 => OrderRef::Oid(Oid(10)),
        3 => OrderRef::Oid(Oid(11)),
        4 => OrderRef::Cloid(CLOIDS[0]),
        _ => OrderRef::Cloid(CLOIDS[1]),
    }
}

/// Storage for one single-leg action.
struct Leg {
    kind: u8,
    user: User,
    order: [OrderReq; 1],
    cancel: [CancelReq; 1],
    modify: [ModifyReq; 1],
}

fn leg() -> Leg {
    let kind: u8 = kani::any();
    kani::assume(kind < 3);
    let order = order_req();
    Leg {
        kind,
        user: usr(),
        order: [order],
        cancel: [CancelReq {
            asset: ASSET,
            target: order_ref(),
        }],
        modify: [ModifyReq {
            target: order_ref(),
            order,
        }],
    }
}

type Shape = ([OrderReq; 1], [CancelReq; 1], [ModifyReq; 1]);

fn pick(s: &Shape, k: u8, user: User) -> UserAction<'_> {
    match k {
        0 => UserAction::Order { user, orders: &s.0 },
        1 => UserAction::Cancel {
            user,
            cancels: &s.1,
        },
        2 => UserAction::Modify {
            user,
            modifies: &s.2,
        },
        3 => UserAction::Noop,
        _ => UserAction::Unsupported("x"),
    }
}

fn action(l: &Leg) -> UserAction<'_> {
    match l.kind {
        0 => UserAction::Order {
            user: l.user,
            orders: &l.order,
        },
        1 => UserAction::Cancel {
            user: l.user,
            cancels: &l.cancel,
        },
        _ => UserAction::Modify {
            user: l.user,
            modifies: &l.modify,
        },
    }
}

/// (7) index consistency. Bounds: snapshot of <= 2 orders, a block of <= 2
/// single-leg actions (order GTC/ALO/IOC, cancel or modify by oid or
/// cloid), STP on or off. Before and after the block: the oid index is a
/// bijection onto the resting orders and every (user, cloid) resolves to
/// exactly the resting order carrying it (none when none rests).
#[kani::proof]
#[kani::unwind(6)]
fn engine_index_consistent() {
    let mut e = engine_with(2, kani::any());
    assert!(index_consistent(&e));
    let (l0, l1) = (leg(), leg());
    let n: u8 = kani::any();
    kani::assume(n <= 2);
    let acts = [action(&l0), action(&l1)];
    e.execute_block(1, &acts[..usize::from(n)]);
    assert!(index_consistent(&e));
    kani::cover!(
        e.order_count() == 2 && engine_view(&e).cloids.iter().flatten().count() == 2,
        "two cloids live after the block"
    );
}

/// Rollback, priority harness. Bounds: snapshot of <= 2 orders (prices
/// 1..=3, sizes 1..=3, 2 users, cloid none/1/2), ONE single-leg action
/// (order GTC/ALO/IOC, cancel or modify, by oid 1/2/10/11 or cloid 1/2),
/// STP on or off. `resync(&[])` after `execute_block` gives back the
/// pre-block engine: resting orders (every public field, queue order), L2,
/// next oid, oid-index size, invariants, and cloid resolution for every
/// (user, cloid) of the domain.
#[kani::proof]
#[kani::unwind(5)]
#[kani::solver(cadical)]
fn engine_resync_empty_one_action() {
    let mut e = engine_with(2, kani::any());
    let pre = engine_view(&e);
    let l0 = leg();
    let changed = !e.execute_block(1, &[action(&l0)]).diffs.is_empty();
    let post_differs = engine_view(&e) != pre;
    assert!(e.resync(&[]).is_ok());
    assert!(engine_view(&e) == pre);
    assert!(e.check_invariants());
    kani::cover!(
        changed && post_differs,
        "block changed the book, then rolled back"
    );
    kani::cover!(
        changed && pre.orders.len() == 2 && pre.cloids.iter().flatten().count() >= 1,
        "rollback with a live cloid"
    );
}

/// (4)/(7) rollback exactness. Bounds as `engine_index_consistent`.
/// `resync(&[])` after `execute_block` gives back the pre-block engine:
/// resting orders (all public fields, queue order), L2, next oid, oid index
/// size and cloid resolution.
#[kani::proof]
#[kani::unwind(6)]
fn engine_resync_empty_restores_pre_block() {
    let mut e = engine_with(2, kani::any());
    let pre = engine_view(&e);
    let (l0, l1) = (leg(), leg());
    let n: u8 = kani::any();
    kani::assume(n <= 2);
    let acts = [action(&l0), action(&l1)];
    let changed = !e.execute_block(1, &acts[..usize::from(n)]).diffs.is_empty();
    let r = e.resync(&[]);
    assert!(r.is_ok());
    assert!(engine_view(&e) == pre);
    assert!(e.check_invariants());
    kani::cover!(changed && n == 2, "two-action block with diffs rolled back");
}

/// (4) resync to the block's own diffs. Bounds as `engine_index_consistent`.
/// For engines E1, E2 equal before the block: E1 runs the block; E2 runs it
/// and then `resync(E1's diffs)`. Both end in the same state (orders with
/// every public field except `prio`, which native diffs do not carry; L2;
/// next oid; indexes), i.e. the book after the block is the book the diffs
/// describe.
#[kani::proof]
#[kani::unwind(6)]
fn engine_resync_own_diffs_reproduces_block() {
    let mut e1 = engine_with(2, kani::any());
    let mut e2 = e1.clone();
    let (l0, l1) = (leg(), leg());
    let n: u8 = kani::any();
    kani::assume(n <= 2);
    let acts = [action(&l0), action(&l1)];
    let diffs: Vec<BookDiff> = e1.execute_block(1, &acts[..usize::from(n)]).diffs.clone();
    e2.execute_block(1, &acts[..usize::from(n)]);
    let r = e2.resync(&diffs);
    assert!(r.is_ok());
    // Documented protocol: oids taken by legs that rested nowhere are not in
    // the diffs; the driver sets the counter from native's next response.
    e2.set_next_oid(e1.next_oid());
    assert!(same_book_ignoring_prio(
        &engine_view(&e1),
        &engine_view(&e2)
    ));
    kani::cover!(diffs.len() >= 3, "three or more diffs replayed");
}

/// (5) engine level. Bounds: snapshot of <= 2 orders, one block of <= 2
/// actions, each any of: order leg with every field unconstrained (any asset,
/// any u64 price and size, any TIF incl. Other, reduce-only, any pre-reject,
/// any oid hint, any priority), cancel or modify with any target, noop,
/// unsupported. No panic and no overflow; the engine's invariants hold and
/// resync(&[]) succeeds afterwards.
#[kani::proof]
#[kani::unwind(6)]
fn engine_no_panic_any_action() {
    let mut e = engine_with(2, kani::any());
    let any_req = || {
        let t: u8 = kani::any();
        kani::assume(t < 4);
        OrderReq {
            asset: kani::any(),
            side: side(),
            px: Px(kani::any()),
            sz: Sz(kani::any()),
            reduce_only: kani::any(),
            tif: [Tif::Gtc, Tif::Alo, Tif::Ioc, Tif::Other][usize::from(t)],
            cloid: cloid(),
            priority: kani::any(),
            pre_reject: if kani::any() {
                Some(RejectReason::TickSize)
            } else {
                None
            },
            oid_hint: if kani::any() {
                Some(Oid(kani::any()))
            } else {
                None
            },
        }
    };
    let any_ref = || {
        if kani::any() {
            OrderRef::Oid(Oid(kani::any()))
        } else {
            OrderRef::Cloid(Cloid(kani::any()))
        }
    };
    let mk = || {
        let o = any_req();
        (
            [o],
            [CancelReq {
                asset: kani::any(),
                target: any_ref(),
            }],
            [ModifyReq {
                target: any_ref(),
                order: any_req(),
            }],
        )
    };
    let (a, b) = (mk(), mk());
    let (k0, k1): (u8, u8) = (kani::any(), kani::any());
    let acts = [pick(&a, k0, usr()), pick(&b, k1, usr())];
    let out = e.execute_block(kani::any(), &acts);
    let placed = out
        .statuses
        .iter()
        .any(|s| matches!(s, OrderStatus::Placed { .. }));
    assert!(e.check_invariants());
    assert!(e.books().all(book_sound));
    kani::cover!(placed, "an order placed");
    assert!(e.resync(&[]).is_ok());
}

/// F3 (a). Bounds: snapshot <= 2 orders, ONE order leg (domain of
/// `order_req`) with an arbitrary `oid_hint` (None or any u64), STP on/off.
/// The outcome (apart from `oid_mismatches`) and the resulting state equal
/// the no-hint run; with the hint None or equal to the counter,
/// `oid_mismatches == 0`.
#[kani::proof]
#[kani::unwind(5)]
fn engine_oid_hint_does_not_influence() {
    let base = engine_with(2, kani::any());
    let o = order_req();
    let h: Option<Oid> = if kani::any() {
        Some(Oid(kani::any()))
    } else {
        None
    };
    let mut plain = base.clone();
    plain.execute_block(
        1,
        &[UserAction::Order {
            user: USERS[0],
            orders: &[o],
        }],
    );
    let mut hinted = base.clone();
    let oh = [OrderReq { oid_hint: h, ..o }];
    hinted.execute_block(
        1,
        &[UserAction::Order {
            user: USERS[0],
            orders: &oh,
        }],
    );
    let mut a = hinted.outcome().clone();
    let m = a.oid_mismatches;
    a.oid_mismatches = 0;
    assert!(a == *plain.outcome());
    assert!(engine_view(&hinted) == engine_view(&plain));
    if h.is_none() || h == Some(base.next_oid()) {
        assert!(m == 0);
    }
    kani::cover!(h.is_some() && m == 0, "correct hint");
}

/// F3 (b), refined. Same bounds. A leg that takes an oid (counter moved)
/// with a hint different from the counter gives `oid_mismatches == 1`.
#[kani::proof]
#[kani::unwind(5)]
fn engine_oid_hint_mismatch_counted() {
    let mut e = engine_with(2, kani::any());
    let c = e.next_oid();
    let h = Oid(kani::any());
    let o = [OrderReq {
        oid_hint: Some(h),
        ..order_req()
    }];
    e.execute_block(
        1,
        &[UserAction::Order {
            user: USERS[0],
            orders: &o,
        }],
    );
    let took = e.next_oid() != c;
    if took && h != c {
        assert!(e.outcome().oid_mismatches == 1);
    }
    kani::cover!(took && h != c, "counted mismatch");
}
