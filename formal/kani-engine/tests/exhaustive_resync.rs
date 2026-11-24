//! Exhaustive bounded check (EVIDENCE, not a Kani proof; Kani cannot yet
//! verify `rollback`, see README) that
//! `Engine::resync(&[])` after `execute_block` restores the pre-block engine,
//! and that the oid / cloid indexes stay consistent across the block.
//!
//! Why not Kani: Kani 0.68.0 (and 0.68.0 + upstream fix #5066) runs CBMC out
//! of memory on `Book::rollback` undoing a single journal entry (see
//! `formal/kani-engine/README.md`). Until that is fixed, this test enumerates
//! EVERY case of the bounded domain below, so within it the check is
//! complete; it says nothing outside it.
//!
//! Domain: one asset; snapshot of 0, 1 or 2 resting orders (oids 1, 2), each
//! side {bid, ask} x price {1,2,3} x size {1,2} x user {u1,u2} x cloid
//! {none, c1, c2} (snapshots the loader refuses, and two orders of one user
//! with one cloid, are skipped); next oid 10; one block of ONE single-leg
//! action from {u1,u2}: order (side x price x size {1,2} x TIF {GTC,ALO,IOC}
//! x cloid x priority {0,1}), cancel or modify (target oid 1/2/10/11 or
//! cloid c1/c2; modify's new order from the same order domain); STP on/off.
//!
//! Run: `cargo test --release -p kani-engine --test exhaustive_resync -- --nocapture`

use hl_exec::{
    CancelReq, Capacity, Cloid, Engine, EngineConfig, ModifyReq, Oid, OrderRef, OrderReq, Px,
    RestingOrder, Side, Sz, Tif, User, UserAction,
};
use kani_engine::model::{engine_view, index_consistent, ASSET, CLOIDS, USERS};

const SIDES: [Side; 2] = [Side::Bid, Side::Ask];
const PXS: [u64; 3] = [1, 2, 3];
const SZS: [u64; 2] = [1, 2];
const CL: [Option<Cloid>; 3] = [None, Some(CLOIDS[0]), Some(CLOIDS[1])];

fn cap() -> Capacity {
    Capacity {
        orders: 4,
        levels_per_side: 3,
        actions_per_block: 2,
        events_per_block: 8,
    }
}

fn resting() -> Vec<(User, Side, u64, u64, Option<Cloid>)> {
    let mut v = Vec::new();
    for u in USERS {
        for s in SIDES {
            for p in PXS {
                for z in SZS {
                    for c in CL {
                        v.push((u, s, p, z, c));
                    }
                }
            }
        }
    }
    v
}

fn snapshots() -> Vec<Vec<RestingOrder>> {
    let r = resting();
    let mk =
        |oid, (user, side, px, sz, cloid): (User, Side, u64, u64, Option<Cloid>)| RestingOrder {
            asset: ASSET,
            user,
            oid: Oid(oid),
            side,
            px: Px(px),
            sz: Sz(sz),
            cloid,
            t_ms: 0,
        };
    let mut out = vec![vec![]];
    for &a in &r {
        out.push(vec![mk(1, a)]);
        for &b in &r {
            if a.4.is_some() && a.4 == b.4 && a.0 == b.0 {
                continue;
            }
            out.push(vec![mk(1, a), mk(2, b)]);
        }
    }
    out
}

fn orders() -> Vec<OrderReq> {
    let mut v = Vec::new();
    for side in SIDES {
        for p in PXS {
            for z in SZS {
                for tif in [Tif::Gtc, Tif::Alo, Tif::Ioc] {
                    for cloid in CL {
                        for priority in [0, 1] {
                            v.push(OrderReq {
                                asset: ASSET,
                                side,
                                px: Px(p),
                                sz: Sz(z),
                                reduce_only: false,
                                tif,
                                cloid,
                                priority,
                                pre_reject: None,
                                oid_hint: None,
                            });
                        }
                    }
                }
            }
        }
    }
    v
}

const REFS: [OrderRef; 6] = [
    OrderRef::Oid(Oid(1)),
    OrderRef::Oid(Oid(2)),
    OrderRef::Oid(Oid(10)),
    OrderRef::Oid(Oid(11)),
    OrderRef::Cloid(CLOIDS[0]),
    OrderRef::Cloid(CLOIDS[1]),
];

