//! Checker tests: level comparison, replay, and a deliberately wrong candidate.
//! Written before the implementation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Cursor;

use book_check::{
    check, compare_l2, compare_l4, Candidate, CheckError, Divergence, Field, LevelDiff,
};
use hl_book_ref::{Book, BookOp, BookSnapshot, L2Level, Level, Lots, Oid, Order, Px, Side};
use hl_dataset::{
    AssetMeta, AssetOp, Block, BookState, Header, L4Book, Reader, Writer, FORMAT_VERSION,
};

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
    assert_eq!(compare_l4(&a, &a.clone()), None);
}

#[test]
fn a_size_difference_is_reported_before_count_or_queue() {
    let recorded = snap(vec![level(100, &[(1, 5), (2, 1)])], vec![]);
    let candidate = snap(vec![level(100, &[(1, 4)])], vec![]);
    assert_eq!(
        compare_l4(&recorded, &candidate),
        Some(LevelDiff {
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
        compare_l4(&recorded, &candidate),
        Some(LevelDiff {
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
        compare_l4(&recorded, &candidate),
        Some(LevelDiff {
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
        compare_l4(&recorded, &candidate),
        Some(LevelDiff {
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
        compare_l4(&recorded, &candidate),
        Some(LevelDiff {
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
        compare_l4(&recorded, &candidate),
        Some(LevelDiff {
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
        compare_l4(&recorded, &candidate),
        Some(LevelDiff {
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
    let d = compare_l4(&recorded, &candidate).unwrap();
    assert_eq!(d.side, Side::Bid);
}

fn header() -> Header {
    Header {
        format_version: FORMAT_VERSION,
        chain: "test".to_owned(),
        first_height: 1,
        source: "in-memory".to_owned(),
        assets: vec![
            AssetMeta::new(0, "AAA", 0, false).unwrap(),
            AssetMeta::new(1, "BBB", 0, false).unwrap(),
        ],
        extra: std::collections::BTreeMap::default(),
    }
}

fn insert(asset: u32, oid: u64, side: Side, px: u64, sz: u64) -> AssetOp {
    AssetOp {
        asset,
        op: BookOp::Insert(Order {
            oid: Oid(oid),
            side,
            px: Px(px),
            sz: Lots(sz),
        }),
    }
}

fn block(height: u64, ops: Vec<AssetOp>) -> Block {
    Block {
        height,
        time_ms: None,
        hash: None,
        proposer: None,
        txs: vec![],
        ops,
    }
}

fn l4(height: u64, asset: u32, s: &BookSnapshot) -> BookState {
    BookState::L4(L4Book::from_snapshot(height, asset, s))
}

/// Two blocks over two assets; the second resizes AAA order 1 from 5 to 2 lots.
fn dataset() -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header()).unwrap();
    w.write_block(&block(
        1,
        vec![
            insert(0, 1, Side::Bid, 100, 5),
            insert(0, 2, Side::Ask, 105, 3),
            // Same oid space is per dataset, but books are per asset.
            insert(1, 3, Side::Bid, 50, 9),
        ],
    ))
    .unwrap();
    w.write_book(&l4(
        1,
        0,
        &snap(vec![level(100, &[(1, 5)])], vec![level(105, &[(2, 3)])]),
    ))
    .unwrap();
    w.write_book(&l4(1, 1, &snap(vec![level(50, &[(3, 9)])], vec![])))
        .unwrap();
    w.write_block(&block(
        2,
        vec![AssetOp {
            asset: 0,
            op: BookOp::Resize {
                oid: Oid(1),
                sz: Lots(2),
            },
        }],
    ))
    .unwrap();
    w.write_book(&l4(
        2,
        0,
        &snap(vec![level(100, &[(1, 2)])], vec![level(105, &[(2, 3)])]),
    ))
    .unwrap();
    w.finish().unwrap();
    out
}

fn reader(bytes: Vec<u8>) -> Reader<Cursor<Vec<u8>>> {
    Reader::new(Cursor::new(bytes)).unwrap()
}

#[test]
fn the_reference_book_replays_a_consistent_multi_asset_dataset() {
    let report = check(reader(dataset()), |_| Book::new()).unwrap();
    assert_eq!(report.blocks, 2);
    assert_eq!(report.l4_books_compared, 3);
    assert_eq!(report.l2_books_compared, 0);
    assert_eq!(report.ops, 4);
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
    let err = check(reader(dataset()), |_| IgnoresResizes(Book::new())).unwrap_err();
    match err {
        CheckError::Divergence(d) => assert_eq!(
            d,
            Divergence {
                height: 2,
                asset: 0,
                coin: "AAA".to_owned(),
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
fn books_are_per_asset() {
    // Asset BBB's book must not see AAA's orders: an AAA bid at 100 would sit above BBB's 50.
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header()).unwrap();
    w.write_block(&block(
        1,
        vec![
            insert(0, 1, Side::Bid, 100, 5),
            insert(1, 2, Side::Bid, 50, 1),
        ],
    ))
    .unwrap();
    w.write_book(&l4(1, 1, &snap(vec![level(50, &[(2, 1)])], vec![])))
        .unwrap();
    w.finish().unwrap();
    check(reader(out), |_| Book::new()).unwrap();
}

#[test]
fn an_op_the_candidate_rejects_is_reported_with_its_block_asset_and_index() {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header()).unwrap();
    w.write_block(&block(
        1,
        vec![
            insert(0, 1, Side::Bid, 100, 5),
            AssetOp {
                asset: 1,
                op: BookOp::Cancel { oid: Oid(1) },
            },
        ],
    ))
    .unwrap();
    w.finish().unwrap();
    let err = check(reader(out), |_| Book::new()).unwrap_err();
    match err {
        CheckError::Apply {
            height,
            asset,
            op_index,
            ..
        } => {
            assert_eq!((height, asset, op_index), (1, 1, 1));
        }
        other => panic!("expected an apply error, got {other:?}"),
    }
}

// ---- L2 ----

fn l2(px: u64, sz: u64, n: u32) -> L2Level {
    L2Level {
        px: Px(px),
        sz: Lots(sz),
        n,
    }
}

#[test]
fn l2_compares_size_then_order_count() {
    assert_eq!(
        compare_l2(&[l2(100, 5, 2)], &[], &[l2(100, 5, 2)], &[]),
        None
    );
    assert_eq!(
        compare_l2(&[l2(100, 5, 2)], &[], &[l2(100, 4, 2)], &[]),
        Some(LevelDiff {
            side: Side::Bid,
            px: Px(100),
            field: Field::Size {
                recorded: Lots(5),
                candidate: Lots(4),
            },
        })
    );
    assert_eq!(
        compare_l2(&[], &[l2(101, 5, 2)], &[], &[l2(101, 5, 1)]),
        Some(LevelDiff {
            side: Side::Ask,
            px: Px(101),
            field: Field::OrderCount {
                recorded: 2,
                candidate: 1,
            },
        })
    );
    assert_eq!(
        compare_l2(&[], &[l2(101, 5, 2)], &[], &[l2(100, 1, 1), l2(101, 5, 2)]),
        Some(LevelDiff {
            side: Side::Ask,
            px: Px(100),
            field: Field::ExtraLevel {
                candidate: Lots(1),
                candidate_orders: 1,
            },
        })
    );
}

fn l2_line(height: u64, depth: Option<u32>, bids: &str, asks: &str) -> String {
    let depth = depth.map_or(String::new(), |d| format!(r#","depth":{d}"#));
    format!(
        r#"{{"kind":"l2_book","height":{height},"asset":0{depth},"bids":[{bids}],"asks":[{asks}]}}"#
    )
}

fn header_and_block(ops: &str) -> String {
    let mut out = Vec::new();
    Writer::new(&mut out, &header()).unwrap().finish().unwrap();
    format!(
        "{}{{\"kind\":\"block\",\"height\":1,\"ops\":[{ops}]}}\n",
        String::from_utf8(out).unwrap()
    )
}

const THREE_BIDS: &str = r#"{"asset":0,"op":"insert","oid":1,"side":"bid","px":100,"sz":5},{"asset":0,"op":"insert","oid":2,"side":"bid","px":100,"sz":1},{"asset":0,"op":"insert","oid":3,"side":"bid","px":99,"sz":7}"#;

#[test]
fn an_l2_book_with_depth_compares_only_the_top_levels() {
    // Candidate has bids 100 (6 lots, 2 orders) and 99; recorded top-1 shows only 100.
    let text = format!(
        "{}{}\n",
        header_and_block(THREE_BIDS),
        l2_line(1, Some(1), r#"{"px_ticks":100,"sz_lots":6,"n":2}"#, "")
    );
    let report = check(reader(text.into_bytes()), |_| Book::new()).unwrap();
    assert_eq!(report.l2_books_compared, 1);
}

#[test]
fn an_l2_book_without_depth_is_the_whole_book() {
    let text = format!(
        "{}{}\n",
        header_and_block(THREE_BIDS),
        l2_line(1, None, r#"{"px_ticks":100,"sz_lots":6,"n":2}"#, "")
    );
    let err = check(reader(text.into_bytes()), |_| Book::new()).unwrap_err();
    let CheckError::Divergence(d) = err else {
        panic!("expected a divergence, got {err:?}");
    };
    assert_eq!((d.side, d.px), (Side::Bid, Px(99)));
    assert!(matches!(d.field, Field::ExtraLevel { .. }), "{d:?}");
}

#[test]
fn an_l2_order_count_divergence_is_caught() {
    let text = format!(
        "{}{}\n",
        header_and_block(THREE_BIDS),
        l2_line(
            1,
            Some(20),
            r#"{"px_ticks":100,"sz_lots":6,"n":3},{"px_ticks":99,"sz_lots":7,"n":1}"#,
            ""
        )
    );
    let err = check(reader(text.into_bytes()), |_| Book::new()).unwrap_err();
    let CheckError::Divergence(d) = err else {
        panic!("expected a divergence, got {err:?}");
    };
    assert_eq!(
        d.field,
        Field::OrderCount {
            recorded: 3,
            candidate: 2
        }
    );
}

#[test]
fn raw_txs_are_counted_but_not_replayed() {
    let text = format!(
        "{}{{\"kind\":\"block\",\"height\":2,\"txs\":[{{\"user\":\"0x1\",\"hash\":\"0x2\",\"error\":null,\"action\":{{\"type\":\"noop\",\"nonce\":1}}}}]}}\n",
        header_and_block("")
    );
    let report = check(reader(text.into_bytes()), |_| Book::new()).unwrap();
    assert_eq!(report.txs, 1);
    assert_eq!(report.ops, 0);
    assert!(
        report.to_string().contains("1 txs not replayed"),
        "{report}"
    );
}

#[test]
fn divergences_print_block_coin_side_price_and_field() {
    let d = Divergence {
        height: 4,
        asset: 0,
        coin: "GOLD".to_owned(),
        side: Side::Bid,
        px: Px(1000),
        field: Field::Size {
            recorded: Lots(250),
            candidate: Lots(200),
        },
    };
    assert_eq!(
        d.to_string(),
        "divergence at block 4 in GOLD (asset 0) on the bid side at price 1000 ticks: size: recorded 250 lots, candidate 200 lots"
    );
}
