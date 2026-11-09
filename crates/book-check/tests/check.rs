//! Checker tests: level comparison, replay, and a deliberately wrong candidate.
//! Written before the implementation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Cursor;

use book_check::{check, compare_books, Candidate, CheckError, Divergence, Field};
use hl_book_ref::{Book, BookOp, BookSnapshot, Level, Lots, Oid, Order, Px, Side};
use hl_dataset::{Block, Header, Reader, Writer, FORMAT_VERSION};

fn level(px: u64, orders: &[(u64, u64)]) -> Level {
    Level {
        px: Px(px),
        orders: orders.iter().map(|&(o, s)| (Oid(o), Lots(s))).collect(),
    }
}

fn snap(bids: Vec<Level>, asks: Vec<Level>) -> BookSnapshot {
    BookSnapshot { bids, asks }
}

#[test]
fn identical_books_have_no_divergence() {
    let a = snap(vec![level(100, &[(1, 5)])], vec![level(101, &[(2, 1)])]);
    assert_eq!(compare_books(7, &a, &a.clone()), None);
}

#[test]
fn a_size_difference_is_reported_before_count_or_queue() {
    let recorded = snap(vec![level(100, &[(1, 5), (2, 1)])], vec![]);
    let candidate = snap(vec![level(100, &[(1, 4)])], vec![]);
    assert_eq!(
        compare_books(7, &recorded, &candidate),
        Some(Divergence {
            height: 7,
            side: Side::Bid,
            px: Px(100),
            field: Field::Size {
                recorded: Lots(6),
                candidate: Lots(4),
            },
        })
    );
}

#[test]
fn an_order_count_difference_with_equal_size_is_reported() {
    let recorded = snap(vec![], vec![level(100, &[(1, 3), (2, 3)])]);
    let candidate = snap(vec![], vec![level(100, &[(1, 6)])]);
    assert_eq!(
        compare_books(1, &recorded, &candidate),
        Some(Divergence {
            height: 1,
            side: Side::Ask,
            px: Px(100),
            field: Field::OrderCount {
                recorded: 2,
                candidate: 1,
            },
        })
    );
}

#[test]
fn a_queue_order_difference_is_reported_at_its_position() {
    let recorded = snap(vec![level(100, &[(1, 5), (2, 7)])], vec![]);
    let candidate = snap(vec![level(100, &[(2, 7), (1, 5)])], vec![]);
    assert_eq!(
        compare_books(3, &recorded, &candidate),
        Some(Divergence {
            height: 3,
            side: Side::Bid,
            px: Px(100),
            field: Field::QueuePosition {
                position: 0,
                recorded: Oid(1),
                candidate: Oid(2),
            },
        })
    );
}

#[test]
fn a_per_order_size_difference_with_equal_totals_is_reported() {
    let recorded = snap(vec![level(100, &[(1, 5), (2, 7)])], vec![]);
    let candidate = snap(vec![level(100, &[(1, 6), (2, 6)])], vec![]);
    assert_eq!(
        compare_books(3, &recorded, &candidate),
        Some(Divergence {
            height: 3,
            side: Side::Bid,
            px: Px(100),
            field: Field::OrderSize {
                position: 0,
                oid: Oid(1),
                recorded: Lots(5),
                candidate: Lots(6),
            },
        })
    );
}

#[test]
fn a_level_only_in_the_recorded_book_is_missing_from_the_candidate() {
    // Bids: recorded has 100 and 99, candidate only 99. The better (higher) level 100 is missing.
    let recorded = snap(vec![level(100, &[(1, 5)]), level(99, &[(2, 1)])], vec![]);
    let candidate = snap(vec![level(99, &[(2, 1)])], vec![]);
    assert_eq!(
        compare_books(9, &recorded, &candidate),
        Some(Divergence {
            height: 9,
            side: Side::Bid,
            px: Px(100),
            field: Field::MissingLevel {
                recorded: Lots(5),
                recorded_orders: 1,
            },
        })
    );
    // Asks: recorded has 101 and 102, candidate only 101: the tail level is missing.
    let recorded = snap(vec![], vec![level(101, &[(1, 5)]), level(102, &[(2, 1)])]);
    let candidate = snap(vec![], vec![level(101, &[(1, 5)])]);
    assert_eq!(
        compare_books(9, &recorded, &candidate),
        Some(Divergence {
            height: 9,
            side: Side::Ask,
            px: Px(102),
            field: Field::MissingLevel {
                recorded: Lots(1),
                recorded_orders: 1,
            },
        })
    );
}

#[test]
fn a_level_only_in_the_candidate_is_extra() {
    let recorded = snap(vec![], vec![level(102, &[(2, 1)])]);
    let candidate = snap(vec![], vec![level(101, &[(1, 5)]), level(102, &[(2, 1)])]);
    assert_eq!(
        compare_books(9, &recorded, &candidate),
        Some(Divergence {
            height: 9,
            side: Side::Ask,
            px: Px(101),
            field: Field::ExtraLevel {
                candidate: Lots(5),
                candidate_orders: 1,
            },
        })
    );
}

