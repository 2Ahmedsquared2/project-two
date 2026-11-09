//! Round-trip and validation tests for the dataset format. Written before the implementation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Cursor;

use hl_book_ref::{BookOp, BookSnapshot, Level, Lots, Oid, Order, Px, Side};
use hl_dataset::{Block, Header, ReadError, Reader, Step, WriteError, Writer, FORMAT_VERSION};

fn header() -> Header {
    Header {
        format_version: FORMAT_VERSION,
        chain: "mainnet".to_owned(),
        asset: "BTC".to_owned(),
        px_decimals: 1,
        sz_decimals: 5,
        first_height: 100,
        source: "hand-built".to_owned(),
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

fn level(px: u64, orders: &[(u64, u64)]) -> Level {
    Level {
        px: Px(px),
        orders: orders.iter().map(|&(o, s)| (Oid(o), Lots(s))).collect(),
    }
}

fn two_block_dataset() -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header()).unwrap();
    w.write_block(&Block {
        height: 100,
        time_ms: Some(1_700_000_000_000),
        ops: vec![insert(1, Side::Bid, 1000, 5), insert(2, Side::Ask, 1010, 3)],
    })
    .unwrap();
    w.write_book(&BookSnapshot {
        bids: vec![level(1000, &[(1, 5)])],
        asks: vec![level(1010, &[(2, 3)])],
    })
    .unwrap();
    w.write_block(&Block {
        height: 101,
        time_ms: None,
        ops: vec![BookOp::Resize {
            oid: Oid(1),
            sz: Lots(2),
        }],
    })
    .unwrap();
    w.write_book(&BookSnapshot {
        bids: vec![level(1000, &[(1, 2)])],
        asks: vec![level(1010, &[(2, 3)])],
    })
    .unwrap();
    w.finish().unwrap();
    out
}

#[test]
fn round_trips_header_blocks_and_books() {
    let bytes = two_block_dataset();
    let reader = Reader::new(Cursor::new(bytes)).unwrap();
    assert_eq!(reader.header(), &header());
    let steps: Vec<Step> = reader.steps().collect::<Result<_, _>>().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].block.height, 100);
    assert_eq!(steps[0].block.ops.len(), 2);
    assert_eq!(
        steps[0].book.as_ref().unwrap().bids,
        vec![level(1000, &[(1, 5)])]
    );
    assert_eq!(steps[1].block.height, 101);
    assert_eq!(
        steps[1].book.as_ref().unwrap().bids,
        vec![level(1000, &[(1, 2)])]
    );
}

#[test]
fn a_block_without_a_book_record_yields_a_step_with_no_book() {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header()).unwrap();
    w.write_block(&Block {
        height: 100,
        time_ms: None,
        ops: vec![],
    })
    .unwrap();
    w.write_block(&Block {
        height: 101,
        time_ms: None,
        ops: vec![],
    })
    .unwrap();
    w.write_book(&BookSnapshot::default()).unwrap();
    w.finish().unwrap();
    let steps: Vec<Step> = Reader::new(Cursor::new(out))
        .unwrap()
        .steps()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(steps[0].book.is_none());
    assert!(steps[1].book.is_some());
}

#[test]
fn the_wire_shape_is_documented_json_lines() {
    let bytes = two_block_dataset();
    let text = String::from_utf8(bytes).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "header, block, book, block, book");
    assert_eq!(
        lines[0],
        r#"{"kind":"header","format_version":1,"chain":"mainnet","asset":"BTC","px_decimals":1,"sz_decimals":5,"first_height":100,"source":"hand-built"}"#
    );
    assert_eq!(
        lines[1],
        r#"{"kind":"block","height":100,"time_ms":1700000000000,"ops":[{"op":"insert","oid":1,"side":"bid","px":1000,"sz":5},{"op":"insert","oid":2,"side":"ask","px":1010,"sz":3}]}"#
    );
    assert_eq!(
        lines[2],
        r#"{"kind":"book","height":100,"bids":[{"px":1000,"orders":[[1,5]]}],"asks":[{"px":1010,"orders":[[2,3]]}]}"#
    );
    assert_eq!(
        lines[3],
        r#"{"kind":"block","height":101,"ops":[{"op":"resize","oid":1,"sz":2}]}"#
    );
}

#[test]
fn unknown_header_fields_survive_a_round_trip() {
    let line = r#"{"kind":"header","format_version":1,"chain":"mainnet","asset":"ETH","px_decimals":2,"sz_decimals":4,"first_height":7,"source":"x","hl_node_binary_sha256":"abc"}"#;
    let reader = Reader::new(Cursor::new(format!("{line}\n"))).unwrap();
    assert_eq!(
        reader.header().extra.get("hl_node_binary_sha256"),
        Some(&serde_json::Value::String("abc".to_owned()))
    );
    let mut out = Vec::new();
    Writer::new(&mut out, reader.header())
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(String::from_utf8(out).unwrap().trim_end(), line);
}

