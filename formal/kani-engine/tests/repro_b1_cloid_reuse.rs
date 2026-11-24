//! B1 regression test. Failed on lane 1 `56514ca`; fixed in `ff7821e`
//! (journalled per-book cloid index).
//! A user's new order reuses the cloid of their own live order. The block
//! overwrites the (user, cloid) index entry, and `resync(&[])` then deletes
//! it instead of restoring it: the old order still rests but
//! `cancelByCloid` no longer finds it.

use hl_exec::{
    CancelReq, Capacity, Cloid, Engine, EngineConfig, Oid, OrderRef, OrderReq, OrderStatus, Px,
    RestingOrder, Side, Sz, Tif, User, UserAction,
};

fn u1() -> User {
    let mut b = [0u8; 20];
    b[0] = 1;
    User(b)
}

fn cancel_by_cloid_finds(e: &Engine, c: Cloid) -> bool {
    let mut e = e.clone();
    let leg = [CancelReq {
        asset: 0,
        target: OrderRef::Cloid(c),
    }];
    let out = e.execute_block(
        2,
        &[UserAction::Cancel {
            user: u1(),
            cancels: &leg,
        }],
    );
    out.statuses.first() == Some(&OrderStatus::Cancelled)
}

#[test]
fn resync_empty_restores_cloid_of_live_order() {
    let snap = [RestingOrder {
        asset: 0,
        user: u1(),
        oid: Oid(2),
        side: Side::Bid,
        px: Px(1),
        sz: Sz(1),
        cloid: Some(Cloid(1)),
        t_ms: 0,
    }];
    let cap = Capacity {
        orders: 4,
        levels_per_side: 3,
        actions_per_block: 2,
        events_per_block: 8,
    };
    let mut e =
        Engine::from_l4_snapshot(&[0], snap, Some(Oid(10)), cap, EngineConfig::default()).unwrap();
    assert!(cancel_by_cloid_finds(&e, Cloid(1)), "before the block");
    let o = [OrderReq {
        asset: 0,
        side: Side::Bid,
        px: Px(1),
        sz: Sz(1),
        reduce_only: false,
        tif: Tif::Gtc,
        cloid: Some(Cloid(1)),
        priority: 0,
        pre_reject: None,
        oid_hint: None,
    }];
    e.execute_block(
        1,
        &[UserAction::Order {
            user: u1(),
            orders: &o,
        }],
    );
    e.resync(&[]).unwrap();
    assert_eq!(e.order_count(), 1);
    assert!(
        cancel_by_cloid_finds(&e, Cloid(1)),
        "after resync(&[]): oid 2 rests with cloid 1 but is unreachable"
    );
}
