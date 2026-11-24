//! DESCOPED Kani harnesses: kept as source, not run by the gate, not proofs.
//!
//! These are the minimal-bound harnesses (at most 2 resting orders, prices
//! 1..=2, sizes 1..=2, one operation or one action, one kind of journal
//! entry per rollback harness, sides fixed per instance) that hit the 8 GiB
//! cap or a 15-minute timeout without a counterexample. They are descoped
//! as a Kani/CBMC memory or time limit; `README.md` lists each one, its
//! measured steps and peak memory, and the exhaustive test (evidence, not
//! proof) that stands in for it. Kept so #75 (growth-free harnesses) can
//! rerun them. Correctness only, never speed.
//!
//! Compiled only with `cfg(kani)` AND the `descoped` feature, so a plain
//! `cargo kani -p kani-engine` runs only the verified harness in `proofs`.
//! Run one (8 GiB cap, one at a time):
//! `cargo kani -p kani-engine --features descoped --harness descoped::<name> --exact`.

use hl_exec::book::{Book, BookError, NewOrder, Taker};
use hl_exec::{
    BookDiff, CancelReq, Capacity, Cloid, DiffKind, Engine, EngineConfig, ModifyReq, Oid, OrderRef,
    OrderReq, OrderStatus, Px, RejectReason, RestingOrder, Side, Sz, Tif, User, UserAction,
};

use crate::model::{
    book_orders, book_sound, cloid_resolves, engine_l2, engine_orders, fill_triples, l2_consistent,
    l2_view, reference_match, side_orders, Resting, ASSET, CLOIDS, USERS,
};

// ---------------------------------------------------------------- domains

/// Generates one harness per side combination: `body(Side::X, ...)`.
/// Sides are fixed per instance because a symbolic side makes
/// `Book::ladder_mut(side)` a pointer to either ladder `Vec`, and CBMC runs
/// out of memory on that even for one `rest` (see README, "Kani limits").
/// The instances of a property together cover every side combination.
macro_rules! per_sides {
    ($unwind:literal, $body:ident { $($name:ident: [$($s:ident),*]),* $(,)? }) => {
        $(
            #[kani::proof]
            #[kani::unwind($unwind)]
            fn $name() {
                $body($(Side::$s),*)
            }
        )*
    };
}

/// Price in 1..=2.
fn px() -> Px {
    if kani::any() {
        Px(1)
    } else {
        Px(2)
    }
}

/// Size in 1..=2.
fn sz() -> Sz {
    if kani::any() {
        Sz(1)
    } else {
        Sz(2)
    }
}

fn usr() -> User {
    if kani::any() {
        USERS[0]
    } else {
        USERS[1]
    }
}

/// Cloid none or 1.
fn cloid() -> Option<Cloid> {
    if kani::any() {
        None
    } else {
        Some(CLOIDS[0])
    }
}

/// Slot 0 or 1. `book_with` never removes, so before the one operation a
/// slot is live or never used, never free.
fn slot() -> u32 {
    u32::from(kani::any::<bool>())
}

fn new_order(oid: u64, side: Side) -> NewOrder {
    NewOrder {
        oid: Oid(oid),
        user: usr(),
        cloid: None,
        side,
        px: px(),
        sz: sz(),
        t_ms: 0,
        prio: 0,
    }
}

fn taker(oid: u64, side: Side, stp: bool) -> Taker {
    Taker {
        oid: Oid(oid),
        user: usr(),
        side,
        px: px(),
        sz: sz(),
        stp,
    }
}

/// A book (preallocated for 4 orders/journal entries, 2 levels per side, so
/// no Vec grows inside a harness) holding up to `n` resting
/// orders: order i (oid i+1) on `sides[i]`, the first k of them for any
/// k, each a successful `rest` of an arbitrary order on that side.
fn book_with(sides: &[Side]) -> Book {
    let mut b = Book::with_capacity(ASSET, 4, 2);
    let k: usize = kani::any();
    kani::assume(k <= sides.len());
    for (i, &sd) in sides.iter().enumerate() {
        if i < k {
            let r = b.rest(new_order(i as u64 + 1, sd), None);
            kani::assume(r.is_ok());
        }
    }
    b
}

