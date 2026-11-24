//! Differential vectors printed by the Lean model `formal/lean/Model/HlBook.lean`
//! (`#eval` at the end: `VECTOR i: side px total count; ... | queue [oids]`),
//! replayed on the production `hl_exec::book::Book`. The Lean `take1` is one
//! fill (`match1`), so the taker size here is capped at the front maker's.
#![allow(clippy::unwrap_used, clippy::type_complexity, missing_docs)]

use hl_exec::book::{Book, NewOrder, Taker};
use hl_exec::{Oid, Px, Side, Sz, User};

enum Op {
    Rest(u64, bool, u64, u64),
    RestFront(u64, bool, u64, u64),
    Cancel(u64),
    Resize(u64, u64),
    Take1(bool, u64, u64),
}

fn side(b: bool) -> Side {
    if b {
        Side::Bid
    } else {
        Side::Ask
    }
}

fn slot(b: &Book, oid: u64) -> u32 {
    (0..64)
        .find(|&s| b.node(s).is_some_and(|n| n.oid == Oid(oid)))
        .unwrap()
}

fn run(ops: &[Op]) -> (Vec<(bool, u64, u64, u32)>, Vec<u64>) {
    let mut b = Book::with_capacity(0, 64, 16);
    let new = |oid, bid, px, sz| NewOrder {
        oid: Oid(oid),
        user: User([0; 20]),
        cloid: None,
        side: side(bid),
        px: Px(px),
        sz: Sz(sz),
        t_ms: 0,
        prio: 0,
    };
    for op in ops {
        match *op {
            Op::Rest(o, bid, px, sz) => {
                b.rest(new(o, bid, px, sz), None).unwrap();
            }
            Op::RestFront(o, bid, px, sz) => {
                // Lean `restAt 0`: in front of the whole queue; here the
                // level's front (same effect on the L2 view and its queue).
                let front = b.queue(side(bid), Px(px)).next().map(|n| n.oid.0);
                let anchor = front.map(|f| slot(&b, f));
                b.rest(new(o, bid, px, sz), anchor).unwrap();
            }
            Op::Cancel(o) => {
                b.remove(slot(&b, o)).unwrap();
            }
            Op::Resize(o, sz) => {
                b.resize(slot(&b, o), Sz(sz)).unwrap();
            }
            Op::Take1(bid, px, sz) => {
                let opp = side(bid).opposite();
                let best = b.best(opp).unwrap();
                let maker = b.queue(opp, best).next().unwrap().sz.0;
                let (mut d, mut f) = (Vec::new(), Vec::new());
                b.match_taker(
                    Taker {
                        oid: Oid(999),
                        user: User([1; 20]),
                        side: side(bid),
                        px: Px(px),
                        sz: Sz(sz.min(maker)),
                        stp: false,
                    },
                    &mut d,
                    &mut f,
                );
                assert_eq!(f.len(), 1);
            }
        }
    }
    assert!(b.check_invariants());
    let keys = [(true, 4), (true, 5), (false, 5), (false, 6)];
    let l2 = keys
        .iter()
        .map(|&(bid, px)| {
            b.l2(side(bid))
                .find(|l| l.0 == Px(px))
                .map_or((bid, px, 0, 0), |l| (bid, px, l.1 .0, l.2))
        })
        .collect();
    let mut q: Vec<u64> = Vec::new();
    for s in [Side::Bid, Side::Ask] {
        for px in b.levels(s).collect::<Vec<_>>() {
            q.extend(b.queue(s, px).map(|n| n.oid.0));
        }
    }
    (l2, q)
}

#[test]
fn lean_vectors_agree_with_the_rust_book() {
    use Op::{Cancel, Resize, Rest, RestFront, Take1};
    // (ops, expected L2 at (true,4) (true,5) (false,5) (false,6), queue oids)
    // copied from the Lean `#eval` output.
    let cases: Vec<(Vec<Op>, [(u64, u32); 4], Vec<u64>)> = vec![
        (
            vec![Rest(1, true, 5, 2), Rest(2, true, 5, 3), Cancel(1)],
            [(0, 0), (3, 1), (0, 0), (0, 0)],
            vec![2],
        ),
        (
            vec![
                Rest(1, false, 6, 2),
                Rest(2, false, 6, 3),
                Take1(true, 6, 4),
            ],
            [(0, 0), (0, 0), (0, 0), (3, 1)],
            vec![2],
        ),
        (
            vec![
                Rest(1, true, 5, 2),
                RestFront(2, true, 5, 3),
                Resize(1, 9),
                Take1(false, 5, 4),
            ],
            [(0, 0), (9, 1), (0, 0), (0, 0)],
            vec![1],
        ),
        (
            vec![
                Rest(1, false, 5, 2),
                Rest(2, false, 6, 3),
                Take1(true, 6, 4),
            ],
            [(0, 0), (0, 0), (0, 0), (3, 1)],
            vec![2],
        ),
        (
            vec![
                Rest(1, true, 4, 2),
                Rest(2, true, 5, 3),
                Take1(false, 4, 10),
            ],
            [(2, 1), (0, 0), (0, 0), (0, 0)],
            vec![1],
        ),
    ];
    let n = cases.len();
    let mut agree = 0;
    for (ops, l2, q) in cases {
        let (got, gq) = run(&ops);
        let got: Vec<(u64, u32)> = got.iter().map(|x| (x.2, x.3)).collect();
        assert_eq!(got, l2.to_vec());
        assert_eq!(gq, q);
        agree += 1;
    }
    assert_eq!(agree, n);
}
