//! Hyperliquid Brief #1 additions: `insert_before`, L2 aggregation, and replay
//! matching (GTC rests, IOC never rests, ALO rejected if it would cross).
//! Written before the implementation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_book_ref::{
    ApplyError, Book, BookOp, Fill, L2Level, Lots, Oid, Order, Placed, Px, Side, Tif,
};

fn order(oid: u64, side: Side, px: u64, sz: u64) -> Order {
    Order {
        oid: Oid(oid),
        side,
        px: Px(px),
        sz: Lots(sz),
    }
}

fn book_with(orders: &[Order]) -> Book {
    let mut b = Book::new();
    for &o in orders {
        b.apply(&BookOp::Insert(o)).unwrap();
    }
    b
}

fn queue(b: &Book, side: Side, px: u64) -> Vec<u64> {
    b.level(side, Px(px))
        .map(|l| l.orders.iter().map(|(o, _)| o.0).collect())
        .unwrap_or_default()
}

// ---- insert_before ----

#[test]
fn insert_before_places_the_order_ahead_of_the_named_one() {
    let mut b = book_with(&[order(1, Side::Bid, 100, 5), order(2, Side::Bid, 100, 5)]);
    b.apply(&BookOp::InsertBefore {
        before: Oid(2),
        order: order(3, Side::Bid, 100, 1),
    })
    .unwrap();
    assert_eq!(queue(&b, Side::Bid, 100), vec![1, 3, 2]);
    // Cancel and resize still find it through the index.
    b.apply(&BookOp::Resize {
        oid: Oid(3),
        sz: Lots(4),
    })
    .unwrap();
    assert_eq!(b.get(Oid(3)).unwrap().sz, Lots(4));
    b.apply(&BookOp::Cancel { oid: Oid(3) }).unwrap();
    assert_eq!(queue(&b, Side::Bid, 100), vec![1, 2]);
}

#[test]
fn insert_before_rejects_an_unknown_or_mismatched_anchor_and_leaves_the_book_unchanged() {
    let mut b = book_with(&[order(1, Side::Bid, 100, 5), order(2, Side::Ask, 101, 5)]);
    let before = b.snapshot();
    let err = b
        .apply(&BookOp::InsertBefore {
            before: Oid(9),
            order: order(3, Side::Bid, 100, 1),
        })
        .unwrap_err();
    assert_eq!(err, ApplyError::UnknownOid(Oid(9)));
    // Anchor at a different price.
    let err = b
        .apply(&BookOp::InsertBefore {
            before: Oid(1),
            order: order(3, Side::Bid, 99, 1),
        })
        .unwrap_err();
    assert_eq!(
        err,
        ApplyError::AnchorMismatch {
            oid: Oid(3),
            before: Oid(1)
        }
    );
    // Anchor on the other side.
    let err = b
        .apply(&BookOp::InsertBefore {
            before: Oid(2),
            order: order(3, Side::Bid, 101, 1),
        })
        .unwrap_err();
    assert_eq!(
        err,
        ApplyError::AnchorMismatch {
            oid: Oid(3),
            before: Oid(2)
        }
    );
    assert_eq!(b.snapshot(), before);
}

#[test]
fn insert_before_wire_shape() {
    let op = BookOp::InsertBefore {
        before: Oid(2),
        order: order(3, Side::Bid, 100, 1),
    };
    let json = serde_json::to_string(&op).unwrap();
    assert_eq!(
        json,
        r#"{"op":"insert_before","before":2,"order":{"oid":3,"side":"bid","px":100,"sz":1}}"#
    );
    assert_eq!(serde_json::from_str::<BookOp>(&json).unwrap(), op);
}

// ---- L2 aggregation ----

#[test]
fn l2_aggregates_size_and_order_count_per_level_best_first() {
    let b = book_with(&[
        order(1, Side::Bid, 100, 5),
        order(2, Side::Bid, 100, 7),
        order(3, Side::Bid, 98, 1),
        order(4, Side::Bid, 99, 2),
        order(5, Side::Ask, 102, 3),
        order(6, Side::Ask, 101, 4),
    ]);
    let l2 = b.l2(None).unwrap();
    assert_eq!(
        l2.bids,
        vec![
            L2Level {
                px: Px(100),
                sz: Lots(12),
                n: 2
            },
            L2Level {
                px: Px(99),
                sz: Lots(2),
                n: 1
            },
            L2Level {
                px: Px(98),
                sz: Lots(1),
                n: 1
            },
        ]
    );
    assert_eq!(
        l2.asks,
        vec![
            L2Level {
                px: Px(101),
                sz: Lots(4),
                n: 1
            },
            L2Level {
                px: Px(102),
                sz: Lots(3),
                n: 1
            },
        ]
    );
}

