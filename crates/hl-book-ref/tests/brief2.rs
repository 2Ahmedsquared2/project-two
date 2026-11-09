//! Hyperliquid Brief #2 additions: self-trade prevention (resting order
//! cancelled, incoming continues) behind a config flag that defaults on, and
//! modify = cancel old + place new (new oid, loses queue position).
//! Written before the implementation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_book_ref::{
    Book, Fill, Lots, MatchConfig, Modified, Oid, Order, Owner, Placed, Px, Side, Tif,
};

fn order(oid: u64, side: Side, px: u64, sz: u64) -> Order {
    Order {
        oid: Oid(oid),
        side,
        px: Px(px),
        sz: Lots(sz),
    }
}

fn queue(b: &Book, side: Side, px: u64) -> Vec<(u64, u64)> {
    b.level(side, Px(px))
        .map(|l| l.orders.iter().map(|(o, s)| (o.0, s.0)).collect())
        .unwrap_or_default()
}

const A: Owner = Owner(1);
const B: Owner = Owner(2);

// ---- self-trade prevention ----

#[test]
fn self_trade_prevention_is_on_by_default() {
    assert!(MatchConfig::default().self_trade_prevention);
    assert!(Book::new().config().self_trade_prevention);
}

#[test]
fn a_self_trade_cancels_the_resting_order_and_the_incoming_order_continues() {
    // Asks at 101: [A#1 x3, B#2 x4]. A buys 5 @ 101 GTC.
    let mut b = Book::new();
    b.place_owned(A, order(1, Side::Ask, 101, 3), Tif::Alo)
        .unwrap();
    b.place_owned(B, order(2, Side::Ask, 101, 4), Tif::Alo)
        .unwrap();
    let placed = b
        .place_owned(A, order(3, Side::Bid, 101, 5), Tif::Gtc)
        .unwrap();
    assert_eq!(
        placed,
        Placed {
            fills: vec![Fill {
                maker: Oid(2),
                px: Px(101),
                sz: Lots(4)
            }],
            rested: Some(Lots(1)),
            self_trade_cancels: vec![Oid(1)],
        }
    );
    // A's own ask is gone without a fill, B's is filled, A's remainder rests.
    assert!(b.get(Oid(1)).is_none());
    assert_eq!(b.best_ask(), None);
    assert_eq!(queue(&b, Side::Bid, 101), vec![(3, 1)]);
    assert_eq!(b.owner(Oid(3)), Some(A));
    assert_eq!(b.owner(Oid(1)), None);
}

#[test]
fn with_self_trade_prevention_off_the_same_owner_fills() {
    let mut b = Book::with_config(MatchConfig {
        self_trade_prevention: false,
    });
    b.place_owned(A, order(1, Side::Ask, 101, 3), Tif::Alo)
        .unwrap();
    let placed = b
        .place_owned(A, order(3, Side::Bid, 101, 2), Tif::Ioc)
        .unwrap();
    assert_eq!(placed.fills.len(), 1);
    assert_eq!(placed.self_trade_cancels, Vec::<Oid>::new());
    assert_eq!(queue(&b, Side::Ask, 101), vec![(1, 1)]);
}

#[test]
fn orders_without_an_owner_never_self_trade() {
    let mut b = Book::new();
    b.place(order(1, Side::Ask, 101, 3), Tif::Alo).unwrap();
    let placed = b.place(order(2, Side::Bid, 101, 3), Tif::Ioc).unwrap();
    assert_eq!(placed.fills.len(), 1);
    assert_eq!(placed.self_trade_cancels, Vec::<Oid>::new());
}

#[test]
fn owners_are_forgotten_when_an_order_leaves_the_book() {
    let mut b = Book::new();
    b.place_owned(A, order(1, Side::Ask, 101, 3), Tif::Alo)
        .unwrap();
    b.place_owned(B, order(2, Side::Bid, 101, 3), Tif::Ioc)
        .unwrap();
    assert_eq!(b.owner(Oid(1)), None);
    assert_eq!(b.order_count(), 0);
}

// ---- modify ----

#[test]
fn modify_cancels_the_old_order_and_places_a_new_one_at_the_back() {
    // Bids at 100: [A#1 x5, B#2 x5]. A modifies #1 down to 3 at the same price.
    let mut b = Book::new();
    b.place_owned(A, order(1, Side::Bid, 100, 5), Tif::Alo)
        .unwrap();
    b.place_owned(B, order(2, Side::Bid, 100, 5), Tif::Alo)
        .unwrap();
    let m = b.modify(Oid(1), Some(A), order(3, Side::Bid, 100, 3), Tif::Alo);
    assert_eq!(
        m,
        Modified {
            cancelled: true,
            placed: Ok(Placed {
                rested: Some(Lots(3)),
                ..Placed::default()
            }),
        }
    );
    // Smaller size does NOT keep queue position: new oid, back of the queue.
    assert_eq!(queue(&b, Side::Bid, 100), vec![(2, 5), (3, 3)]);
    assert_eq!(b.owner(Oid(3)), Some(A));
}

#[test]
fn modify_places_the_new_order_even_if_the_old_one_is_gone() {
    let mut b = Book::new();
    let m = b.modify(Oid(42), Some(A), order(3, Side::Ask, 105, 2), Tif::Gtc);
    assert!(!m.cancelled);
    assert_eq!(m.placed.unwrap().rested, Some(Lots(2)));
    assert_eq!(queue(&b, Side::Ask, 105), vec![(3, 2)]);
}

#[test]
fn modify_that_crosses_matches_like_a_new_order() {
    let mut b = Book::new();
    b.place_owned(B, order(1, Side::Ask, 101, 2), Tif::Alo)
        .unwrap();
    b.place_owned(A, order(2, Side::Bid, 99, 4), Tif::Alo)
        .unwrap();
    let m = b.modify(Oid(2), Some(A), order(3, Side::Bid, 101, 4), Tif::Gtc);
    assert!(m.cancelled);
    let placed = m.placed.unwrap();
    assert_eq!(placed.fills.len(), 1);
    assert_eq!(placed.rested, Some(Lots(2)));
    assert_eq!(b.best_bid(), Some(Px(101)));
    assert!(b.get(Oid(2)).is_none());
}

#[test]
fn a_rejected_new_order_still_leaves_the_old_one_cancelled() {
    // ALO modify that would cross: old order is gone, new one rejected.
    let mut b = Book::new();
    b.place_owned(B, order(1, Side::Ask, 101, 2), Tif::Alo)
        .unwrap();
    b.place_owned(A, order(2, Side::Bid, 99, 4), Tif::Alo)
        .unwrap();
    let m = b.modify(Oid(2), Some(A), order(3, Side::Bid, 101, 4), Tif::Alo);
    assert!(m.cancelled);
    assert!(m.placed.is_err());
    assert_eq!(b.order_count(), 1);
    assert_eq!(b.best_bid(), None);
}