#[test]
fn bids_are_checked_before_asks() {
    let recorded = snap(vec![level(100, &[(1, 5)])], vec![level(101, &[(2, 1)])]);
    let candidate = snap(vec![level(100, &[(1, 4)])], vec![level(101, &[(2, 2)])]);
    let d = compare_books(1, &recorded, &candidate).unwrap();
    assert_eq!(d.side, Side::Bid);
}

fn header() -> Header {
    Header {
        format_version: FORMAT_VERSION,
        chain: "test".to_owned(),
        asset: "T".to_owned(),
        px_decimals: 0,
        sz_decimals: 0,
        first_height: 1,
        source: "in-memory".to_owned(),
        extra: std::collections::BTreeMap::default(),
    }
}

fn insert(oid: u64, side: Side, px: u64, sz: u64) -> BookOp {
    BookOp::Insert(Order {
        oid: Oid(oid),
        side,
        px: Px(px),
        sz: Lots(sz),
    })
}

/// Two blocks; the second resizes order 1 from 5 to 2 lots.
fn dataset() -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header()).unwrap();
    w.write_block(&Block {
        height: 1,
        time_ms: None,
        ops: vec![insert(1, Side::Bid, 100, 5), insert(2, Side::Ask, 105, 3)],
    })
    .unwrap();
    w.write_book(&snap(
        vec![level(100, &[(1, 5)])],
        vec![level(105, &[(2, 3)])],
    ))
    .unwrap();
    w.write_block(&Block {
        height: 2,
        time_ms: None,
        ops: vec![BookOp::Resize {
            oid: Oid(1),
            sz: Lots(2),
        }],
    })
    .unwrap();
    w.write_book(&snap(
        vec![level(100, &[(1, 2)])],
        vec![level(105, &[(2, 3)])],
    ))
    .unwrap();
    w.finish().unwrap();
    out
}

#[test]
fn the_reference_book_replays_a_consistent_dataset() {
    let reader = Reader::new(Cursor::new(dataset())).unwrap();
    let report = check(reader, &mut Book::new()).unwrap();
    assert_eq!(report.blocks, 2);
    assert_eq!(report.books_compared, 2);
    assert_eq!(report.ops, 3);
}

/// A candidate that silently ignores every `Resize`: a realistic class of replica bug.
struct IgnoresResizes(Book);

impl Candidate for IgnoresResizes {
    type Error = hl_book_ref::ApplyError;

    fn apply(&mut self, op: &BookOp) -> Result<(), Self::Error> {
        if matches!(op, BookOp::Resize { .. }) {
            return Ok(());
        }
        self.0.apply(op)
    }

    fn snapshot(&self) -> BookSnapshot {
        self.0.snapshot()
    }
}

#[test]
fn a_candidate_that_drops_resizes_diverges_at_the_first_affected_block() {
    let reader = Reader::new(Cursor::new(dataset())).unwrap();
    let err = check(reader, &mut IgnoresResizes(Book::new())).unwrap_err();
    match err {
        CheckError::Divergence(d) => assert_eq!(
            d,
            Divergence {
                height: 2,
                side: Side::Bid,
                px: Px(100),
                field: Field::Size {
                    recorded: Lots(2),
                    candidate: Lots(5),
                },
            }
        ),
        other => panic!("expected a divergence, got {other:?}"),
    }
}

#[test]
fn an_op_the_candidate_rejects_is_reported_with_its_block_and_index() {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header()).unwrap();
    w.write_block(&Block {
        height: 1,
        time_ms: None,
        ops: vec![
            insert(1, Side::Bid, 100, 5),
            BookOp::Cancel { oid: Oid(42) },
        ],
    })
    .unwrap();
    w.finish().unwrap();
    let reader = Reader::new(Cursor::new(out)).unwrap();
    let err = check(reader, &mut Book::new()).unwrap_err();
    match err {
        CheckError::Apply {
            height, op_index, ..
        } => {
            assert_eq!((height, op_index), (1, 1));
        }
        other => panic!("expected an apply error, got {other:?}"),
    }
}

#[test]
fn divergences_print_block_side_price_and_field() {
    let d = Divergence {
        height: 4,
        side: Side::Bid,
        px: Px(1000),
        field: Field::Size {
            recorded: Lots(250),
            candidate: Lots(200),
        },
    };
    let text = d.to_string();
    assert!(text.contains("block 4"), "{text}");
    assert!(text.contains("bid"), "{text}");
    assert!(text.contains("1000"), "{text}");
    assert!(text.contains("size"), "{text}");
    assert!(text.contains("250"), "{text}");
    assert!(text.contains("200"), "{text}");
}