#[derive(Clone, Copy)]
enum Act {
    Order(OrderReq),
    Cancel(OrderRef),
    Modify(OrderRef, OrderReq),
}

fn actions() -> Vec<Act> {
    let os = orders();
    let mut v: Vec<Act> = os.iter().map(|&o| Act::Order(o)).collect();
    for r in REFS {
        v.push(Act::Cancel(r));
        for &o in &os {
            v.push(Act::Modify(r, o));
        }
    }
    v
}

/// The action places an order whose cloid is already live for the same user
/// (and, for a modify, the target is not that order): bug class B1, fixed by
/// lane 1 in ff7821e. Counted apart so a regression is named.
fn reuses_live_cloid(s: &[RestingOrder], a: Act, user: User) -> bool {
    let (req, target) = match a {
        Act::Order(r) => (r, None),
        Act::Modify(t, r) => (r, Some(t)),
        Act::Cancel(_) => return false,
    };
    let Some(c) = req.cloid else { return false };
    s.iter().any(|o| {
        o.user == user
            && o.cloid == Some(c)
            && target != Some(OrderRef::Oid(o.oid))
            && target != Some(OrderRef::Cloid(c))
    })
}

/// Run every case of the domain; returns (cases, failures as text, B1 failures).
fn check(stp: bool, snaps: &[Vec<RestingOrder>], acts: &[Act]) -> (u64, Vec<String>, u64) {
    let cfg = EngineConfig {
        stp,
        alo_priority_window_ms: 400,
    };
    let mut n = 0u64;
    let mut bad = Vec::new();
    let mut b1 = 0u64;
    for s in snaps {
        let Ok(base) =
            Engine::from_l4_snapshot(&[ASSET], s.iter().copied(), Some(Oid(10)), cap(), cfg)
        else {
            continue;
        };
        assert!(index_consistent(&base), "snapshot index: {s:?}");
        let pre = engine_view(&base);
        for &a in acts {
            for user in USERS {
                let o;
                let c;
                let m;
                let ua = match a {
                    Act::Order(r) => {
                        o = [r];
                        UserAction::Order { user, orders: &o }
                    }
                    Act::Cancel(t) => {
                        c = [CancelReq {
                            asset: ASSET,
                            target: t,
                        }];
                        UserAction::Cancel { user, cancels: &c }
                    }
                    Act::Modify(t, r) => {
                        m = [ModifyReq {
                            target: t,
                            order: r,
                        }];
                        UserAction::Modify { user, modifies: &m }
                    }
                };
                let mut e = base.clone();
                e.execute_block(1, &[ua]);
                n += 1;
                let idx_ok = index_consistent(&e);
                let r = e.resync(&[]);
                let post = engine_view(&e);
                if !idx_ok || r.is_err() || post != pre || !e.check_invariants() {
                    if reuses_live_cloid(s, a, user) {
                        b1 += 1;
                    } else if bad.len() < 5 {
                        bad.push(format!(
                            "stp={stp} snap={s:?} action={ua:?} index_ok={idx_ok} resync={r:?}\n pre={pre:?}\n post={post:?}"
                        ));
                    }
                }
            }
        }
    }
    (n, bad, b1)
}

#[test]
fn resync_empty_restores_pre_block_exhaustive() {
    let snaps = snapshots();
    let acts = actions();
    let threads = std::thread::available_parallelism()
        .map_or(2, usize::from)
        .max(2);
    let chunk = snaps.len().div_ceil(threads);
    let results: Vec<(u64, Vec<String>, u64)> = std::thread::scope(|sc| {
        let hs: Vec<_> = snaps
            .chunks(chunk)
            .flat_map(|ch| {
                let acts = &acts;
                [true, false].map(move |stp| sc.spawn(move || check(stp, ch, acts)))
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("worker")).collect()
    });
    let cases: u64 = results.iter().map(|r| r.0).sum();
    let bad: Vec<&String> = results.iter().flat_map(|r| r.1.iter()).collect();
    let b1: u64 = results.iter().map(|r| r.2).sum();
    println!(
        "exhaustive resync(&[]): {} snapshots x {} actions x 2 users x STP on/off = {cases} cases; {b1} failures in class B1 (live cloid reused; fixed in ff7821e); {} other failures",
        snaps.len(),
        acts.len(),
        bad.len()
    );
    for b in bad.iter().take(3) {
        println!("{b}");
    }
    assert!(bad.is_empty() && b1 == 0);
}
