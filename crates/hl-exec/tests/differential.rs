//! Differential tests against the Phase 1 reference book (`hl-book-ref`,
//! the executable spec): random valid and invalid action sequences must give
//! identical books after every leg and identical outcomes per leg. Also: our
//! emitted diffs replayed onto a second reference book reproduce our book,
//! the incremental L2 equals the L2 recomputed from the queues, and
//! `resync` (rollback + adopt native diffs) lands on exactly the state the
//! diffs describe.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    missing_docs
)]

use hl_book_ref as r;
use hl_exec::{
    Asset, BookDiff, CancelReq, Capacity, Cloid, DiffKind, Engine, EngineConfig, ModifyReq, Oid,
    OrderRef, OrderReq, OrderStatus, Px, RejectReason, Side, Sz, Tif, User, UserAction,
};
use proptest::prelude::*;

const A: Asset = 7;

fn user(u: u8) -> User {
    User([u; 20])
}

fn side(b: bool) -> Side {
    if b {
        Side::Bid
    } else {
        Side::Ask
    }
}

fn rside(s: Side) -> r::Side {
    match s {
        Side::Bid => r::Side::Bid,
        Side::Ask => r::Side::Ask,
    }
}

fn cap() -> Capacity {
    Capacity {
        orders: 4096,
        levels_per_side: 64,
        actions_per_block: 64,
        events_per_block: 1024,
    }
}

fn engine() -> Engine {
    Engine::from_l4_snapshot(&[A], [], Some(Oid(1)), cap(), EngineConfig::default()).unwrap()
}

/// Our book as the reference's plain snapshot.
fn snap(e: &Engine) -> r::BookSnapshot {
    let b = e.book(A).unwrap();
    let side = |s: Side| -> Vec<r::Level> {
        b.levels(s)
            .map(|px| r::Level {
                px: r::Px(px.0),
                orders: b
                    .queue(s, px)
                    .map(|n| (r::Oid(n.oid.0), r::Lots(n.sz.0)))
                    .collect(),
            })
            .collect()
    };
    r::BookSnapshot {
        bids: side(Side::Bid),
        asks: side(Side::Ask),
    }
}

/// The incremental L2 must equal the one recomputed from the queues.
fn l2_matches(e: &Engine) -> bool {
    let b = e.book(A).unwrap();
    let inc: Vec<_> = [Side::Bid, Side::Ask]
        .into_iter()
        .flat_map(|s| b.l2(s).map(move |(px, sz, n)| (s, px.0, sz.0, n)))
        .collect();
    let rec: Vec<_> = [Side::Bid, Side::Ask]
        .into_iter()
        .flat_map(|s| {
            b.levels(s).map(move |px| {
                let q: Vec<_> = b.queue(s, px).collect();
                let tot: u64 = q.iter().map(|n| n.sz.0).sum();
                (s, px.0, tot, u32::try_from(q.len()).unwrap())
            })
        })
        .collect();
    inc == rec
}

fn diff_to_op(d: &BookDiff) -> r::BookOp {
    let order = |sz: Sz| r::Order {
        oid: r::Oid(d.oid.0),
        side: rside(d.side),
        px: r::Px(d.px.0),
        sz: r::Lots(sz.0),
    };
    match d.kind {
        DiffKind::New {
            sz,
            insert_before: None,
        } => r::BookOp::Insert(order(sz)),
        DiffKind::New {
            sz,
            insert_before: Some(b),
        } => r::BookOp::InsertBefore {
            before: r::Oid(b.0),
            order: order(sz),
        },
        DiffKind::Update { new, .. } => r::BookOp::Resize {
            oid: r::Oid(d.oid.0),
            sz: r::Lots(new.0),
        },
        DiffKind::Remove => r::BookOp::Cancel {
            oid: r::Oid(d.oid.0),
        },
    }
}

