//! The cloid index, checked against what native's state shows.
//!
//! Native (hl-node `translate-abci-state` at 1232430000, `books[].cloid_tracker`)
//! keeps one cloid index per book, mapping (user, cloid) to the newest order
//! placed with it. It accepts a new order that reuses a cloid still live for
//! the same user (the snapshot holds one user with 60 live orders on one cloid
//! on one book, and the same cloid live on two books); older orders with that
//! cloid stay on the book, unreachable by cloid.
#![allow(clippy::unwrap_used, missing_docs)]

use hl_exec::{
    Asset, CancelReq, Capacity, Cloid, Engine, EngineConfig, Oid, OrderRef, OrderReq, OrderStatus,
    Px, RejectReason, RestingOrder, Side, Sz, Tif, User, UserAction,
};

const A: Asset = 0;
const B: Asset = 6;

fn cap() -> Capacity {
    Capacity {
        orders: 64,
        levels_per_side: 16,
        actions_per_block: 16,
        events_per_block: 64,
    }
}

fn u1() -> User {
    User([1; 20])
}

fn resting(asset: Asset, oid: u64, cloid: u128) -> RestingOrder {
    RestingOrder {
        asset,
        user: u1(),
        oid: Oid(oid),
        side: Side::Bid,
        px: Px(1),
        sz: Sz(1),
        cloid: Some(Cloid(cloid)),
        t_ms: 0,
    }
}

fn gtc(asset: Asset, tif: Tif, cloid: u128) -> OrderReq {
    OrderReq {
        asset,
        side: Side::Bid,
        px: Px(1),
        sz: Sz(1),
        reduce_only: false,
        tif,
        cloid: Some(Cloid(cloid)),
        priority: 0,
        pre_reject: None,
        oid_hint: None,
    }
}

fn place(e: &mut Engine, o: OrderReq) -> OrderStatus {
    e.execute_block(
        0,
        &[UserAction::Order {
            user: u1(),
            orders: std::slice::from_ref(&o),
        }],
    )
    .statuses_of(0)[0]
}

/// Cancel by cloid in a block of its own; returns the status and whether
/// `oid` is gone from the asset's book afterwards.
fn cancel_cloid(e: &mut Engine, asset: Asset, cloid: u128, oid: u64) -> (OrderStatus, bool) {
    let c = [CancelReq {
        asset,
        target: OrderRef::Cloid(Cloid(cloid)),
    }];
    let st = e
        .execute_block(
            0,
            &[UserAction::Cancel {
                user: u1(),
                cancels: &c,
            }],
        )
        .statuses_of(0)[0];
    let gone = !e
        .book(asset)
        .unwrap()
        .queue(Side::Bid, Px(1))
        .any(|n| n.oid == Oid(oid));
    (st, gone)
}

/// Proof's B1: a block that reuses a live cloid, undone by `resync(&[])`,
/// must leave the snapshot's order reachable by its cloid again.
#[test]
fn resync_restores_a_cloid_entry_the_block_overwrote() {
    for tif in [Tif::Gtc, Tif::Alo] {
        let mut e = Engine::from_l4_snapshot(
            &[A],
            [resting(A, 2, 1)],
            Some(Oid(10)),
            cap(),
            EngineConfig::default(),
        )
        .unwrap();
        let st = place(&mut e, gtc(A, tif, 1));
        assert!(
            matches!(st, OrderStatus::Placed { oid: Oid(10), .. }),
            "{st:?}"
        );
        e.resync(&[]).unwrap();
        assert!(e.check_invariants());
        assert_eq!(
            cancel_cloid(&mut e, A, 1, 2),
            (OrderStatus::Cancelled, true),
            "{tif:?}"
        );
    }
}

/// Native accepts the reuse; the cloid then names the newest order.
#[test]
fn reusing_a_live_cloid_rests_and_the_cloid_names_the_newest_order() {
    let mut e = Engine::from_l4_snapshot(
        &[A],
        [resting(A, 2, 1)],
        Some(Oid(10)),
        cap(),
        EngineConfig::default(),
    )
    .unwrap();
    let st = place(&mut e, gtc(A, Tif::Gtc, 1));
    assert!(
        matches!(st, OrderStatus::Placed { oid: Oid(10), .. }),
        "{st:?}"
    );
    assert_eq!(
        cancel_cloid(&mut e, A, 1, 10),
        (OrderStatus::Cancelled, true)
    );
    // oid 2 is now unreachable by cloid, as in native's tracker.
    assert_eq!(
        cancel_cloid(&mut e, A, 1, 2),
        (OrderStatus::Rejected(RejectReason::UnknownOrder), false)
    );
}

