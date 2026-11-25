//! Apply cost, reference book (`hl-book-ref`, the executable spec) vs the
//! `hl-exec` book on the same op stream, and `execute_block` on a block the
//! size of our node's mean (1,858,991 actions / 2,000 blocks ~ 930).
//!
//! ```text
//! cargo bench -p hl-exec --bench exec
//! ```
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::too_many_lines
)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use hl_book_ref as r;
use hl_exec::book::{Book, NewOrder};
use hl_exec::{
    CancelReq, Capacity, Cloid, Engine, EngineConfig, Oid, OrderRef, OrderReq, Px, Side, Sz, Tif,
    User, UserAction,
};

/// Deterministic xorshift.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[derive(Clone, Copy)]
enum Op {
    Insert(u64, bool, u64, u64),
    Cancel(u64),
    Resize(u64, u64),
}

/// A book of ~2,000 resting orders over 200 levels, then `n` ops that keep it
/// that size: insert near the touch, cancel a live order, resize a live order.
fn stream(n: usize) -> (Vec<Op>, Vec<Op>) {
    let mut g = Rng(0x9E37_79B9_7F4A_7C15);
    let mut live: Vec<u64> = Vec::new();
    let mut next = 1u64;
    let ins = |g: &mut Rng, live: &mut Vec<u64>, next: &mut u64| {
        let bid = g.next().is_multiple_of(2);
        let px = if bid {
            900 + g.next() % 100
        } else {
            1001 + g.next() % 100
        };
        let o = *next;
        *next += 1;
        live.push(o);
        Op::Insert(o, bid, px, 1 + g.next() % 50)
    };
    let warm: Vec<Op> = (0..2000)
        .map(|_| ins(&mut g, &mut live, &mut next))
        .collect();
    let mut ops = Vec::with_capacity(n);
    for _ in 0..n {
        let k = g.next() % 10;
        if k < 4 {
            ops.push(ins(&mut g, &mut live, &mut next));
        } else if k < 8 {
            let i = (g.next() as usize) % live.len();
            ops.push(Op::Cancel(live.swap_remove(i)));
        } else {
            let i = (g.next() as usize) % live.len();
            ops.push(Op::Resize(live[i], 1 + g.next() % 50));
        }
    }
    (warm, ops)
}

fn ref_apply(b: &mut r::Book, op: Op) {
    let o = match op {
        Op::Insert(oid, bid, px, sz) => r::BookOp::Insert(r::Order {
            oid: r::Oid(oid),
            side: if bid { r::Side::Bid } else { r::Side::Ask },
            px: r::Px(px),
            sz: r::Lots(sz),
        }),
        Op::Cancel(oid) => r::BookOp::Cancel { oid: r::Oid(oid) },
        Op::Resize(oid, sz) => r::BookOp::Resize {
            oid: r::Oid(oid),
            sz: r::Lots(sz),
        },
    };
    b.apply(&o).unwrap();
}

/// hl-exec book: the slot of an oid comes from a dense table (the engine's
/// index is a hash map; this isolates the book).
fn ex_apply(b: &mut Book, slots: &mut [u32], op: Op) {
    match op {
        Op::Insert(oid, bid, px, sz) => {
            let s = b
                .rest(
                    NewOrder {
                        oid: Oid(oid),
                        user: User([0; 20]),
                        cloid: None,
                        side: if bid { Side::Bid } else { Side::Ask },
                        px: Px(px),
                        sz: Sz(sz),
                        t_ms: 0,
                        prio: 0,
                    },
                    None,
                )
                .unwrap();
            slots[oid as usize] = s;
        }
        Op::Cancel(oid) => {
            b.remove(slots[oid as usize]).unwrap();
        }
        Op::Resize(oid, sz) => {
            b.resize(slots[oid as usize], Sz(sz)).unwrap();
        }
    }
}