#[derive(Debug, Clone)]
enum Act {
    Order {
        u: u8,
        bid: bool,
        px: u64,
        sz: u64,
        tif: u8,
    },
    Cancel {
        u: u8,
        pick: usize,
        by_cloid: bool,
    },
    Modify {
        pick: usize,
        bid: bool,
        px: u64,
        sz: u64,
        tif: u8,
    },
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        5 => (0u8..3, any::<bool>(), 90u64..111, 0u64..30, 0u8..3)
            .prop_map(|(u, bid, px, sz, tif)| Act::Order { u, bid, px, sz, tif }),
        2 => (0u8..3, any::<usize>(), any::<bool>())
            .prop_map(|(u, pick, by_cloid)| Act::Cancel { u, pick, by_cloid }),
        1 => (any::<usize>(), any::<bool>(), 90u64..111, 0u64..30, 0u8..3)
            .prop_map(|(pick, bid, px, sz, tif)| Act::Modify { pick, bid, px, sz, tif }),
    ]
}

fn tif(t: u8) -> (Tif, r::Tif) {
    match t {
        0 => (Tif::Alo, r::Tif::Alo),
        1 => (Tif::Gtc, r::Tif::Gtc),
        _ => (Tif::Ioc, r::Tif::Ioc),
    }
}

fn req(bid: bool, px: u64, sz: u64, t: Tif, cloid: Option<Cloid>) -> OrderReq {
    OrderReq {
        asset: A,
        side: side(bid),
        px: Px(px),
        sz: Sz(sz),
        reduce_only: false,
        tif: t,
        cloid,
        priority: 0,
        pre_reject: None,
        oid_hint: None,
    }
}

/// Our leg status against the reference's result for the same placement.
fn same_outcome(ours: OrderStatus, theirs: &Result<r::Placed, r::ApplyError>, rt: r::Tif) -> bool {
    match (ours, theirs) {
        (
            OrderStatus::Placed {
                filled,
                notional,
                rested,
                ..
            },
            Ok(p),
        ) => {
            let f: u64 = p.fills.iter().map(|f| f.sz.0).sum();
            let n: u128 = p
                .fills
                .iter()
                .map(|f| u128::from(f.px.0) * u128::from(f.sz.0))
                .sum();
            f == filled.0 && n == notional && p.rested.map_or(0, |l| l.0) == rested.0
        }
        (OrderStatus::Rejected(RejectReason::IocNoMatch), Ok(p)) => {
            rt == r::Tif::Ioc && p.fills.is_empty()
        }
        (
            OrderStatus::Rejected(RejectReason::AloWouldCross { .. }),
            Err(r::ApplyError::AloWouldCross { .. }),
        )
        | (OrderStatus::Rejected(RejectReason::ZeroSize), Err(r::ApplyError::ZeroSize(_))) => true,
        _ => false,
    }
}