#[test]
fn reader_rejects_a_file_without_a_header() {
    let err = Reader::new(Cursor::new(
        r#"{"kind":"block","height":1,"ops":[]}
"#,
    ))
    .err()
    .unwrap();
    assert!(matches!(err, ReadError::MissingHeader), "{err:?}");
    let err = Reader::new(Cursor::new("")).err().unwrap();
    assert!(matches!(err, ReadError::MissingHeader), "{err:?}");
}

#[test]
fn reader_rejects_an_unsupported_version() {
    let err = Reader::new(Cursor::new(
        r#"{"kind":"header","format_version":99,"chain":"m","asset":"A","px_decimals":0,"sz_decimals":0,"first_height":1,"source":"s"}
"#,
    ))
    .err()
    .unwrap();
    assert!(matches!(err, ReadError::UnsupportedVersion(99)), "{err:?}");
}

#[test]
fn reader_rejects_a_height_gap() {
    let text = r#"{"kind":"header","format_version":1,"chain":"m","asset":"A","px_decimals":0,"sz_decimals":0,"first_height":10,"source":"s"}
{"kind":"block","height":10,"ops":[]}
{"kind":"block","height":12,"ops":[]}
"#;
    let err = Reader::new(Cursor::new(text))
        .unwrap()
        .steps()
        .collect::<Result<Vec<_>, _>>()
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            ReadError::HeightGap {
                expected: 11,
                found: 12,
                line: 3
            }
        ),
        "{err:?}"
    );
}

#[test]
fn reader_rejects_a_first_block_that_does_not_match_the_header() {
    let text = r#"{"kind":"header","format_version":1,"chain":"m","asset":"A","px_decimals":0,"sz_decimals":0,"first_height":10,"source":"s"}
{"kind":"block","height":11,"ops":[]}
"#;
    let err = Reader::new(Cursor::new(text))
        .unwrap()
        .steps()
        .collect::<Result<Vec<_>, _>>()
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            ReadError::HeightGap {
                expected: 10,
                found: 11,
                line: 2
            }
        ),
        "{err:?}"
    );
}

#[test]
fn reader_rejects_a_book_that_does_not_follow_its_block() {
    let text = r#"{"kind":"header","format_version":1,"chain":"m","asset":"A","px_decimals":0,"sz_decimals":0,"first_height":10,"source":"s"}
{"kind":"block","height":10,"ops":[]}
{"kind":"book","height":11,"bids":[],"asks":[]}
"#;
    let err = Reader::new(Cursor::new(text))
        .unwrap()
        .steps()
        .collect::<Result<Vec<_>, _>>()
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            ReadError::BookWithoutBlock {
                height: 11,
                line: 3
            }
        ),
        "{err:?}"
    );
    let text = r#"{"kind":"header","format_version":1,"chain":"m","asset":"A","px_decimals":0,"sz_decimals":0,"first_height":10,"source":"s"}
{"kind":"book","height":10,"bids":[],"asks":[]}
"#;
    let err = Reader::new(Cursor::new(text))
        .unwrap()
        .steps()
        .collect::<Result<Vec<_>, _>>()
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            ReadError::BookWithoutBlock {
                height: 10,
                line: 2
            }
        ),
        "{err:?}"
    );
}

#[test]
fn reader_reports_malformed_json_with_its_line_number() {
    let text = r#"{"kind":"header","format_version":1,"chain":"m","asset":"A","px_decimals":0,"sz_decimals":0,"first_height":10,"source":"s"}
{"kind":"block","height":10,"ops":[]}
not json
"#;
    let err = Reader::new(Cursor::new(text))
        .unwrap()
        .steps()
        .collect::<Result<Vec<_>, _>>()
        .err()
        .unwrap();
    assert!(matches!(err, ReadError::Json { line: 3, .. }), "{err:?}");
}

#[test]
fn writer_enforces_contiguity_and_book_placement() {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header()).unwrap();
    let err = w.write_book(&BookSnapshot::default()).err().unwrap();
    assert!(matches!(err, WriteError::BookWithoutBlock), "{err:?}");
    let err = w
        .write_block(&Block {
            height: 101,
            time_ms: None,
            ops: vec![],
        })
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            WriteError::HeightGap {
                expected: 100,
                found: 101
            }
        ),
        "{err:?}"
    );
    w.write_block(&Block {
        height: 100,
        time_ms: None,
        ops: vec![],
    })
    .unwrap();
    w.write_book(&BookSnapshot::default()).unwrap();
    let err = w.write_book(&BookSnapshot::default()).err().unwrap();
    assert!(matches!(err, WriteError::DuplicateBook), "{err:?}");
}
