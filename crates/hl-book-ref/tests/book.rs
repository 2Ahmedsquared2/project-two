//! Behavioural tests for the reference book. Written before the implementation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_book_ref::{ApplyError, Book, BookOp, Level, Lots, Oid, Order, Px, Side};

fn order(oid: u64, side: Side, px: u64, sz: u64) -> Order {
    Order {
        oid: Oid(oid),
        side,
        px: Px(px),
        sz: Lots(sz),
    }
}

#[test]
fn inserts_rest_fifo_within_a_level() {
    let mut book = Book::new();
    book.apply(&BookOp::Insert(order(1, Side::Bid, 100, 5)))
        .unwrap();
    book.apply(&BookOp::Insert(order(2, Side::Bid, 100, 7)))
        .unwrap();
    let level = book.level(Side::Bid, Px(100)).expect("level exists");
    assert_eq!(level.orders, vec![(Oid(1), Lots(5)), (Oid(2), Lots(7))]);
    assert_eq!(level.total(), Some(Lots(12)));
    assert_eq!(book.order_count(), 2);
}

#[test]
fn snapshot_orders_bids_descending_and_asks_ascending() {
    let mut book = Book::new();
    for (oid, side, px) in [
        (1, Side::Bid, 98),
        (2, Side::Bid, 100),
        (3, Side::Bid, 99),
        (4, Side::Ask, 103),
        (5, Side::Ask, 101),
        (6, Side::Ask, 102),
    ] {
        book.apply(&BookOp::Insert(order(oid, side, px, 1)))
            .unwrap();
    }
    let snap = book.snapshot();
    let bid_pxs: Vec<u64> = snap.bids.iter().map(|l| l.px.0).collect();
    let ask_pxs: Vec<u64> = snap.asks.iter().map(|l| l.px.0).collect();
    assert_eq!(bid_pxs, vec![100, 99, 98]);
    assert_eq!(ask_pxs, vec![101, 102, 103]);
    assert_eq!(book.best_bid(), Some(Px(100)));
    assert_eq!(book.best_ask(), Some(Px(101)));
}

#[test]
fn cancel_removes_the_order_and_empties_the_level() {
    let mut book = Book::new();
    book.apply(&BookOp::Insert(order(1, Side::Ask, 200, 3)))
        .unwrap();
    book.apply(&BookOp::Cancel { oid: Oid(1) }).unwrap();
    assert!(book.level(Side::Ask, Px(200)).is_none());
    assert_eq!(book.order_count(), 0);
    assert_eq!(book.best_ask(), None);
    assert_eq!(
        book.apply(&BookOp::Cancel { oid: Oid(1) }),
        Err(ApplyError::UnknownOid(Oid(1)))
    );
}

#[test]
fn duplicate_oid_is_rejected_and_leaves_the_book_unchanged() {
    let mut book = Book::new();
    book.apply(&BookOp::Insert(order(1, Side::Bid, 100, 5)))
        .unwrap();
    let before = book.snapshot();
    assert_eq!(
        book.apply(&BookOp::Insert(order(1, Side::Ask, 300, 5))),
        Err(ApplyError::DuplicateOid(Oid(1)))
    );
    assert_eq!(book.snapshot(), before);
}

#[test]
fn resize_keeps_queue_position() {
    let mut book = Book::new();
    book.apply(&BookOp::Insert(order(1, Side::Bid, 100, 5)))
        .unwrap();
    book.apply(&BookOp::Insert(order(2, Side::Bid, 100, 7)))
        .unwrap();
    book.apply(&BookOp::Resize {
        oid: Oid(1),
        sz: Lots(2),
    })
    .unwrap();
    let level = book.level(Side::Bid, Px(100)).unwrap();
    assert_eq!(level.orders, vec![(Oid(1), Lots(2)), (Oid(2), Lots(7))]);
    assert_eq!(
        book.apply(&BookOp::Resize {
            oid: Oid(1),
            sz: Lots(0)
        }),
        Err(ApplyError::ZeroSize(Oid(1)))
    );
    assert_eq!(
        book.apply(&BookOp::Resize {
            oid: Oid(9),
            sz: Lots(1)
        }),
        Err(ApplyError::UnknownOid(Oid(9)))
    );
}

#[test]
fn zero_size_insert_is_rejected() {
    let mut book = Book::new();
    assert_eq!(
        book.apply(&BookOp::Insert(order(1, Side::Bid, 100, 0))),
        Err(ApplyError::ZeroSize(Oid(1)))
    );
}

#[test]
fn a_resting_insert_may_not_cross_the_book() {
    let mut book = Book::new();
    book.apply(&BookOp::Insert(order(1, Side::Ask, 101, 5)))
        .unwrap();
    assert_eq!(
        book.apply(&BookOp::Insert(order(2, Side::Bid, 101, 5))),
        Err(ApplyError::Crossed {
            oid: Oid(2),
            px: Px(101),
            opposite_best: Px(101),
        })
    );
    assert_eq!(book.order_count(), 1);
    book.apply(&BookOp::Insert(order(3, Side::Bid, 100, 5)))
        .unwrap();
}

#[test]
fn snapshot_round_trips_through_from_snapshot() {
    let mut book = Book::new();
    book.apply(&BookOp::Insert(order(1, Side::Bid, 100, 5)))
        .unwrap();
    book.apply(&BookOp::Insert(order(2, Side::Bid, 100, 7)))
        .unwrap();
    book.apply(&BookOp::Insert(order(3, Side::Ask, 105, 1)))
        .unwrap();
    let snap = book.snapshot();
    let rebuilt = Book::from_snapshot(&snap).unwrap();
    assert_eq!(rebuilt.snapshot(), snap);
    assert_eq!(rebuilt.get(Oid(2)), Some(&order(2, Side::Bid, 100, 7)));
    assert_eq!(
        Level {
            px: Px(100),
            orders: vec![(Oid(1), Lots(5)), (Oid(2), Lots(7))]
        },
        snap.bids[0]
    );
}