/// Run `acts` on both books, one action per block. Returns a failure message.
fn run(acts: &[Act]) -> Result<(), String> {
    let mut e = engine();
    let mut rb = r::Book::new();
    let mut mirror = r::Book::new();
    // Every oid ever placed with its owner and cloid (dead ones too).
    let mut placed: Vec<(Oid, u8, Cloid)> = Vec::new();
    for (i, a) in acts.iter().enumerate() {
        let before = e.next_oid();
        match *a {
            Act::Order {
                u,
                bid,
                px,
                sz,
                tif: t,
            } => {
                let (tt, rt) = tif(t);
                let cloid = Cloid(u128::from(before.0));
                let legs = [req(bid, px, sz, tt, Some(cloid))];
                let out = e.execute_block(
                    0,
                    &[UserAction::Order {
                        user: user(u),
                        orders: &legs,
                    }],
                );
                let st = out.statuses_of(0)[0];
                let ord = r::Order {
                    oid: r::Oid(before.0),
                    side: rside(side(bid)),
                    px: r::Px(px),
                    sz: r::Lots(sz),
                };
                let theirs = rb.place_owned(r::Owner(u32::from(u)), ord, rt);
                if !same_outcome(st, &theirs, rt) {
                    return Err(format!("act {i} {a:?}: ours {st:?} theirs {theirs:?}"));
                }
                if sz > 0 {
                    placed.push((before, u, cloid));
                }
            }
            Act::Cancel { u, pick, by_cloid } => {
                let Some(&(oid, owner, cloid)) = placed.get(pick % placed.len().max(1)) else {
                    continue;
                };
                let target = if by_cloid {
                    OrderRef::Cloid(cloid)
                } else {
                    OrderRef::Oid(oid)
                };
                let legs = [CancelReq { asset: A, target }];
                let out = e.execute_block(
                    0,
                    &[UserAction::Cancel {
                        user: user(u),
                        cancels: &legs,
                    }],
                );
                let st = out.statuses_of(0)[0];
                let live = rb.get(r::Oid(oid.0)).is_some();
                let expect_ok = live && owner == u;
                if expect_ok {
                    rb.apply(&r::BookOp::Cancel { oid: r::Oid(oid.0) }).unwrap();
                }
                let ok = st == OrderStatus::Cancelled;
                let rej = st == OrderStatus::Rejected(RejectReason::UnknownOrder);
                if !(ok && expect_ok || rej && !expect_ok) {
                    return Err(format!(
                        "act {i} {a:?}: ours {st:?} expected ok={expect_ok}"
                    ));
                }
            }
            Act::Modify {
                pick,
                bid,
                px,
                sz,
                tif: t,
            } => {
                let Some(&(oid, owner, _)) = placed.get(pick % placed.len().max(1)) else {
                    continue;
                };
                let (tt, rt) = tif(t);
                let cloid = Cloid(u128::from(before.0));
                let legs = [ModifyReq {
                    target: OrderRef::Oid(oid),
                    order: req(bid, px, sz, tt, Some(cloid)),
                }];
                let out = e.execute_block(
                    0,
                    &[UserAction::Modify {
                        user: user(owner),
                        modifies: &legs,
                    }],
                );
                let st = out.statuses_of(0)[0];
                if rb.get(r::Oid(oid.0)).is_some() {
                    let ord = r::Order {
                        oid: r::Oid(before.0),
                        side: rside(side(bid)),
                        px: r::Px(px),
                        sz: r::Lots(sz),
                    };
                    let m = rb.modify(r::Oid(oid.0), Some(r::Owner(u32::from(owner))), ord, rt);
                    if !m.cancelled || !same_outcome(st, &m.placed, rt) {
                        return Err(format!("act {i} {a:?}: ours {st:?} theirs {m:?}"));
                    }
                    if sz > 0 {
                        placed.push((before, owner, cloid));
                    }
                } else if st != OrderStatus::Rejected(RejectReason::ModifyUnknownOrder)
                    || e.next_oid() != before
                {
                    return Err(format!("act {i} {a:?}: ours {st:?} for a dead target"));
                }
            }
        }
        for d in &e.outcome().diffs {
            // A full fill is `update` to 0 then `remove`; the remove applies it.
            if matches!(d.kind, DiffKind::Update { new: Sz(0), .. }) {
                continue;
            }
            mirror
                .apply(&diff_to_op(d))
                .map_err(|err| format!("act {i}: our diff {d:?} does not apply: {err}"))?;
        }
        let ours = snap(&e);
        if ours != rb.snapshot() {
            return Err(format!(
                "act {i} {a:?}: books differ\nours {ours:?}\nref  {:?}",
                rb.snapshot()
            ));
        }
        if ours != mirror.snapshot() {
            return Err(format!("act {i}: diffs do not reproduce our book"));
        }
        if !e.check_invariants() || !l2_matches(&e) {
            return Err(format!("act {i}: invariant or L2 view broken"));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn matches_the_reference_book(acts in prop::collection::vec(act(), 0..120)) {
        if let Err(msg) = run(&acts) {
            prop_assert!(false, "{}", msg);
        }
    }

    /// `resync(&[])` undoes a whole block exactly; `resync(diffs)` of another
    /// engine's run of the same block lands on that engine's state.
    #[test]
    fn resync_rolls_back_and_adopts(
        warm in prop::collection::vec((0u8..3, any::<bool>(), 90u64..111, 1u64..30, 0u8..3), 0..60),
        block in prop::collection::vec((0u8..3, any::<bool>(), 90u64..111, 1u64..30, 0u8..3), 1..40),
        junk in prop::collection::vec((0u8..3, any::<bool>(), 90u64..111, 1u64..30, 0u8..3), 1..40),
    ) {
        let legs = |v: &[(u8, bool, u64, u64, u8)]| -> Vec<(User, OrderReq)> {
            v.iter().map(|&(u, b, px, sz, t)| (user(u), req(b, px, sz, tif(t).0, Some(Cloid(u128::from(px * 1000 + sz)))))).collect()
        };
        let exec = |e: &mut Engine, v: &[(User, OrderReq)]| {
            let acts: Vec<UserAction<'_>> = v.iter().map(|(u, o)| UserAction::Order { user: *u, orders: std::slice::from_ref(o) }).collect();
            e.execute_block(0, &acts).diffs.clone()
        };
        let mut a = engine();
        exec(&mut a, &legs(&warm));
        let before = snap(&a);
        let oid0 = a.next_oid();
        let mut b = a.clone();
        let diffs = exec(&mut a, &legs(&block));
        exec(&mut b, &legs(&junk));
        let mut c = b.clone();
        c.resync(&[]).unwrap();
        prop_assert_eq!(snap(&c), before);
        prop_assert_eq!(c.next_oid(), oid0);
        prop_assert!(c.check_invariants());
        b.resync(&diffs).unwrap();
        // Diffs only show oids that rested; native's next response gives
        // the counter (oids taken by IOC / rejected ALO legs).
        prop_assert!(b.next_oid() <= a.next_oid());
        b.set_next_oid(a.next_oid());
        prop_assert_eq!(snap(&b), snap(&a));
        prop_assert!(b.check_invariants());
        prop_assert_eq!(b.order_count(), a.order_count());
        // The adopted engine keeps working identically.
        let next = legs(&junk);
        let da = exec(&mut a, &next);
        let db = exec(&mut b, &next);
        prop_assert_eq!(da, db);
        prop_assert_eq!(snap(&a), snap(&b));
    }
}

#[test]
fn ioc_with_nothing_to_match_is_rejected_and_takes_an_oid() {
    let mut e = engine();
    let legs = [req(true, 100, 5, Tif::Ioc, None)];
    let st = e
        .execute_block(
            0,
            &[UserAction::Order {
                user: user(1),
                orders: &legs,
            }],
        )
        .statuses_of(0)[0];
    assert_eq!(st, OrderStatus::Rejected(RejectReason::IocNoMatch));
    assert_eq!(e.next_oid(), Oid(2));
}

#[test]
fn alo_cross_reports_bbo_and_takes_an_oid() {
    let mut e = engine();
    let rest = [
        req(false, 101, 5, Tif::Alo, None),
        req(true, 99, 5, Tif::Alo, None),
    ];
    e.execute_block(
        0,
        &[UserAction::Order {
            user: user(1),
            orders: &rest,
        }],
    );
    let cross = [req(true, 101, 1, Tif::Alo, None)];
    let st = e
        .execute_block(
            0,
            &[UserAction::Order {
                user: user(2),
                orders: &cross,
            }],
        )
        .statuses_of(0)[0];
    assert_eq!(
        st,
        OrderStatus::Rejected(RejectReason::AloWouldCross {
            bid: Some(Px(99)),
            ask: Some(Px(101))
        })
    );
    assert_eq!(e.next_oid(), Oid(4));
}

#[test]
fn self_trade_removes_the_resting_order_and_continues() {
    let mut e = engine();
    let rest = [req(false, 100, 5, Tif::Alo, None)];
    e.execute_block(
        0,
        &[UserAction::Order {
            user: user(1),
            orders: &rest,
        }],
    );
    let rest2 = [req(false, 101, 5, Tif::Alo, None)];
    e.execute_block(
        0,
        &[UserAction::Order {
            user: user(2),
            orders: &rest2,
        }],
    );
    let take = [req(true, 101, 3, Tif::Ioc, None)];
    let out = e.execute_block(
        0,
        &[UserAction::Order {
            user: user(1),
            orders: &take,
        }],
    );
    assert_eq!(out.diffs[0].kind, DiffKind::Remove);
    assert_eq!(out.diffs[0].oid, Oid(1));
    assert_eq!(out.fills.len(), 1);
    assert_eq!(out.fills[0].px, Px(101));
}

#[test]
fn unsupported_marks_the_block() {
    let mut e = engine();
    let mut ro = req(true, 100, 5, Tif::Gtc, None);
    ro.reduce_only = true;
    let legs = [ro];
    let out = e.execute_block(
        0,
        &[
            UserAction::Order {
                user: user(1),
                orders: &legs,
            },
            UserAction::Unsupported("updateLeverage"),
        ],
    );
    assert!(out.has_unsupported);
    assert_eq!(out.statuses_of(0), &[OrderStatus::Unsupported]);
    assert!(out.responses[1].unsupported);
    assert_eq!(e.outcome().diffs, []);
}

/// "Action already expired": `expiresAfter < block time` rejects the whole
/// action, takes no oid and touches nothing; `expiresAfter == block time`
/// runs (our node, blocks 1232430001..=1232432000: 338/338 and 12 equal-time
/// actions not expired).
#[test]
fn expired_actions_are_rejected_without_an_oid() {
    let mut e = engine();
    let legs = [req(true, 100, 5, Tif::Alo, None)];
    let acts = [
        UserAction::Order {
            user: user(1),
            orders: &legs,
        },
        UserAction::Order {
            user: user(1),
            orders: &legs,
        },
    ];
    let out = e.execute_block_with_expiry(1000, &acts, &[Some(999), Some(1000)]);
    assert!(out.responses[0].expired);
    assert_eq!(out.statuses_of(0), &[]);
    assert!(!out.responses[1].expired);
    assert_eq!(
        out.statuses_of(1),
        &[OrderStatus::Placed {
            oid: Oid(1),
            filled: Sz(0),
            notional: 0,
            rested: Sz(5)
        }]
    );
    assert!(!out.has_unsupported);
    assert_eq!(e.next_oid(), Oid(2));
}

/// A leg that rested whole with no fill.
fn rest(oid: u64, sz: u64) -> OrderStatus {
    OrderStatus::Placed {
        oid: Oid(oid),
        filled: Sz(0),
        notional: 0,
        rested: Sz(sz),
    }
}

/// Native's in-block order (our node's blocks 1232430001..=1232432000:
/// consistent with the diff order on 2,000/2,000 blocks): within each
/// signed-action bundle, actions sending no GTC/IOC leg first, then cancels,
/// then actions sending a GTC/IOC (or trigger) leg; input order inside each
/// class; bundles in input order. Responses stay in input order.
#[test]
fn bundled_block_runs_alo_then_cancels_then_gtc_per_bundle() {
    let mut e = engine();
    // Block 1: a resting ask (oid 1) that the cancel below targets.
    let ask = [req(false, 105, 3, Tif::Gtc, None)];
    e.execute_block(
        0,
        &[UserAction::Order {
            user: user(1),
            orders: &ask,
        }],
    );
    assert_eq!(e.next_oid(), Oid(2));
    // Bundle 1 (input order): GTC buy crossing 105, cancel oid 1, ALO bid 90.
    // Native order: ALO (oid 2), cancel (removes 1), GTC (oid 3, rests: no ask left).
    // Bundle 2: ALO bid 91 (oid 4) after everything in bundle 1.
    let gtc = [req(true, 105, 1, Tif::Gtc, None)];
    let cancel = [CancelReq {
        asset: A,
        target: OrderRef::Oid(Oid(1)),
    }];
    let alo = [req(true, 90, 1, Tif::Alo, None)];
    let alo2 = [req(true, 91, 1, Tif::Alo, None)];
    let acts = [
        UserAction::Order {
            user: user(2),
            orders: &gtc,
        },
        UserAction::Cancel {
            user: user(1),
            cancels: &cancel,
        },
        UserAction::Order {
            user: user(3),
            orders: &alo,
        },
        UserAction::Order {
            user: user(3),
            orders: &alo2,
        },
    ];
    let out = e.execute_block_bundled(0, &acts, &[], &[3, 1]);
    assert_eq!(out.responses.len(), 4);
    assert_eq!(out.statuses_of(0), &[rest(3, 1)]);
    assert_eq!(out.statuses_of(1), &[OrderStatus::Cancelled]);
    assert_eq!(out.statuses_of(2), &[rest(2, 1)]);
    assert_eq!(out.statuses_of(3), &[rest(4, 1)]);
    let seq: Vec<(u64, bool)> = out
        .diffs
        .iter()
        .map(|d| (d.oid.0, matches!(d.kind, DiffKind::Remove)))
        .collect();
    assert_eq!(seq, vec![(2, false), (1, true), (3, false), (4, false)]);
    assert!(e.check_invariants());
}

/// With no bundle lengths the whole block is one bundle; input order is
/// kept only by the unbundled `execute_block`.
#[test]
fn bundled_block_without_lengths_is_one_bundle() {
    let mut e = engine();
    let gtc = [req(true, 100, 1, Tif::Gtc, None)];
    let alo = [req(true, 90, 1, Tif::Alo, None)];
    let acts = [
        UserAction::Order {
            user: user(2),
            orders: &gtc,
        },
        UserAction::Order {
            user: user(3),
            orders: &alo,
        },
    ];
    let out = e.execute_block_bundled(0, &acts, &[], &[]);
    assert_eq!(out.statuses_of(0), &[rest(2, 1)]);
    assert_eq!(out.statuses_of(1), &[rest(1, 1)]);
}

/// Native's diff shape for a match (our node's blocks 1232430001..=1232432000,
/// 5,193/5,193 maker-update runs): every maker fill is an `update` (to 0 for
/// a full fill), then the fully filled makers are removed in fill order,
/// then the taker's own `new` if it rests.
#[test]
fn match_diffs_are_updates_then_removes_then_new() {
    let mut e = engine();
    let asks = [
        req(false, 100, 2, Tif::Alo, None),
        req(false, 100, 5, Tif::Alo, None),
    ];
    e.execute_block(
        0,
        &[UserAction::Order {
            user: user(1),
            orders: &asks,
        }],
    );
    // Buy 4 @ 100 (fills oid 1 fully, oid 2 by 2), then a GTC rest at 99 is
    // not involved; a second taker buys 4 @ 101: fills the 3 left, rests 1.
    let t1 = [req(true, 100, 4, Tif::Gtc, None)];
    let t2 = [req(true, 101, 4, Tif::Gtc, None)];
    let out = e.execute_block(
        0,
        &[
            UserAction::Order {
                user: user(2),
                orders: &t1,
            },
            UserAction::Order {
                user: user(3),
                orders: &t2,
            },
        ],
    );
    let seq: Vec<(u64, String)> = out
        .diffs
        .iter()
        .map(|d| {
            (
                d.oid.0,
                match d.kind {
                    DiffKind::New { sz, .. } => format!("new {}", sz.0),
                    DiffKind::Update { orig, new } => format!("upd {}->{}", orig.0, new.0),
                    DiffKind::Remove => "remove".to_owned(),
                },
            )
        })
        .collect();
    let want: Vec<(u64, String)> = [
        (1, "upd 2->0"),
        (2, "upd 5->3"),
        (1, "remove"),
        (2, "upd 3->0"),
        (2, "remove"),
        (4, "new 1"),
    ]
    .iter()
    .map(|&(o, s)| (o, s.to_owned()))
    .collect();
    assert_eq!(seq, want);
    // Resync from native's shape (update to 0, then remove) must load.
    let native = out.diffs.clone();
    e.resync(&native).expect("native full-fill shape resyncs");
    assert!(e.check_invariants());
}
