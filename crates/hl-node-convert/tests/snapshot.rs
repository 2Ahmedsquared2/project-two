//! Parsing `l4Snapshots` (`includeUsers`, `includeHeightInOutput`) and turning one
//! coin into a recorded `l4_book`.

use hl_dataset::{AssetMeta, L4Level, L4Order};
use hl_node_convert::node::NodeSide;
use hl_node_convert::snapshot::{l4_book, L4Snapshot, SnapshotError};

fn order(user: &str, side: &str, px: &str, sz: &str, oid: u64, trigger: bool) -> String {
    format!(
        r#"["{user}",{{"coin":"BTC","side":"{side}","limitPx":"{px}","sz":"{sz}","oid":{oid},"timestamp":1,"triggerCondition":"N/A","isTrigger":{trigger},"triggerPx":"0.0","children":[],"isPositionTpsl":false,"reduceOnly":false,"orderType":"Limit","origSz":"{sz}","tif":"Gtc","cloid":null}}]"#
    )
}

fn btc() -> AssetMeta {
    AssetMeta::new(0, "BTC", 5, false).unwrap_or_else(|e| panic!("{e}"))
}

fn snap(bids: &[String], asks: &[String]) -> L4Snapshot {
    let text = format!(
        r#"[1003,[["BTC",[[{}],[{}]]],["ETH",[[],[]]]]]"#,
        bids.join(","),
        asks.join(",")
    );
    L4Snapshot::from_json(&text).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn parses_height_coins_users_and_triggers() {
    let s = snap(
        &[order("0xaa", "B", "100.0", "0.5", 1, false)],
        &[order("0xbb", "A", "101.0", "0.1", 2, true)],
    );
    assert_eq!(s.height, 1003);
    assert_eq!(s.books.len(), 2);
    let b = s.book("BTC").unwrap_or_else(|| panic!("no BTC"));
    assert_eq!(b.bids.len(), 1);
    assert_eq!(b.bids[0].user, "0xaa");
    assert_eq!(b.bids[0].side, NodeSide::Bid);
    assert_eq!(b.bids[0].limit_px, "100.0");
    assert_eq!(b.bids[0].sz, "0.5");
    assert!(!b.bids[0].is_trigger);
    assert!(b.asks[0].is_trigger);
    assert!(s.book("SOL").is_none());
}

#[test]
fn snapshot_without_users_or_height_is_rejected() {
    // includeUsers false: orders are bare objects, not [user, order].
    let no_users = r#"[5,[["BTC",[[{"coin":"BTC","side":"B","limitPx":"1.0","sz":"1.0","oid":1,"isTrigger":false}],[]]]]]"#;
    assert!(matches!(
        L4Snapshot::from_json(no_users),
        Err(SnapshotError::Json(_))
    ));
    // includeHeightInOutput false: no leading height.
    let no_height = r#"[["BTC",[[],[]]]]"#;
    assert!(matches!(
        L4Snapshot::from_json(no_height),
        Err(SnapshotError::Json(_))
    ));
}

#[test]
fn l4_book_groups_levels_best_first_and_keeps_queue_order() {
    // Listed in a deliberately scrambled level order; queue order inside 100.0 is 3, 1.
    let s = snap(
        &[
            order("0xaa", "B", "99.0", "1.0", 9, false),
            order("0xaa", "B", "100.0", "0.25", 3, false),
            order("0xbb", "B", "100.0", "0.5", 1, false),
        ],
        &[
            order("0xcc", "A", "102.0", "0.2", 4, false),
            order("0xcc", "A", "105.0", "0.1", 8, true),
            order("0xdd", "A", "101.0", "0.3", 5, false),
        ],
    );
    let book = l4_book(s.book("BTC"), &btc(), 1003).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(book.height, 1003);
    assert_eq!(book.asset, 0);
    assert_eq!(book.time_ms, None);
    let o = |oid, sz_lots, user: &str| L4Order {
        oid,
        sz_lots,
        user: Some(user.to_owned()),
    };
    assert_eq!(
        book.bids,
        vec![
            L4Level {
                px_ticks: 1000,
                px: Some("100.0".into()),
                orders: vec![o(3, 25_000, "0xaa"), o(1, 50_000, "0xbb")]
            },
            L4Level {
                px_ticks: 990,
                px: Some("99.0".into()),
                orders: vec![o(9, 100_000, "0xaa")]
            },
        ]
    );
    // Trigger order 8 is not on the book.
    assert_eq!(
        book.asks,
        vec![
            L4Level {
                px_ticks: 1010,
                px: Some("101.0".into()),
                orders: vec![o(5, 30_000, "0xdd")]
            },
            L4Level {
                px_ticks: 1020,
                px: Some("102.0".into()),
                orders: vec![o(4, 20_000, "0xcc")]
            },
        ]
    );
}

#[test]
fn absent_coin_is_an_empty_book() {
    let book = l4_book(None, &btc(), 7).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((book.height, book.asset), (7, 0));
    assert!(book.bids.is_empty() && book.asks.is_empty());
}

#[test]
fn l4_book_rejects_wrong_list_and_bad_decimals() {
    let s = snap(&[order("0xaa", "A", "100.0", "0.5", 1, false)], &[]);
    assert!(matches!(
        l4_book(s.book("BTC"), &btc(), 1),
        Err(SnapshotError::SideMismatch { oid: 1, .. })
    ));
    let s = snap(&[order("0xaa", "B", "100.05", "0.5", 2, false)], &[]);
    assert!(matches!(
        l4_book(s.book("BTC"), &btc(), 1),
        Err(SnapshotError::Decimal {
            oid: 2,
            what: "price",
            ..
        })
    ));
    let s = snap(&[order("0xaa", "B", "100.0", "0.000001", 3, false)], &[]);
    assert!(matches!(
        l4_book(s.book("BTC"), &btc(), 1),
        Err(SnapshotError::Decimal {
            oid: 3,
            what: "size",
            ..
        })
    ));
}