/// One arbitrary book operation: rest (at the back or in front of slot 0/1),
/// remove, resize (size 0..=2), or a taker (matched; a remainder is not
/// rested, which is the same `rest` as the first branch). Errors are allowed: the book must stay sound.
fn book_op(b: &mut Book, oid: u64, side: Side) {
    let mut diffs = Vec::with_capacity(4);
    let mut fills = Vec::with_capacity(4);
    let k: u8 = kani::any();
    kani::assume(k < 4);
    match k {
        0 => {
            let before = if kani::any() { Some(slot()) } else { None };
            let _ = b.rest(new_order(oid, side), before);
        }
        1 => {
            let _ = b.remove(slot());
        }
        2 => {
            let s: u64 = kani::any();
            kani::assume(s <= 2);
            let _ = b.resize(slot(), Sz(s));
        }
        _ => {
            let _ = b.match_taker(taker(oid, side, kani::any()), &mut diffs, &mut fills);
        }
    }
}

/// Every slot of a 2-order book, whole node (private queue links included,
/// via `Node: PartialEq`).
fn slots(b: &Book) -> [Option<hl_exec::book::Node>; 2] {
    [b.node(0).copied(), b.node(1).copied()]
}

/// Allocation-free L2 observation for prices 1..=2: best bid/ask and each
/// side's level total at price 1 and 2.
fn l2_fixed(b: &Book) -> (Option<Px>, Option<Px>, [u64; 4]) {
    (
        b.best(Side::Bid),
        b.best(Side::Ask),
        [
            b.level_total(Side::Bid, Px(1)),
            b.level_total(Side::Bid, Px(2)),
            b.level_total(Side::Ask, Px(1)),
            b.level_total(Side::Ask, Px(2)),
        ],
    )
}

// ------------------------------------------------------ book-level proofs

/// (1) Bounds: <= 1 resting order on side `s0`, then ONE operation
/// (`book_op`, rest/taker on side `s1`), so <= 2 orders; prices 1..=2
/// (<= 2 levels), sizes 1..=2 (resize 0..=2), slots 0..=1, 2 users. The
/// book is not crossed, no resting order has size 0, and each level's total
/// equals the sum of its orders.
fn book_sound_body(s0: Side, s1: Side) {
    let mut b = book_with(&[s0]);
    book_op(&mut b, 2, s1);
    assert!(book_sound(&b));
    kani::cover!(b.order_count() == 2, "two orders");
    kani::cover!(
        b.levels(Side::Bid).count() + b.levels(Side::Ask).count() == 2,
        "two levels"
    );
}
per_sides!(
    3,
    book_sound_body {
        book_ops_keep_book_sound_bb: [Bid, Bid],
        book_ops_keep_book_sound_ba: [Bid, Ask],
        book_ops_keep_book_sound_ab: [Ask, Bid],
        book_ops_keep_book_sound_aa: [Ask, Ask],
    }
);

/// (1), remove only. Bounds: <= 1 resting order on side `s0` (price 1..=2,
/// size 1..=2, 2 users), then ONE `remove` at slot 0 or 1 (live or never
/// used). The book stays sound (not crossed, no zero size, level
/// total = sum of its orders, every listed level non-empty).
fn remove_sound_body(s0: Side) {
    let mut b = book_with(&[s0]);
    let n = b.order_count();
    let ok = b.remove(slot()).is_ok();
    assert!(book_sound(&b));
    kani::cover!(ok && n == 1, "the live order removed");
    kani::cover!(!ok && n == 1, "a never-used slot refused");
}
per_sides!(
    2,
    remove_sound_body {
        book_remove_keeps_book_sound_b: [Bid],
        book_remove_keeps_book_sound_a: [Ask],
    }
);

/// (6) Same bounds as (1). The incremental L2 view (price, total, count per
/// level, best first) equals the one recomputed from the queues.
fn l2_body(s0: Side, s1: Side) {
    let mut b = book_with(&[s0]);
    book_op(&mut b, 2, s1);
    assert!(l2_consistent(&b));
    kani::cover!(l2_view(&b).len() == 2, "two levels");
    kani::cover!(
        l2_view(&b).iter().any(|l| l.3 == 2),
        "a level with two orders"
    );
}
per_sides!(
    3,
    l2_body {
        book_l2_incremental_equals_recomputed_bb: [Bid, Bid],
        book_l2_incremental_equals_recomputed_ba: [Bid, Ask],
        book_l2_incremental_equals_recomputed_ab: [Ask, Bid],
        book_l2_incremental_equals_recomputed_aa: [Ask, Ask],
    }
);

