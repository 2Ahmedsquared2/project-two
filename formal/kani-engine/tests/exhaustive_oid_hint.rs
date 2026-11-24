//! F3 oid-hint gate. Exhaustive bounded check (EVIDENCE, not a Kani proof;
//! Kani cannot yet finish `execute_block` at these bounds, see README).
//!
//! For every case, a block is run twice from the same engine: once with
//! the case's `oid_hint`s and once with every hint `None`. Checked:
//! (a) the outcome (responses, statuses, diffs, fills, flags) and the
//!     resulting state (orders + queue order, L2, next oid, indexes) are
//!     identical apart from `oid_mismatches`, for ANY hints; and when every
//!     hint is `None` or equal to the counter at its leg, `oid_mismatches == 0`;
//! (b) `oid_mismatches` equals the number of legs that TAKE an oid and carry
//!     a hint different from the counter. Legs that take no oid (pre-reject,
//!     zero size, modify of an unknown order) are counted apart as F3-gap.
//!
//! Domain: snapshot 0 or 1 resting order (oid 1; side x px {1,3} x sz {1,2}
//! x user {u1,u2}), next oid 10, STP on. Block of TWO single-leg actions from
//! u1, each: order (side x px {1,3} x sz {0,2} x TIF {GTC,ALO,IOC,Other} x
//! pre-reject {none, TickSize}), modify (target oid 1 or 10, same order
//! domain) or cancel oid 1; each order leg's hint in {None, 10, 11, 12}.
//!
//! Run: `cargo test --release -p kani-engine --test exhaustive_oid_hint -- --nocapture`

use hl_exec::{
    BlockOutcome, CancelReq, Capacity, Engine, EngineConfig, ModifyReq, Oid, OrderRef, OrderReq,
    Px, RejectReason, RestingOrder, Side, Sz, Tif, UserAction,
};
use kani_engine::model::{engine_view, ASSET, USERS};

fn cap() -> Capacity {
    Capacity {
        orders: 4,
        levels_per_side: 3,
        actions_per_block: 2,
        events_per_block: 8,
    }
}

#[derive(Clone, Copy, Debug)]
enum Act {
    Order(OrderReq),
    Modify(Oid, OrderReq),
    Cancel,
}

impl Act {
    fn with_hint(self, h: Option<Oid>) -> Self {
        match self {
            Act::Order(o) => Act::Order(OrderReq { oid_hint: h, ..o }),
            Act::Modify(t, o) => Act::Modify(t, OrderReq { oid_hint: h, ..o }),
            Act::Cancel => Act::Cancel,
        }
    }
}

/// Owned storage for one single-leg action.
struct Leg([OrderReq; 1], [ModifyReq; 1], [CancelReq; 1], u8);

fn leg(a: Act) -> Leg {
    let dummy = orders()[0];
    match a {
        Act::Order(o) => Leg(
            [o],
            [ModifyReq {
                target: OrderRef::Oid(Oid(0)),
                order: dummy,
            }],
            [cancel()],
            0,
        ),
        Act::Modify(t, o) => Leg(
            [dummy],
            [ModifyReq {
                target: OrderRef::Oid(t),
                order: o,
            }],
            [cancel()],
            1,
        ),
        Act::Cancel => Leg(
            [dummy],
            [ModifyReq {
                target: OrderRef::Oid(Oid(0)),
                order: dummy,
            }],
            [cancel()],
            2,
        ),
    }
}

fn cancel() -> CancelReq {
    CancelReq {
        asset: ASSET,
        target: OrderRef::Oid(Oid(1)),
    }
}

fn ua(l: &Leg) -> UserAction<'_> {
    let user = USERS[0];
    match l.3 {
        0 => UserAction::Order { user, orders: &l.0 },
        1 => UserAction::Modify {
            user,
            modifies: &l.1,
        },
        _ => UserAction::Cancel {
            user,
            cancels: &l.2,
        },
    }
}

fn orders() -> Vec<OrderReq> {
    let mut v = Vec::new();
    for side in [Side::Bid, Side::Ask] {
        for px in [1, 3] {
            for sz in [0, 2] {
                for tif in [Tif::Gtc, Tif::Alo, Tif::Ioc, Tif::Other] {
                    for pre_reject in [None, Some(RejectReason::TickSize)] {
                        v.push(OrderReq {
                            asset: ASSET,
                            side,
                            px: Px(px),
                            sz: Sz(sz),
                            reduce_only: false,
                            tif,
                            cloid: None,
                            priority: 0,
                            pre_reject,
                            oid_hint: None,
                        });
                    }
                }
            }
        }
    }
    v
}

fn actions() -> Vec<Act> {
    let os = orders();
    let mut v: Vec<Act> = os.iter().map(|&o| Act::Order(o)).collect();
    for t in [Oid(1), Oid(10)] {
        v.extend(os.iter().map(|&o| Act::Modify(t, o)));
    }
    v.push(Act::Cancel);
    v
}

fn snapshots() -> Vec<Vec<RestingOrder>> {
    let mut out = vec![vec![]];
    for user in USERS {
        for side in [Side::Bid, Side::Ask] {
            for px in [1, 3] {
                for sz in [1, 2] {
                    out.push(vec![RestingOrder {
                        asset: ASSET,
                        user,
                        oid: Oid(1),
                        side,
                        px: Px(px),
                        sz: Sz(sz),
                        cloid: None,
                        t_ms: 0,
                    }]);
                }
            }
        }
    }
    out
}

const HINTS: [Option<Oid>; 4] = [None, Some(Oid(10)), Some(Oid(11)), Some(Oid(12))];