fn per_op(c: &mut Criterion) {
    let n = 10_000;
    let (warm, ops) = stream(n);
    let max_oid = 2000 + n + 1;
    let mut g = c.benchmark_group("book_ops_10k");
    g.throughput(Throughput::Elements(n as u64));
    g.bench_function("reference_hl_book_ref", |bch| {
        bch.iter_batched(
            || {
                let mut b = r::Book::new();
                for &o in &warm {
                    ref_apply(&mut b, o);
                }
                b
            },
            |mut b| {
                for &o in &ops {
                    ref_apply(&mut b, o);
                }
                black_box(b)
            },
            BatchSize::LargeInput,
        );
    });
    g.bench_function("hl_exec_book", |bch| {
        bch.iter_batched(
            || {
                let mut b = Book::with_capacity(0, 8192, 256);
                let mut s = vec![0u32; max_oid];
                for &o in &warm {
                    ex_apply(&mut b, &mut s, o);
                }
                (b, s)
            },
            |(mut b, mut s)| {
                for &o in &ops {
                    ex_apply(&mut b, &mut s, o);
                }
                black_box(b)
            },
            BatchSize::LargeInput,
        );
    });
    g.finish();
}

fn req(side: Side, px: u64, sz: u64, tif: Tif, cloid: u128) -> OrderReq {
    OrderReq {
        asset: 0,
        side,
        px: Px(px),
        sz: Sz(sz),
        reduce_only: false,
        tif,
        cloid: Some(Cloid(cloid)),
        priority: 0,
        pre_reject: None,
        oid_hint: None,
    }
}

/// ~930 actions: quote refreshes (ALO legs), cancels by cloid, a few takers.
fn block(c: &mut Criterion) {
    let cap = Capacity {
        orders: 1 << 15,
        levels_per_side: 512,
        actions_per_block: 2048,
        events_per_block: 1 << 14,
    };
    let mut e =
        Engine::from_l4_snapshot(&[0], [], Some(Oid(1)), cap, EngineConfig::default()).unwrap();
    let mm = User([1; 20]);
    let tk = User([2; 20]);
    let mut tick: u128 = 0;
    let mut quotes: Vec<Vec<OrderReq>> = Vec::new();
    let mut cancels: Vec<Vec<CancelReq>> = Vec::new();
    let mut takes: Vec<OrderReq> = Vec::new();
    let build = |tick: u128,
                 quotes: &mut Vec<Vec<OrderReq>>,
                 cancels: &mut Vec<Vec<CancelReq>>,
                 takes: &mut Vec<OrderReq>| {
        quotes.clear();
        cancels.clear();
        takes.clear();
        let mid = 10_000 + u64::try_from(tick % 13).unwrap();
        for a in 0..300u128 {
            let k = u64::try_from(a % 20).unwrap();
            quotes.push(vec![
                req(Side::Bid, mid - 1 - k, 5, Tif::Alo, tick * 1000 + a * 2),
                req(Side::Ask, mid + 1 + k, 5, Tif::Alo, tick * 1000 + a * 2 + 1),
            ]);
            if tick > 0 {
                cancels.push(vec![
                    CancelReq {
                        asset: 0,
                        target: OrderRef::Cloid(Cloid((tick - 1) * 1000 + a * 2)),
                    },
                    CancelReq {
                        asset: 0,
                        target: OrderRef::Cloid(Cloid((tick - 1) * 1000 + a * 2 + 1)),
                    },
                ]);
            }
        }
        for a in 0..20u64 {
            takes.push(req(
                if a.is_multiple_of(2) {
                    Side::Bid
                } else {
                    Side::Ask
                },
                if a.is_multiple_of(2) {
                    mid + 3
                } else {
                    mid - 3
                },
                7,
                Tif::Ioc,
                0,
            ));
        }
    };
    let mut g = c.benchmark_group("execute_block");
    g.bench_function("hl_exec_930_actions", |bch| {
        bch.iter(|| {
            build(tick, &mut quotes, &mut cancels, &mut takes);
            let mut acts: Vec<UserAction<'_>> = Vec::with_capacity(1024);
            for q in &quotes {
                acts.push(UserAction::Order {
                    user: mm,
                    orders: q,
                });
            }
            for cn in &cancels {
                acts.push(UserAction::Cancel {
                    user: mm,
                    cancels: cn,
                });
            }
            for t in takes.chunks(1) {
                acts.push(UserAction::Order {
                    user: tk,
                    orders: t,
                });
            }
            let out = e.execute_block(u64::try_from(tick).unwrap() * 70, &acts);
            tick += 1;
            black_box(out.diffs.len())
        });
    });
    g.finish();
}

criterion_group!(benches, per_op, block);
criterion_main!(benches);