/// (2) Bounds: <= 2 resting orders on sides `s0`, `s1` (prices 1..=2, sizes
/// 1..=2, 2 users), ONE taker on side `st` (size 1..=2), STP on or off.
/// Quantity is conserved: filled + remaining = taker size; fills sum to
/// `filled` and `notional` to sum px*sz; each maker lost exactly what it
/// filled; an order that left without a fill is the taker's own under STP;
/// the taker's side is untouched; the opposite side dropped by filled +
/// STP-removed size.
fn conserve_body(s0: Side, s1: Side, st: Side) {
    let mut b = book_with(&[s0, s1]);
    let t = taker(9, st, kani::any());
    let pre = book_orders(&b);
    let pre_same = side_orders(&b, t.side);
    let mut diffs = Vec::with_capacity(4);
    let mut fills = Vec::with_capacity(4);
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
    kani::cover!(m.filled > 0, "a fill");
    kani::cover!(
        m.stp_cancels > 0 && m.filled > 0,
        "STP removal and a fill in one match"
    );
    kani::cover!(
        fills.len() == 2 && m.remaining == 0,
        "two fills, taker used up"
    );
}
per_sides!(
    3,
    conserve_body {
        match_conserves_quantity_bbb: [Bid, Bid, Bid],
        match_conserves_quantity_bba: [Bid, Bid, Ask],
        match_conserves_quantity_bab: [Bid, Ask, Bid],
        match_conserves_quantity_baa: [Bid, Ask, Ask],
        match_conserves_quantity_abb: [Ask, Bid, Bid],
        match_conserves_quantity_aba: [Ask, Bid, Ask],
        match_conserves_quantity_aab: [Ask, Ask, Bid],
        match_conserves_quantity_aaa: [Ask, Ask, Ask],
    }
);

/// (3) Same bounds as (2). The fills are exactly the reference walk of the
/// opposite side: best price first, queue head first within a level, each at
/// the resting price; STP removes own orders in that same order.
fn priority_body(s0: Side, s1: Side, st: Side) {
    let mut b = book_with(&[s0, s1]);
    let t = taker(9, st, kani::any());
    let opp = side_orders(&b, t.side.opposite());
    let mut diffs = Vec::with_capacity(4);
    let mut fills = Vec::with_capacity(4);
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
    kani::cover!(!fills.is_empty(), "a fill");
    kani::cover!(
        fills.len() == 2 && fills[0].px != fills[1].px,
        "fills across two levels"
    );
    kani::cover!(
        fills.len() == 2 && fills[0].px == fills[1].px,
        "two fills in one queue"
    );
}
per_sides!(
    3,
    priority_body {
        match_price_time_priority_bbb: [Bid, Bid, Bid],
        match_price_time_priority_bba: [Bid, Bid, Ask],
        match_price_time_priority_bab: [Bid, Ask, Bid],
        match_price_time_priority_baa: [Bid, Ask, Ask],
        match_price_time_priority_abb: [Ask, Bid, Bid],
        match_price_time_priority_aba: [Ask, Bid, Ask],
        match_price_time_priority_aab: [Ask, Ask, Bid],
        match_price_time_priority_aaa: [Ask, Ask, Ask],
    }
);

