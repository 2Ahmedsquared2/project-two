//! CI/CD review of PR #1 (item 2): a level's total size must stay
//! representable in u64. Written before the fix.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_book_ref::{Book, BookOp, Lots, Oid, Order, Px, Side, Tif};

fn order(oid: u64, side: Side, px: u64, sz: u64) -> Order {
    Order {
        oid: Oid(oid),
        side,
        px: Px(px),
        sz: Lots(sz),
    }
}

#[test]
fn an_insert_that_would_overflow_its_level_is_rejected_and_changes_nothing() {
    let mut b = Book::new();
    b.apply(&BookOp::Insert(order(1, Side::Bid, 100, u64::MAX)))
        .unwrap();
    let before = b.snapshot();
    let r = b.apply(&BookOp::Insert(order(2, Side::Bid, 100, 1)));
    assert!(r.is_err(), "{r:?}");
    assert_eq!(b.snapshot(), before);
    let r = b.apply(&BookOp::InsertBefore {
        before: Oid(1),
        order: order(3, Side::Bid, 100, 1),
    });
    assert!(r.is_err(), "{r:?}");
    assert_eq!(b.snapshot(), before);
}

#[test]
fn a_resize_that_would_overflow_its_level_is_rejected() {
    let mut b = Book::new();
    b.apply(&BookOp::Insert(order(1, Side::Ask, 100, 1)))
        .unwrap();
    b.apply(&BookOp::Insert(order(2, Side::Ask, 100, 1)))
        .unwrap();
    let before = b.snapshot();
    let r = b.apply(&BookOp::Resize {
        oid: Oid(1),
        sz: Lots(u64::MAX),
    });
    assert!(r.is_err(), "{r:?}");
    assert_eq!(b.snapshot(), before);
    // The largest representable total is fine.
    b.apply(&BookOp::Resize {
        oid: Oid(1),
        sz: Lots(u64::MAX - 1),
    })
    .unwrap();
}

#[test]
fn a_placed_order_that_would_overflow_its_level_is_rejected() {
    for tif in [Tif::Gtc, Tif::Alo] {
        let mut b = Book::new();
        b.apply(&BookOp::Insert(order(1, Side::Bid, 100, u64::MAX)))
            .unwrap();
        let before = b.snapshot();
        let r = b.place(order(2, Side::Bid, 100, 1), tif);
        assert!(r.is_err(), "{tif:?}: {r:?}");
        assert_eq!(b.snapshot(), before);
    }
}