/// The index is per book: the same (user, cloid) live on two books is
/// cancelled on the book the cancel names.
#[test]
fn cloid_index_is_per_book() {
    let mut e = Engine::from_l4_snapshot(
        &[A, B],
        [resting(A, 2, 1), resting(B, 3, 1)],
        Some(Oid(10)),
        cap(),
        EngineConfig::default(),
    )
    .unwrap();
    assert_eq!(
        cancel_cloid(&mut e, A, 1, 2),
        (OrderStatus::Cancelled, true)
    );
    assert_eq!(
        cancel_cloid(&mut e, B, 1, 3),
        (OrderStatus::Cancelled, true)
    );

    // Same after placing: a new order on B with the cloid leaves A's entry.
    let mut e = Engine::from_l4_snapshot(
        &[A, B],
        [resting(A, 2, 1)],
        Some(Oid(10)),
        cap(),
        EngineConfig::default(),
    )
    .unwrap();
    place(&mut e, gtc(B, Tif::Gtc, 1));
    assert_eq!(
        cancel_cloid(&mut e, A, 1, 2),
        (OrderStatus::Cancelled, true)
    );
}

/// A snapshot with several live orders on one (user, cloid) indexes the
/// newest (largest oid), whatever order the snapshot lists them in.
#[test]
fn snapshot_indexes_the_newest_order_of_a_shared_cloid() {
    let mut e = Engine::from_l4_snapshot(
        &[A],
        [resting(A, 7, 1), resting(A, 3, 1)],
        Some(Oid(10)),
        cap(),
        EngineConfig::default(),
    )
    .unwrap();
    assert_eq!(
        cancel_cloid(&mut e, A, 1, 7),
        (OrderStatus::Cancelled, true)
    );
}

/// Real block 1232430002 (our node's order statuses): a BTC order under the
/// $10 minimum is rejected yet takes oid 614579393161, and the next order in
/// the block takes 614579393162. Native spends an oid on every order leg that
/// passes the static checks, whatever the outcome. A leg the engine does not
/// model (reduce-only, unknown TIF, a book it does not hold) is answered
/// `Unsupported` but must still take its oid, or every later oid in the block
/// is one short.
#[test]
fn an_unmodelled_leg_still_takes_its_oid() {
    const START: u64 = 614_579_393_161;
    let unmodelled = [
        OrderReq {
            reduce_only: true,
            ..gtc(A, Tif::Gtc, 9)
        },
        gtc(A, Tif::Other, 9),
        gtc(42, Tif::Gtc, 9),
    ];
    for leg in unmodelled {
        let mut e =
            Engine::from_l4_snapshot(&[A], [], Some(Oid(START)), cap(), EngineConfig::default())
                .unwrap();
        let next = [gtc(A, Tif::Gtc, 2)];
        let out = e.execute_block(
            0,
            &[
                UserAction::Order {
                    user: u1(),
                    orders: std::slice::from_ref(&leg),
                },
                UserAction::Order {
                    user: User([2; 20]),
                    orders: &next,
                },
            ],
        );
        assert_eq!(out.statuses_of(0)[0], OrderStatus::Unsupported);
        assert!(
            matches!(out.statuses_of(1)[0], OrderStatus::Placed { oid: Oid(o), .. } if o == START + 1),
            "{leg:?}: {:?}",
            out.statuses_of(1)[0]
        );
        assert_eq!(e.next_oid(), Oid(START + 2));
    }
}

/// Static rejects (lot size, price decimals, tick size) never reach the book
/// and native writes no order status for them: no oid.
#[test]
fn a_static_reject_takes_no_oid() {
    let mut e =
        Engine::from_l4_snapshot(&[A], [], Some(Oid(100)), cap(), EngineConfig::default()).unwrap();
    let leg = OrderReq {
        pre_reject: Some(RejectReason::TickSize),
        ..gtc(A, Tif::Gtc, 1)
    };
    e.execute_block(
        0,
        &[UserAction::Order {
            user: u1(),
            orders: std::slice::from_ref(&leg),
        }],
    );
    assert_eq!(e.next_oid(), Oid(100));
}