/// (5) Bounds: ONE rest on side `s0`, then ONE operation (rest on side `s1`
/// in front of any slot, resize, taker on side `s1`, or remove), every other
/// field unconstrained (any u64 price, size, slot, time, priority; user
/// byte; STP on or off); <= 2 orders. No panic and no arithmetic overflow
/// (Kani checks every operation); the book's invariants hold.
fn no_panic_body(s0: Side, s1: Side) {
    let mut b = Book::with_capacity(ASSET, 4, 2);
    let any_order = |oid: u64, side: Side| NewOrder {
        oid: Oid(oid),
        user: User([kani::any(); 20]),
        cloid: None,
        side,
        px: Px(kani::any()),
        sz: Sz(kani::any()),
        t_ms: kani::any(),
        prio: kani::any(),
    };
    let rested = b.rest(any_order(1, s0), None).is_ok();
    let k: u8 = kani::any();
    let mut filled = 0u64;
    match k {
        0 => {
            let before = if kani::any() { Some(kani::any()) } else { None };
            let _ = b.rest(any_order(2, s1), before);
        }
        1 => {
            let _ = b.resize(kani::any(), Sz(kani::any()));
        }
        2 => {
            let mut diffs = Vec::with_capacity(4);
            let mut fills = Vec::with_capacity(4);
            let t = Taker {
                oid: Oid(2),
                user: User([kani::any(); 20]),
                side: s1,
                px: Px(kani::any()),
                sz: Sz(kani::any()),
                stp: kani::any(),
            };
            filled = b.match_taker(t, &mut diffs, &mut fills).filled;
        }
        _ => {
            let _ = b.remove(kani::any());
        }
    }
    assert!(b.check_invariants());
    kani::cover!(b.order_count() == 2, "two orders");
    kani::cover!(rested && filled > 0, "a fill");
}
per_sides!(
    3,
    no_panic_body {
        book_no_panic_any_input_bb: [Bid, Bid],
        book_no_panic_any_input_ba: [Bid, Ask],
        book_no_panic_any_input_ab: [Ask, Bid],
        book_no_panic_any_input_aa: [Ask, Ask],
    }
);

/// Book errors leave it unchanged. Bounds: <= 1 resting order on side `s0`,
/// then ONE rest on side `s1` (anchor none or slot
/// 0..=1, size 0..=2 or u64::MAX), resize (slot 0..=1, size 0..=2 or
/// u64::MAX) or remove (slot 0..=1). When the call returns an error, every
/// slot (whole node) and the L2 view are as before.
fn error_body(s0: Side, s1: Side) {
    let mut b = book_with(&[s0]);
    let pre = slots(&b);
    let pre_l2 = l2_fixed(&b);
    let any_sz = || {
        if kani::any() {
            Sz(u64::MAX)
        } else {
            let s: u64 = kani::any();
            kani::assume(s <= 2);
            Sz(s)
        }
    };
    let k: u8 = kani::any();
    let err: Option<BookError> = match k {
        0 => {
            let mut o = new_order(9, s1);
            o.sz = any_sz();
            let before = if kani::any() { Some(slot()) } else { None };
            b.rest(o, before).err()
        }
        1 => b.resize(slot(), any_sz()).err(),
        _ => b.remove(slot()).err(),
    };
    kani::cover!(err.is_some(), "an error");
    if let Some(e) = err {
        assert!(slots(&b) == pre);
        assert!(l2_fixed(&b) == pre_l2);
        assert!(b.check_invariants());
        kani::cover!(e == BookError::Crossed, "crossed rest refused");
        kani::cover!(e == BookError::AnchorMismatch, "anchor mismatch refused");
    }
}
per_sides!(
    3,
    error_body {
        book_error_leaves_book_unchanged_bb: [Bid, Bid],
        book_error_leaves_book_unchanged_ba: [Bid, Ask],
        book_error_leaves_book_unchanged_ab: [Ask, Bid],
        book_error_leaves_book_unchanged_aa: [Ask, Ask],
    }
);

/// begin/rollback restores the book exactly, one `Resized` journal entry.
/// Bounds: <= 1 resting order on side `s0`, begin, ONE resize (slot
/// 0..=1, size 1..=2). After rollback both slots hold the same node (all
/// fields, queue links included) and L2 is as before.
fn rollback_resized_body(s0: Side) {
    let mut b = book_with(&[s0]);
    b.begin();
    let pre = slots(&b);
    let pre_l2 = l2_fixed(&b);
    let ok = b.resize(slot(), sz()).is_ok();
    let changed = slots(&b) != pre;
    b.rollback(|_| {});
    assert!(slots(&b) == pre);
    assert!(l2_fixed(&b) == pre_l2);
    kani::cover!(ok && changed, "rollback of a real resize");
}
per_sides!(
    3,
    rollback_resized_body {
        book_rollback_exact_resized_b: [Bid],
        book_rollback_exact_resized_a: [Ask],
    }
);