fn strip(o: &BlockOutcome) -> BlockOutcome {
    let mut o = o.clone();
    o.oid_mismatches = 0;
    o
}

#[derive(Default)]
struct Tally {
    cases: u64,
    a_fail: Vec<String>,
    b_fail: Vec<String>,
    gap: u64,
    gap_example: Option<String>,
    covered_mismatch: u64,
    covered_match: u64,
}

fn run(snaps: &[Vec<RestingOrder>], acts: &[Act]) -> Tally {
    let mut t = Tally::default();
    for s in snaps {
        let base = Engine::from_l4_snapshot(
            &[ASSET],
            s.iter().copied(),
            Some(Oid(10)),
            cap(),
            EngineConfig::default(),
        )
        .expect("snapshot");
        for &a0 in acts {
            // Counter before leg 0 and whether it takes an oid.
            let c0 = base.next_oid();
            let mut e0 = base.clone();
            let l0 = leg(a0);
            e0.execute_block(1, &[ua(&l0)]);
            let c1 = e0.next_oid();
            let takes0 = c1 != c0;
            for &a1 in acts {
                let l1 = leg(a1);
                let mut plain = base.clone();
                plain.execute_block(1, &[ua(&l0), ua(&l1)]);
                let takes1 = plain.next_oid() != c1;
                let plain_out = plain.outcome().clone();
                let plain_view = engine_view(&plain);
                let h0s: &[Option<Oid>] = if matches!(a0, Act::Cancel) {
                    &HINTS[..1]
                } else {
                    &HINTS
                };
                let h1s: &[Option<Oid>] = if matches!(a1, Act::Cancel) {
                    &HINTS[..1]
                } else {
                    &HINTS
                };
                for &h0 in h0s {
                    for &h1 in h1s {
                        t.cases += 1;
                        let (m0, m1) = (a0.with_hint(h0), a1.with_hint(h1));
                        let (k0, k1) = (leg(m0), leg(m1));
                        let mut e = base.clone();
                        e.execute_block(1, &[ua(&k0), ua(&k1)]);
                        let out = e.outcome().clone();
                        let differs0 = h0.is_some_and(|h| h != c0);
                        let differs1 = h1.is_some_and(|h| h != c1);
                        let want = u32::from(takes0 && differs0) + u32::from(takes1 && differs1);
                        let all_agree = !differs0 && !differs1;
                        let ctx = || {
                            format!("snap={s:?}\n a0={m0:?} (counter {c0:?}, takes={takes0})\n a1={m1:?} (counter {c1:?}, takes={takes1})")
                        };
                        let a_bad = strip(&out) != strip(&plain_out)
                            || engine_view(&e) != plain_view
                            || (all_agree && out.oid_mismatches != 0);
                        if a_bad && t.a_fail.len() < 3 {
                            t.a_fail.push(format!(
                                "(a) {}\n mismatches={}",
                                ctx(),
                                out.oid_mismatches
                            ));
                        }
                        if out.oid_mismatches != want && t.b_fail.len() < 3 {
                            t.b_fail.push(format!(
                                "(b) {}\n mismatches={} want={want}",
                                ctx(),
                                out.oid_mismatches
                            ));
                        }
                        if (!takes0 && differs0) || (!takes1 && differs1) {
                            t.gap += 1;
                            if t.gap_example.is_none() && out.oid_mismatches == 0 {
                                t.gap_example = Some(ctx());
                            }
                        }
                        if want > 0 {
                            t.covered_mismatch += 1;
                        } else if h0.is_some() && takes0 {
                            t.covered_match += 1;
                        }
                    }
                }
            }
        }
    }
    t
}

#[test]
fn oid_hint_gate_exhaustive() {
    let snaps = snapshots();
    let acts = actions();
    let threads = std::thread::available_parallelism()
        .map_or(2, usize::from)
        .max(2);
    let chunk = snaps.len().div_ceil(threads);
    let ts: Vec<Tally> = std::thread::scope(|sc| {
        let hs: Vec<_> = snaps
            .chunks(chunk)
            .map(|ch| sc.spawn(|| run(ch, &acts)))
            .collect();
        hs.into_iter().map(|h| h.join().expect("worker")).collect()
    });
    let cases: u64 = ts.iter().map(|t| t.cases).sum();
    let a: Vec<&String> = ts.iter().flat_map(|t| &t.a_fail).collect();
    let b: Vec<&String> = ts.iter().flat_map(|t| &t.b_fail).collect();
    let gap: u64 = ts.iter().map(|t| t.gap).sum();
    let cm: u64 = ts.iter().map(|t| t.covered_mismatch).sum();
    let cok: u64 = ts.iter().map(|t| t.covered_match).sum();
    println!(
        "F3 oid hints: {} snapshots x {} actions^2 x hints = {cases} cases; (a) failures {}; (b) failures {}; F3-gap cases (differing hint on a leg that takes no oid, not counted) {gap}; cover: {cm} cases with a counted mismatch, {cok} with a correct hint on an oid-taking leg",
        snaps.len(),
        acts.len(),
        a.len(),
        b.len()
    );
    for x in a.iter().chain(b.iter()).take(3) {
        println!("{x}");
    }
    if let Some(g) = ts.iter().find_map(|t| t.gap_example.clone()) {
        println!("F3-gap example:\n{g}");
    }
    assert!(cm > 0 && cok > 0, "vacuous");
    assert!(a.is_empty(), "(a) failed");
    assert!(b.is_empty(), "(b) failed");
}
