//! Parsing the node's batch lines and events (schemas: `hyperliquid-dex/node`
//! README, `order_book_server` `server/src/types/{node_data,mod}.rs`, Hyperliquid
//! docs "L1 data schemas").

use hl_node_convert::node::{
    parse_batch, BlockBatches, BookDiff, DiffKind, LineError, NodeError, NodeSide, OrderStatus,
};

fn diff(json: &str) -> BookDiff {
    match serde_json::from_str(json) {
        Ok(d) => d,
        Err(e) => panic!("{json}: {e}"),
    }
}

fn status(st: &str, is_trigger: bool, tif: &str) -> OrderStatus {
    let json = format!(
        r#"{{"time":"2026-11-19T12:00:00.100","user":"0xab","status":"{st}","order":{{"coin":"BTC","side":"B","limitPx":"100.0","sz":"1.0","oid":7,"timestamp":1,"triggerCondition":"N/A","isTrigger":{is_trigger},"triggerPx":"0.0","children":[],"isPositionTpsl":false,"reduceOnly":false,"orderType":"Limit","origSz":"1.0","tif":{tif},"cloid":null}}}}"#
    );
    match serde_json::from_str(&json) {
        Ok(s) => s,
        Err(e) => panic!("{json}: {e}"),
    }
}

#[test]
fn new_diff_without_side_or_anchor() {
    let d = diff(
        r#"{"user":"0xab","oid":5,"coin":"CHILLGUY","px":"1.36","raw_book_diff":{"new":{"sz":"186910.0"}}}"#,
    );
    assert_eq!(d.oid, 5);
    assert_eq!(d.coin, "CHILLGUY");
    assert_eq!(d.px, "1.36");
    assert_eq!(d.side, None);
    assert_eq!(
        d.raw_book_diff,
        DiffKind::New {
            sz: "186910.0".into(),
            insert_before: None
        }
    );
}

#[test]
fn new_diff_with_side_and_insert_before() {
    let d = diff(
        r#"{"user":"0xab","oid":5,"coin":"BTC","side":"Bid","px":"1.0","raw_book_diff":{"new":{"sz":"1.5","insertBefore":105338503859}}}"#,
    );
    assert_eq!(d.side, Some(NodeSide::Bid));
    assert_eq!(
        d.raw_book_diff,
        DiffKind::New {
            sz: "1.5".into(),
            insert_before: Some(105_338_503_859)
        }
    );
}

#[test]
fn update_and_remove_diffs() {
    let u = diff(
        r#"{"user":"0xab","oid":6,"coin":"BTC","side":"Ask","px":"115323.2","raw_book_diff":{"update":{"origSz":"0.2086","newSz":"0.2076"}}}"#,
    );
    assert_eq!(u.side, Some(NodeSide::Ask));
    assert_eq!(
        u.raw_book_diff,
        DiffKind::Update {
            orig_sz: "0.2086".into(),
            new_sz: "0.2076".into()
        }
    );
    let r = diff(
        r#"{"user":"0xab","oid":7,"side":"A","px":"1.0","coin":"HYPE","raw_book_diff":"remove"}"#,
    );
    assert_eq!(r.side, Some(NodeSide::Ask));
    assert_eq!(r.raw_book_diff, DiffKind::Remove);
}

#[test]
fn unknown_diff_kind_is_an_error() {
    let bad = r#"{"user":"0xab","oid":7,"coin":"HYPE","px":"1.0","raw_book_diff":"explode"}"#;
    assert!(serde_json::from_str::<BookDiff>(bad).is_err());
    let bad2 =
        r#"{"user":"0xab","oid":7,"coin":"HYPE","px":"1.0","raw_book_diff":{"move":{"sz":"1"}}}"#;
    assert!(serde_json::from_str::<BookDiff>(bad2).is_err());
}