#[test]
fn l2_top_n_truncates_each_side_independently() {
    let b = book_with(&[
        order(1, Side::Bid, 100, 5),
        order(2, Side::Bid, 99, 7),
        order(3, Side::Ask, 101, 1),
    ]);
    let l2 = b.l2(Some(1)).unwrap();
    assert_eq!(l2.bids.len(), 1);
    assert_eq!(l2.bids[0].px, Px(100));
    assert_eq!(l2.asks.len(), 1);
    assert_eq!(b.snapshot().to_l2(Some(1)).unwrap(), l2);
}

// ---- matching ----

#[test]
fn a_gtc_taker_fills_best_price_first_fifo_at_the_resting_price_then_rests() {
    // Asks: 101 [1x3, 2x4], 102 [3x5]. Buy 10 @ 102 GTC.
    let mut b = book_with(&[
        order(1, Side::Ask, 101, 3),
        order(2, Side::Ask, 101, 4),
        order(3, Side::Ask, 102, 5),
        order(9, Side::Bid, 99, 1),
    ]);
    let placed = b.place(order(10, Side::Bid, 102, 10), Tif::Gtc).unwrap();
    assert_eq!(
        placed,
        Placed {
            fills: vec![
                Fill {
                    maker: Oid(1),
                    px: Px(101),
                    sz: Lots(3)
                },
                Fill {
                    maker: Oid(2),
                    px: Px(101),
                    sz: Lots(4)
                },
                Fill {
                    maker: Oid(3),
                    px: Px(102),
                    sz: Lots(3)
                },
            ],
            rested: None,
            self_trade_cancels: vec![],
        }
    );
    // Maker 3 partially filled and keeps its place; 101 level gone.
    assert_eq!(b.best_ask(), Some(Px(102)));
    assert_eq!(b.get(Oid(3)).unwrap().sz, Lots(2));
    assert!(b.get(Oid(10)).is_none());

    // Now a GTC that more than clears the book: remainder rests.
    let placed = b.place(order(11, Side::Bid, 102, 5), Tif::Gtc).unwrap();
    assert_eq!(placed.fills.len(), 1);
    assert_eq!(placed.rested, Some(Lots(3)));
    assert_eq!(b.best_bid(), Some(Px(102)));
    assert_eq!(b.get(Oid(11)).unwrap().sz, Lots(3));
    assert_eq!(b.best_ask(), None);
}

#[test]
fn an_ioc_never_rests() {
    let mut b = book_with(&[order(1, Side::Bid, 100, 2)]);
    let placed = b.place(order(5, Side::Ask, 100, 5), Tif::Ioc).unwrap();
    assert_eq!(
        placed.fills,
        vec![Fill {
            maker: Oid(1),
            px: Px(100),
            sz: Lots(2)
        }]
    );
    assert_eq!(placed.rested, None);
    assert_eq!(b.order_count(), 0);
    // A non-crossing IOC does nothing.
    let placed = b.place(order(6, Side::Ask, 100, 5), Tif::Ioc).unwrap();
    assert_eq!(placed, Placed::default());
    assert_eq!(b.order_count(), 0);
}

#[test]
fn an_alo_that_would_cross_is_rejected_and_the_book_is_unchanged() {
    let mut b = book_with(&[order(1, Side::Ask, 101, 2)]);
    let before = b.snapshot();
    let err = b.place(order(5, Side::Bid, 101, 1), Tif::Alo).unwrap_err();
    assert_eq!(
        err,
        ApplyError::AloWouldCross {
            oid: Oid(5),
            px: Px(101),
            opposite_best: Px(101)
        }
    );
    assert_eq!(b.snapshot(), before);
    // A non-crossing ALO rests.
    let placed = b.place(order(6, Side::Bid, 100, 1), Tif::Alo).unwrap();
    assert_eq!(placed.rested, Some(Lots(1)));
    assert_eq!(b.best_bid(), Some(Px(100)));
}

#[test]
fn place_validates_like_insert() {
    let mut b = book_with(&[order(1, Side::Bid, 100, 2)]);
    assert_eq!(
        b.place(order(1, Side::Bid, 99, 1), Tif::Gtc).unwrap_err(),
        ApplyError::DuplicateOid(Oid(1))
    );
    assert_eq!(
        b.place(order(2, Side::Bid, 99, 0), Tif::Gtc).unwrap_err(),
        ApplyError::ZeroSize(Oid(2))
    );
}
