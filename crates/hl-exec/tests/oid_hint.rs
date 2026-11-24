//! `oid_hint` on legs that take no oid (Proof's F3 at feae97a).
//!
//! A hint is native's oid for the leg. Where the engine takes no oid (a
//! static pre-reject, size 0, price 0, a modify of an unknown asset or
//! order), a `Some` hint means native spent an oid the engine did not, so
//! later oids drift: it must count in `oid_mismatches`. A `None` hint on
//! such a leg is agreement and counts nothing.
#![allow(clippy::unwrap_used, missing_docs)]

use hl_exec::{
    Asset, Capacity, Engine, EngineConfig, ModifyReq, Oid, OrderRef, OrderReq, Px, RejectReason,
    Side, Sz, Tif, User, UserAction,
};

const A: Asset = 0;

fn engine() -> Engine {
    Engine::from_l4_snapshot(
        &[A],
        [],
        Some(Oid(10)),
        Capacity {
            orders: 64,
            levels_per_side: 16,
            actions_per_block: 16,
            events_per_block: 64,
        },
        EngineConfig::default(),
    )
    .unwrap()
}

fn leg(asset: Asset, px: u64, sz: u64, hint: Option<u64>) -> OrderReq {
    OrderReq {
        asset,
        side: Side::Bid,
        px: Px(px),
        sz: Sz(sz),
        reduce_only: false,
        tif: Tif::Gtc,
        cloid: None,
        priority: 0,
        pre_reject: None,
        oid_hint: hint.map(Oid),
    }
}

fn order_mismatches(o: OrderReq) -> u32 {
    let mut e = engine();
    e.execute_block(
        0,
        &[UserAction::Order {
            user: User([1; 20]),
            orders: std::slice::from_ref(&o),
        }],
    )
    .oid_mismatches
}

fn modify_mismatches(target: OrderRef, o: OrderReq) -> u32 {
    let mut e = engine();
    let m = [ModifyReq { target, order: o }];
    e.execute_block(
        0,
        &[UserAction::Modify {
            user: User([1; 20]),
            modifies: &m,
        }],
    )
    .oid_mismatches
}

#[test]
fn hint_on_a_leg_that_takes_no_oid_is_a_mismatch() {
    // Proof's smallest trace: size 0, next_oid 10, hint 11.
    assert_eq!(order_mismatches(leg(A, 1, 0, Some(11))), 1);
    assert_eq!(order_mismatches(leg(A, 0, 1, Some(10))), 1);
    let mut pre = leg(A, 1, 1, Some(10));
    pre.pre_reject = Some(RejectReason::TickSize);
    assert_eq!(order_mismatches(pre), 1);
    // Modify of an unknown order, and of an asset the engine does not hold.
    assert_eq!(
        modify_mismatches(OrderRef::Oid(Oid(99)), leg(A, 1, 1, Some(10))),
        1
    );
    assert_eq!(
        modify_mismatches(OrderRef::Oid(Oid(99)), leg(7, 1, 1, Some(10))),
        1
    );
}

#[test]
fn no_hint_on_a_leg_that_takes_no_oid_is_agreement() {
    assert_eq!(order_mismatches(leg(A, 1, 0, None)), 0);
    assert_eq!(order_mismatches(leg(A, 0, 1, None)), 0);
    let mut pre = leg(A, 1, 1, None);
    pre.pre_reject = Some(RejectReason::TickSize);
    assert_eq!(order_mismatches(pre), 0);
    assert_eq!(
        modify_mismatches(OrderRef::Oid(Oid(99)), leg(A, 1, 1, None)),
        0
    );
    assert_eq!(
        modify_mismatches(OrderRef::Oid(Oid(99)), leg(7, 1, 1, None)),
        0
    );
}

#[test]
fn matching_hint_on_an_oid_taking_leg_still_counts_nothing() {
    assert_eq!(order_mismatches(leg(A, 1, 1, Some(10))), 0);
    assert_eq!(order_mismatches(leg(A, 1, 1, Some(11))), 1);
}