/// As above, one `Removed` journal entry: <= 1 resting order on side `s0`,
/// begin, ONE remove (slot 0..=1).
fn rollback_removed_body(s0: Side) {
    let mut b = book_with(&[s0]);
    b.begin();
    let pre = slots(&b);
    let pre_l2 = l2_fixed(&b);
    let ok = b.remove(slot()).is_ok();
    b.rollback(|_| {});
    assert!(slots(&b) == pre);
    assert!(l2_fixed(&b) == pre_l2);
    kani::cover!(ok, "a removal undone");
}
per_sides!(
    3,
    rollback_removed_body {
        book_rollback_exact_removed_b: [Bid],
        book_rollback_exact_removed_a: [Ask],
    }
);

/// As above, one `Rested` journal entry: an empty book, begin, ONE rest on
/// side `s0` (so <= 1 order).
fn rollback_rested_body(s0: Side) {
    let mut b = book_with(&[]);
    b.begin();
    let pre = slots(&b);
    let pre_l2 = l2_fixed(&b);
    let ok = b.rest(new_order(5, s0), None).is_ok();
    b.rollback(|_| {});
    assert!(slots(&b) == pre);
    assert!(l2_fixed(&b) == pre_l2);
    kani::cover!(ok, "a rest undone");
}
per_sides!(
    3,
    rollback_rested_body {
        book_rollback_exact_rested_b: [Bid],
        book_rollback_exact_rested_a: [Ask],
    }
);

// ---------------------------------------------------- engine-level proofs

fn cap() -> Capacity {
    Capacity {
        orders: 2,
        levels_per_side: 1,
        actions_per_block: 1,
        events_per_block: 4,
    }
}

/// An engine loaded from a snapshot of <= 1 resting order (oid 1, fields
/// from the small domains, side `s0`, cloid none/1). Next oid is 10.
fn engine_with(stp: bool, s0: Side) -> Engine {
    let mut v = Vec::new();
    if kani::any() {
        v.push(RestingOrder {
            asset: ASSET,
            user: usr(),
            oid: Oid(1),
            side: s0,
            px: px(),
            sz: sz(),
            cloid: cloid(),
            t_ms: 0,
        });
    }
    let cfg = EngineConfig {
        stp,
        alo_priority_window_ms: 400,
    };
    let e = Engine::from_l4_snapshot(&[ASSET], v, Some(Oid(10)), cap(), cfg);
    kani::assume(e.is_ok());
    e.unwrap_or_else(|_| unreachable!())
}

