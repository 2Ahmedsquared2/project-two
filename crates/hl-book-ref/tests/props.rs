//! Property tests: invariants hold under random op sequences.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_book_ref::{Book, BookOp, Lots, Oid, Order, Px, Side};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum RawOp {
    Insert { side: bool, px: u64, sz: u64 },
    Cancel { idx: usize },
    Resize { idx: usize, sz: u64 },
}

fn raw_op() -> impl Strategy<Value = RawOp> {
    prop_oneof![
        3 => (any::<bool>(), 90u64..110, 1u64..20)
            .prop_map(|(side, px, sz)| RawOp::Insert { side, px, sz }),
        1 => (0usize..64).prop_map(|idx| RawOp::Cancel { idx }),
        1 => (0usize..64, 1u64..20).prop_map(|(idx, sz)| RawOp::Resize { idx, sz }),
    ]
}

proptest! {
    #[test]
    fn invariants_hold(ops in prop::collection::vec(raw_op(), 0..200)) {
        let mut book = Book::new();
        let mut next_oid = 1u64;
        let mut live: Vec<Oid> = Vec::new();
        for op in ops {
            match op {
                RawOp::Insert { side, px, sz } => {
                    let side = if side { Side::Bid } else { Side::Ask };
                    let oid = Oid(next_oid);
                    next_oid += 1;
                    let order = Order { oid, side, px: Px(px), sz: Lots(sz) };
                    if book.apply(&BookOp::Insert(order)).is_ok() {
                        live.push(oid);
                    }
                }
                RawOp::Cancel { idx } => {
                    if !live.is_empty() {
                        let oid = live.swap_remove(idx % live.len());
                        book.apply(&BookOp::Cancel { oid }).unwrap();
                    }
                }
                RawOp::Resize { idx, sz } => {
                    if !live.is_empty() {
                        let oid = live[idx % live.len()];
                        book.apply(&BookOp::Resize { oid, sz: Lots(sz) }).unwrap();
                    }
                }
            }
            let snap = book.snapshot();
            // Every live order is in exactly one level; the index agrees with the levels.
            let in_levels: usize = snap
                .bids
                .iter()
                .chain(snap.asks.iter())
                .map(|l| l.orders.len())
                .sum();
            prop_assert_eq!(in_levels, book.order_count());
            prop_assert_eq!(in_levels, live.len());
            // No empty levels, no zero sizes.
            for l in snap.bids.iter().chain(snap.asks.iter()) {
                prop_assert!(!l.orders.is_empty());
                prop_assert!(l.orders.iter().all(|(_, sz)| sz.0 > 0));
            }
            // Bids strictly descending, asks strictly ascending, never crossed.
            prop_assert!(snap.bids.windows(2).all(|w| w[0].px > w[1].px));
            prop_assert!(snap.asks.windows(2).all(|w| w[0].px < w[1].px));
            if let (Some(b), Some(a)) = (book.best_bid(), book.best_ask()) {
                prop_assert!(b < a);
            }
            // Snapshot round trip is lossless.
            prop_assert_eq!(Book::from_snapshot(&snap).unwrap().snapshot(), snap);
        }
    }
}

fn tif(i: u8) -> hl_book_ref::Tif {
    match i % 3 {
        0 => hl_book_ref::Tif::Gtc,
        1 => hl_book_ref::Tif::Ioc,
        _ => hl_book_ref::Tif::Alo,
    }
}

proptest! {
    /// Matching conserves size, never leaves the book crossed, and L2 agrees with the L4 snapshot.
    #[test]
    fn matching_invariants_hold(
        orders in prop::collection::vec((any::<bool>(), 90u64..110, 1u64..20, any::<u8>()), 0..200)
    ) {
        let mut book = Book::new();
        for (i, (side, px, sz, t)) in orders.into_iter().enumerate() {
            let side = if side { Side::Bid } else { Side::Ask };
            let order = Order { oid: Oid(u64::try_from(i).unwrap() + 1), side, px: Px(px), sz: Lots(sz) };
            let opposite_before: u64 = book.l2(None).asks.iter().chain(book.l2(None).bids.iter())
                .map(|l| l.sz.0).sum();
            let tif = tif(t);
            match book.place(order, tif) {
                Ok(placed) => {
                    let filled: u64 = placed.fills.iter().map(|f| f.sz.0).sum();
                    let rested = placed.rested.map_or(0, |l| l.0);
                    prop_assert!(filled + rested <= sz);
                    if tif == hl_book_ref::Tif::Gtc {
                        prop_assert_eq!(filled + rested, sz);
                    }
                    // Every fill is at a price no worse than the limit.
                    for f in &placed.fills {
                        let ok = match side { Side::Bid => f.px <= Px(px), Side::Ask => f.px >= Px(px) };
                        prop_assert!(ok);
                    }
                    let after: u64 = book.l2(None).asks.iter().chain(book.l2(None).bids.iter())
                        .map(|l| l.sz.0).sum();
                    prop_assert_eq!(after + filled, opposite_before + rested);
                }
                Err(e) => {
                    let alo = matches!(e, hl_book_ref::ApplyError::AloWouldCross { .. });
                    prop_assert!(alo);
                }
            }
            if let (Some(b), Some(a)) = (book.best_bid(), book.best_ask()) {
                prop_assert!(b < a);
            }
            prop_assert_eq!(book.l2(None), book.snapshot().to_l2(None));
        }
    }
}
