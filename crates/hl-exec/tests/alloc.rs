//! Zero allocation per block after warm-up: a steady-state mix of ALO
//! rests, crossing ALO rejects, GTC/IOC takers that fill and rest, cancels by
//! oid and cloid, modifies and self-trades, on a book that stays roughly the
//! same size. Measured with the workspace `hl-alloc-gate` (counts every heap
//! call on this thread).
#![allow(clippy::unwrap_used, missing_docs)]

use hl_exec::{
    CancelReq, Capacity, Cloid, Engine, EngineConfig, ModifyReq, Oid, OrderRef, OrderReq, Px, Side,
    Sz, Tif, User, UserAction,
};

fn req(side: Side, px: u64, sz: u64, tif: Tif, cloid: u128) -> OrderReq {
    OrderReq {
        asset: 0,
        side,
        px: Px(px),
        sz: Sz(sz),
        reduce_only: false,
        tif,
        cloid: Some(Cloid(cloid)),
        priority: 3,
        pre_reject: None,
        oid_hint: None,
    }
}

#[test]
fn execute_block_does_not_allocate_after_warmup() {
    let cap = Capacity {
        orders: 1 << 14,
        levels_per_side: 256,
        actions_per_block: 64,
        events_per_block: 4096,
    };
    let mut e =
        Engine::from_l4_snapshot(&[0], [], Some(Oid(1)), cap, EngineConfig::default()).unwrap();
    let mm = User([1; 20]);
    let taker = User([2; 20]);
    let mut legs: Vec<OrderReq> = Vec::with_capacity(64);
    let mut cancels: Vec<CancelReq> = Vec::with_capacity(64);
    let mut mods: Vec<ModifyReq> = Vec::with_capacity(8);
    let mut tk_legs: Vec<OrderReq> = Vec::with_capacity(8);
    hl_alloc_gate::assert_no_alloc_after_warmup("hl-exec execute_block", 200, 2000, |tick| {
        let t = u128::from(tick);
        let mid = 1000 + (tick % 7);
        legs.clear();
        cancels.clear();
        mods.clear();
        tk_legs.clear();
        for k in 0..10u64 {
            legs.push(req(
                Side::Bid,
                mid - 1 - k,
                5 + k,
                Tif::Alo,
                t * 100 + u128::from(k),
            ));
            legs.push(req(
                Side::Ask,
                mid + 1 + k,
                5 + k,
                Tif::Alo,
                t * 100 + 50 + u128::from(k),
            ));
        }
        legs.push(req(Side::Bid, mid + 3, 1, Tif::Alo, 0)); // likely crosses
                                                            // Cancel last tick's quotes by cloid, and a never-placed one.
        if tick > 0 {
            for k in 0..10u128 {
                cancels.push(CancelReq {
                    asset: 0,
                    target: OrderRef::Cloid(Cloid((t - 1) * 100 + k)),
                });
                cancels.push(CancelReq {
                    asset: 0,
                    target: OrderRef::Cloid(Cloid((t - 1) * 100 + 50 + k)),
                });
            }
            cancels.push(CancelReq {
                asset: 0,
                target: OrderRef::Oid(Oid(u64::MAX - 1)),
            });
        }
        mods.push(ModifyReq {
            target: OrderRef::Cloid(Cloid(t * 100 + 3)),
            order: req(Side::Bid, mid - 2, 7, Tif::Alo, t * 100 + 99),
        });
        tk_legs.push(req(Side::Bid, mid + 2, 12, Tif::Ioc, 1));
        tk_legs.push(req(Side::Ask, mid - 2, 9, Tif::Gtc, 2));
        tk_legs.push(req(Side::Bid, mid + 5, 3, Tif::Ioc, 3));
        let acts = [
            UserAction::Order {
                user: mm,
                orders: &legs,
            },
            UserAction::Modify {
                user: mm,
                modifies: &mods,
            },
            UserAction::Order {
                user: taker,
                orders: &tk_legs[..2],
            },
            UserAction::Order {
                user: mm,
                orders: &tk_legs[2..],
            }, // self-trade path
            UserAction::Cancel {
                user: mm,
                cancels: &cancels,
            },
            UserAction::Cancel {
                user: taker,
                cancels: &cancels[..cancels.len().min(1)],
            },
            UserAction::Noop,
        ];
        let out = e.execute_block(tick * 70, &acts);
        assert!(!out.has_unsupported);
        // Keep the resting book bounded: drop the taker's GTC rest, if any.
        if tick % 50 == 49 {
            assert!(e.check_invariants());
        }
    });
    assert!(e.check_invariants());
}