/// Order leg: GTC / ALO / IOC, prices 1..=2, sizes 1..=2, cloid none/1,
/// priority 0 or 1, no hint.
fn order_req(side: Side) -> OrderReq {
    let k: u8 = kani::any();
    kani::assume(k < 3);
    OrderReq {
        asset: ASSET,
        side,
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

/// Target: oid 1 (snapshot), oid 10 (not resting) or cloid 1.
fn order_ref() -> OrderRef {
    let k: u8 = kani::any();
    kani::assume(k < 3);
    match k {
        0 => OrderRef::Oid(Oid(1)),
        1 => OrderRef::Oid(Oid(10)),
        _ => OrderRef::Cloid(CLOIDS[0]),
    }
}

/// The small engine view: resting orders (book order, public fields), L2,
/// next oid, oid-index size, and where `cancelByCloid(1)` of each user goes.
#[derive(PartialEq, Eq)]
struct View {
    orders: Vec<Resting>,
    l2: Vec<(Side, Px, Sz, u32)>,
    next_oid: Oid,
    indexed: usize,
    cloids: [Option<Oid>; 2],
}

fn view(e: &Engine) -> View {
    View {
        orders: engine_orders(e),
        l2: engine_l2(e),
        next_oid: e.next_oid(),
        indexed: e.order_count(),
        cloids: [
            cloid_resolves(e, USERS[0], CLOIDS[0]),
            cloid_resolves(e, USERS[1], CLOIDS[0]),
        ],
    }
}

/// (7) index consistency. Bounds: snapshot of <= 1 order, ONE single-leg
/// action (order GTC/ALO/IOC; cancel or modify by oid 1/10 or cloid 1),
/// 2 users, STP on or off; <= 2 orders. After the block the oid index is a
/// bijection onto the resting orders (`Engine::check_invariants`) and each
/// user's cloid 1 resolves to its newest resting order with cloid 1, or to
/// nothing when none rests.
fn engine_index_consistent_body(s0: Side, s1: Side) {
    let mut e = engine_with(kani::any(), s0);
    let user = usr();
    let k: u8 = kani::any();
    let o = [order_req(s1)];
    let c = [CancelReq {
        asset: ASSET,
        target: order_ref(),
    }];
    let m = [ModifyReq {
        target: order_ref(),
        order: order_req(s1),
    }];
    let a = match k {
        0 => UserAction::Order { user, orders: &o },
        1 => UserAction::Cancel { user, cancels: &c },
        _ => UserAction::Modify { user, modifies: &m },
    };
    e.execute_block(1, &[a]);
    assert!(e.check_invariants());
    let orders = engine_orders(&e);
    assert!(orders.len() == e.order_count());
    for u in USERS {
        let want = orders
            .iter()
            .filter(|r| r.user == u && r.cloid == Some(CLOIDS[0]))
            .map(|r| r.oid)
            .max();
        assert!(cloid_resolves(&e, u, CLOIDS[0]) == want);
    }
    kani::cover!(
        e.order_count() == 2 && orders.iter().all(|r| r.cloid.is_some()),
        "two orders with cloids after the block"
    );
}
per_sides!(
    4,
    engine_index_consistent_body {
        engine_index_consistent_bb: [Bid, Bid],
        engine_index_consistent_ba: [Bid, Ask],
        engine_index_consistent_ab: [Ask, Bid],
        engine_index_consistent_aa: [Ask, Ask],
    }
);

/// (5) engine level. Bounds: snapshot of <= 1 order, ONE action: an order
/// leg with every field unconstrained (any asset, u64 price and size, TIF
/// incl. Other, reduce-only, pre-reject, oid hint, priority), a cancel or
/// modify with any target, a noop or an unsupported action; any block time.
/// No panic and no overflow; the engine's invariants hold.
fn engine_no_panic_any_action_body(s0: Side, s1: Side) {
    let mut e = engine_with(kani::any(), s0);
    let any_req = || {
        let t: u8 = kani::any();
        kani::assume(t < 4);
        OrderReq {
            asset: kani::any(),
            side: s1,
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
    let user = usr();
    let o = [any_req()];
    let c = [CancelReq {
        asset: kani::any(),
        target: any_ref(),
    }];
    let m = [ModifyReq {
        target: any_ref(),
        order: any_req(),
    }];
    let k: u8 = kani::any();
    let a = match k {
        0 => UserAction::Order { user, orders: &o },
        1 => UserAction::Cancel { user, cancels: &c },
        2 => UserAction::Modify { user, modifies: &m },
        3 => UserAction::Noop,
        _ => UserAction::Unsupported("x"),
    };
    let out = e.execute_block(kani::any(), &[a]);
    let placed = out
        .statuses
        .iter()
        .any(|s| matches!(s, OrderStatus::Placed { .. }));
    assert!(e.check_invariants());
    kani::cover!(placed, "an order placed");
}
per_sides!(
    4,
    engine_no_panic_any_action_body {
        engine_no_panic_any_action_bb: [Bid, Bid],
        engine_no_panic_any_action_ba: [Bid, Ask],
        engine_no_panic_any_action_ab: [Ask, Bid],
        engine_no_panic_any_action_aa: [Ask, Ask],
    }
);

/// F3 (a). Bounds: snapshot <= 1 order, ONE order leg (domain of
/// `order_req`) with `oid_hint` None or any u64, STP on/off. The outcome
/// (apart from `oid_mismatches`) and the resulting resting orders, L2, next
/// oid and index size equal the no-hint run; with the hint None or equal to
/// the counter, `oid_mismatches == 0`. (Cloid resolution is not compared
/// here; `tests/exhaustive_oid_hint.rs` compares it.)
fn engine_oid_hint_does_not_influence_body(s0: Side, s1: Side) {
    let base = engine_with(kani::any(), s0);
    let o = order_req(s1);
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
    assert!(engine_orders(&hinted) == engine_orders(&plain));
    assert!(engine_l2(&hinted) == engine_l2(&plain));
    assert!(hinted.next_oid() == plain.next_oid());
    assert!(hinted.order_count() == plain.order_count());
    if h.is_none() || h == Some(base.next_oid()) {
        assert!(m == 0);
    }
    kani::cover!(h.is_some() && m == 0, "correct hint");
    kani::cover!(m > 0, "counted mismatch");
}
per_sides!(
    4,
    engine_oid_hint_does_not_influence_body {
        engine_oid_hint_does_not_influence_bb: [Bid, Bid],
        engine_oid_hint_does_not_influence_ba: [Bid, Ask],
        engine_oid_hint_does_not_influence_ab: [Ask, Bid],
        engine_oid_hint_does_not_influence_aa: [Ask, Ask],
    }
);

/// F3 (b), refined. Same bounds. A leg that takes an oid (counter moved)
/// with a hint different from the counter gives `oid_mismatches == 1`.
fn engine_oid_hint_mismatch_counted_body(s0: Side, s1: Side) {
    let mut e = engine_with(kani::any(), s0);
    let c = e.next_oid();
    let h = Oid(kani::any());
    let o = [OrderReq {
        oid_hint: Some(h),
        ..order_req(s1)
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
per_sides!(
    4,
    engine_oid_hint_mismatch_counted_body {
        engine_oid_hint_mismatch_counted_bb: [Bid, Bid],
        engine_oid_hint_mismatch_counted_ba: [Bid, Ask],
        engine_oid_hint_mismatch_counted_ab: [Ask, Bid],
        engine_oid_hint_mismatch_counted_aa: [Ask, Ask],
    }
);

/// (4)/(7) rollback, one kind of journal entry (`Removed`). Bounds:
/// snapshot of <= 1 order (cloid none/1), ONE cancel by oid 1/10 or cloid
/// 1, 2 users, STP on/off. `resync(&[])` after `execute_block` gives back
/// the pre-block engine: resting orders (public fields, queue order), L2,
/// next oid, oid-index size and each user's cloid-1 resolution.
fn engine_resync_empty_one_action_body(s0: Side) {
    let mut e = engine_with(kani::any(), s0);
    let pre = view(&e);
    let c = [CancelReq {
        asset: ASSET,
        target: order_ref(),
    }];
    let changed = !e
        .execute_block(
            1,
            &[UserAction::Cancel {
                user: usr(),
                cancels: &c,
            }],
        )
        .diffs
        .is_empty();
    assert!(e.resync(&[]).is_ok());
    assert!(view(&e) == pre);
    assert!(e.check_invariants());
    kani::cover!(changed, "cancel rolled back");
}
per_sides!(
    3,
    engine_resync_empty_one_action_body {
        engine_resync_empty_one_action_b: [Bid],
        engine_resync_empty_one_action_a: [Ask],
    }
);

/// (4) resync to the block's own diffs, same bounds as above (one cancel).
/// E1 runs the block; E2 runs it and then `resync(E1's diffs)`. Both end in
/// the same state (resting orders, L2, next oid, index size, cloid
/// resolution): the book after the block is the book its diffs describe.
fn engine_resync_own_diffs_reproduces_block_body(s0: Side) {
    let mut e1 = engine_with(kani::any(), s0);
    let mut e2 = e1.clone();
    let c = [CancelReq {
        asset: ASSET,
        target: order_ref(),
    }];
    let a = [UserAction::Cancel {
        user: usr(),
        cancels: &c,
    }];
    let diffs: Vec<BookDiff> = e1.execute_block(1, &a).diffs.clone();
    e2.execute_block(1, &a);
    assert!(e2.resync(&diffs).is_ok());
    e2.set_next_oid(e1.next_oid());
    assert!(view(&e1) == view(&e2));
    kani::cover!(!diffs.is_empty(), "a diff replayed");
}
per_sides!(
    3,
    engine_resync_own_diffs_reproduces_block_body {
        engine_resync_own_diffs_reproduces_block_b: [Bid],
        engine_resync_own_diffs_reproduces_block_a: [Ask],
    }
);