#[test]
fn status_parses_and_inserted_rule() {
    let s = status("open", false, r#""Alo""#);
    assert_eq!(s.order.oid, 7);
    assert_eq!(s.order.side, NodeSide::Bid);
    assert_eq!(s.order.limit_px, "100.0");
    assert!(s.is_inserted_into_book());
    assert!(status("open", false, r#""Gtc""#).is_inserted_into_book());
    assert!(!status("open", false, r#""Ioc""#).is_inserted_into_book());
    assert!(!status("open", true, "null").is_inserted_into_book());
    assert!(status("triggered", true, "null").is_inserted_into_book());
    assert!(!status("triggered", false, r#""Gtc""#).is_inserted_into_book());
    assert!(!status("filled", false, r#""Gtc""#).is_inserted_into_book());
    assert!(!status("canceled", false, r#""Alo""#).is_inserted_into_book());
}

const LINE: &str = r#"{"local_time":"2026-11-19T12:00:00.130","block_time":"2026-11-19T12:00:00.100","block_number":1001,"events":[{"user":"0xab","oid":14,"coin":"BTC","px":"1.0","raw_book_diff":"remove"}]}"#;

#[test]
fn batch_line_parses_block_and_time() {
    let b = parse_batch::<BookDiff>(LINE).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(b.block_number, 1001);
    assert_eq!(b.block_time_ms, 1_795_089_600_100);
    assert_eq!(b.events.len(), 1);
    assert_eq!(b.events[0].oid, 14);
}

#[test]
fn batch_line_errors() {
    assert!(matches!(
        parse_batch::<BookDiff>("{not json"),
        Err(LineError::Json(_))
    ));
    assert!(matches!(
        parse_batch::<BookDiff>(
            r#"{"local_time":"x","block_time":"2026-11-19 12:00:00","block_number":1,"events":[]}"#
        ),
        Err(LineError::Time(_))
    ));
    assert!(matches!(
        parse_batch::<BookDiff>(
            r#"{"local_time":"x","block_time":"2026-11-19T12:00:00","events":[]}"#
        ),
        Err(LineError::Json(_))
    ));
}

fn line(block: u64, time: &str, oids: &[u64]) -> String {
    let events: Vec<String> = oids
        .iter()
        .map(|o| {
            format!(
                r#"{{"user":"0xab","oid":{o},"coin":"BTC","px":"1.0","raw_book_diff":"remove"}}"#
            )
        })
        .collect();
    format!(
        r#"{{"local_time":"2026-11-19T12:00:01","block_time":"2026-11-19T12:00:00.{time}","block_number":{block},"events":[{}]}}"#,
        events.join(",")
    )
}

/// (block, time ms, oids) per yielded batch, or the error text.
type Summary = Result<(u64, u64, Vec<u64>), String>;

fn batches(text: &str) -> Vec<Summary> {
    BlockBatches::<_, BookDiff>::new(text.as_bytes())
        .map(|r| {
            r.map(|b| {
                (
                    b.block_number,
                    b.block_time_ms,
                    b.events.iter().map(|e| e.oid).collect(),
                )
            })
            .map_err(|e| e.to_string())
        })
        .collect()
}

#[test]
fn consecutive_lines_of_one_block_merge_in_order() {
    let text = format!(
        "{}\n{}\n\n{}\n{}\n",
        line(5, "100", &[1, 2]),
        line(5, "100", &[3]),
        line(6, "170", &[]),
        line(8, "240", &[4])
    );
    let got = batches(&text);
    assert_eq!(
        got,
        vec![
            Ok((5, 1_795_089_600_100, vec![1, 2, 3])),
            Ok((6, 1_795_089_600_170, vec![])),
            Ok((8, 1_795_089_600_240, vec![4])),
        ]
    );
}

#[test]
fn regression_is_an_error_with_its_line() {
    let text = format!("{}\n{}\n", line(5, "100", &[1]), line(4, "100", &[2]));
    let mut it = BlockBatches::<_, BookDiff>::new(text.as_bytes());
    let first = it.next();
    assert!(matches!(first, Some(Ok(_))), "{first:?}");
    match it.next() {
        Some(Err(NodeError::Regression {
            line: 2,
            previous: 5,
            found: 4,
        })) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn block_reappearing_after_another_is_a_regression() {
    let text = format!(
        "{}\n{}\n{}\n",
        line(5, "100", &[1]),
        line(6, "170", &[2]),
        line(5, "100", &[3])
    );
    let got = batches(&text);
    assert_eq!(got.len(), 3);
    assert!(
        got[2].as_ref().is_err_and(|e| e.contains("line 3")),
        "{got:?}"
    );
}

#[test]
fn same_block_with_two_times_is_an_error() {
    let text = format!("{}\n{}\n", line(5, "100", &[1]), line(5, "170", &[2]));
    match BlockBatches::<_, BookDiff>::new(text.as_bytes()).next() {
        Some(Err(NodeError::TimeMismatch { line: 2, block: 5 })) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn bad_line_reports_its_number() {
    let text = format!("{}\n\nnope\n", line(5, "100", &[1]));
    let mut it = BlockBatches::<_, BookDiff>::new(text.as_bytes());
    // The complete block before the bad line is still delivered first.
    assert!(matches!(it.next(), Some(Ok(b)) if b.block_number == 5));
    match it.next() {
        Some(Err(NodeError::Line { line: 3, .. })) => {}
        other => panic!("{other:?}"),
    }
}
