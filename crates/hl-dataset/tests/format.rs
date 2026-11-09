//! Round-trip and validation tests for dataset format v2 (Hyperliquid Brief #1).
//! Written before the implementation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Cursor;

use hl_book_ref::{BookOp, Lots, Oid, Order, Px, Side};
use hl_dataset::{
    AssetMeta, AssetOp, Block, BookState, Header, L2Book, L2Level, L4Book, L4Level, L4Order,
    RawJson, ReadError, Reader, Step, Tx, WriteError, Writer, FORMAT_VERSION,
};

fn btc() -> AssetMeta {
    AssetMeta::new(0, "BTC", 5, false).unwrap()
}

fn header() -> Header {
    Header {
        format_version: FORMAT_VERSION,
        chain: "mainnet".to_owned(),
        first_height: 100,
        source: "hand-built".to_owned(),
        assets: vec![btc()],
        extra: std::collections::BTreeMap::default(),
    }
}

fn header_line(first_height: u64) -> String {
    format!(
        r#"{{"kind":"header","format_version":2,"chain":"m","first_height":{first_height},"source":"s","assets":[{{"asset_index":0,"name":"BTC","sz_decimals":5,"is_spot":false,"max_price_decimals":1}}]}}"#
    )
}

fn insert(oid: u64, side: Side, px: u64, sz: u64) -> AssetOp {
    AssetOp {
        asset: 0,
        op: BookOp::Insert(Order {
            oid: Oid(oid),
            side,
            px: Px(px),
            sz: Lots(sz),
        }),
    }
}

fn tx(action: &str) -> Tx {
    Tx {
        time_ms: Some(1_700_000_000_000),
        user: "0x0000000000000000000000000000000000000001".to_owned(),
        hash: "0xabc".to_owned(),
        error: None,
        action: RawJson::from_string(action.to_owned()).unwrap(),
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

fn l4(height: u64, bids: &[(u64, &[(u64, u64)])], asks: &[(u64, &[(u64, u64)])]) -> BookState {
    let levels = |ls: &[(u64, &[(u64, u64)])]| -> Vec<L4Level> {
        ls.iter()
            .map(|&(px, os)| L4Level {
                px_ticks: px,
                px: None,
                orders: os
                    .iter()
                    .map(|&(oid, sz)| L4Order {
                        oid,
                        sz_lots: sz,
                        user: None,
                    })
                    .collect(),
            })
            .collect()
    };
    BookState::L4(L4Book {
        height,
        asset: 0,
        time_ms: None,
        bids: levels(bids),
        asks: levels(asks),
    })
}

fn l2_level(px_ticks: u64, sz_lots: u64, n: u32, px: &str, sz: &str) -> L2Level {
    L2Level {
        px_ticks,
        sz_lots,
        n,
        px: Some(px.to_owned()),
        sz: Some(sz.to_owned()),
    }
}

fn two_block_dataset() -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header()).unwrap();
    let mut b = block(
        100,
        vec![insert(1, Side::Bid, 1000, 5), insert(2, Side::Ask, 1010, 3)],
    );
    b.time_ms = Some(1_700_000_000_000);
    b.txs = vec![tx(r#"{"type":"noop","nonce":1}"#)];
    w.write_block(&b).unwrap();
    w.write_book(&l4(100, &[(1000, &[(1, 5)])], &[(1010, &[(2, 3)])]))
        .unwrap();
    w.write_block(&block(
        101,
        vec![AssetOp {
            asset: 0,
            op: BookOp::Resize {
                oid: Oid(1),
                sz: Lots(2),
            },
        }],
    ))
    .unwrap();
    w.write_book(&BookState::L2(L2Book {
        height: 101,
        asset: 0,
        time_ms: Some(1_700_000_000_500),
        depth: Some(20),
        bids: vec![l2_level(1000, 2, 1, "100.0", "0.00002")],
        asks: vec![l2_level(1010, 3, 1, "101", "0.00003")],
    }))
    .unwrap();
    w.finish().unwrap();
    out
}

fn read_all(bytes: impl AsRef<[u8]>) -> Result<Vec<Step>, ReadError> {
    Reader::new(Cursor::new(bytes.as_ref().to_vec()))?
        .steps()
        .collect()
}

// ---- round trip and wire shape ----

#[test]
fn round_trips_header_blocks_txs_and_both_book_kinds() {
    let reader = Reader::new(Cursor::new(two_block_dataset())).unwrap();
    assert_eq!(reader.header(), &header());
    let steps: Vec<Step> = reader.steps().collect::<Result<_, _>>().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].block.txs.len(), 1);
    assert_eq!(steps[0].block.ops.len(), 2);
    assert_eq!(
        steps[0].books,
        vec![l4(100, &[(1000, &[(1, 5)])], &[(1010, &[(2, 3)])])]
    );
    let BookState::L2(l2) = &steps[1].books[0] else {
        panic!("expected an L2 book");
    };
    assert_eq!(l2.depth, Some(20));
    assert_eq!(l2.bids[0].sz_lots, 2);
}

#[test]
fn the_wire_shape_is_documented_json_lines() {
    let text = String::from_utf8(two_block_dataset()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "header, block, l4_book, block, l2_book");
    assert_eq!(
        lines[0],
        r#"{"kind":"header","format_version":2,"chain":"mainnet","first_height":100,"source":"hand-built","assets":[{"asset_index":0,"name":"BTC","sz_decimals":5,"is_spot":false,"max_price_decimals":1}]}"#
    );
    assert_eq!(
        lines[1],
        r#"{"kind":"block","height":100,"time_ms":1700000000000,"txs":[{"time_ms":1700000000000,"user":"0x0000000000000000000000000000000000000001","hash":"0xabc","error":null,"action":{"type":"noop","nonce":1}}],"ops":[{"asset":0,"op":"insert","oid":1,"side":"bid","px":1000,"sz":5},{"asset":0,"op":"insert","oid":2,"side":"ask","px":1010,"sz":3}]}"#
    );
    assert_eq!(
        lines[2],
        r#"{"kind":"l4_book","height":100,"asset":0,"bids":[{"px_ticks":1000,"orders":[{"oid":1,"sz_lots":5}]}],"asks":[{"px_ticks":1010,"orders":[{"oid":2,"sz_lots":3}]}]}"#
    );
    assert_eq!(
        lines[3],
        r#"{"kind":"block","height":101,"txs":[],"ops":[{"asset":0,"op":"resize","oid":1,"sz":2}]}"#
    );
    assert_eq!(
        lines[4],
        r#"{"kind":"l2_book","height":101,"asset":0,"time_ms":1700000000500,"depth":20,"bids":[{"px_ticks":1000,"sz_lots":2,"n":1,"px":"100.0","sz":"0.00002"}],"asks":[{"px_ticks":1010,"sz_lots":3,"n":1,"px":"101","sz":"0.00003"}]}"#
    );
}

#[test]
fn raw_actions_round_trip_byte_for_byte() {
    // Key order, spacing inside the action and decimal strings must survive untouched:
    // the action is what was signed, and later phases hash it.
    let action = r#"{"type":"order","orders":[{"a":0,"b":true,"p":"100.50","s":"0.00010","r":false,"t":{"limit":{"tif":"Gtc"}}}],"grouping":"na"}"#;
    let block_line = format!(
        r#"{{"kind":"block","height":7,"txs":[{{"user":"0x1","hash":"0x2","error":"Order has invalid price.","action":{action}}}],"ops":[]}}"#
    );
    let text = format!("{}\n{block_line}\n", header_line(7));
    let steps = read_all(&text).unwrap();
    let tx = &steps[0].block.txs[0];
    assert_eq!(tx.action.get(), action);
    assert_eq!(tx.error.as_deref(), Some("Order has invalid price."));
    assert_eq!(tx.time_ms, None);
    // Re-write and compare the action bytes.
    let reader = Reader::new(Cursor::new(text.clone())).unwrap();
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, reader.header()).unwrap();
    w.write_block(&steps[0].block).unwrap();
    w.finish().unwrap();
    assert!(String::from_utf8(out).unwrap().contains(action));
}

#[test]
fn unknown_header_fields_survive_a_round_trip() {
    let line = r#"{"kind":"header","format_version":2,"chain":"mainnet","first_height":7,"source":"x","assets":[],"capture_tool":"hl-capture 0.1"}"#;
    let reader = Reader::new(Cursor::new(format!("{line}\n"))).unwrap();
    assert_eq!(
        reader.header().extra.get("capture_tool"),
        Some(&serde_json::Value::String("hl-capture 0.1".to_owned()))
    );
    assert!(!reader.header().extra.contains_key("kind"));
    let mut out = Vec::new();
    Writer::new(&mut out, reader.header())
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(String::from_utf8(out).unwrap().trim_end(), line);
}

#[test]
fn l4_books_convert_to_and_from_reference_snapshots() {
    let BookState::L4(book) = l4(5, &[(1000, &[(1, 5), (3, 2)])], &[(1010, &[(2, 3)])]) else {
        unreachable!()
    };
    let snap = book.to_snapshot();
    assert_eq!(
        snap.bids[0].orders,
        vec![(Oid(1), Lots(5)), (Oid(3), Lots(2))]
    );
    assert_eq!(L4Book::from_snapshot(5, 0, &snap), book);
}

// ---- asset meta and decimals ----

#[test]
fn asset_meta_derives_max_price_decimals_from_the_hyperliquid_rule() {
    // perp: 6 - szDecimals, spot: 8 - szDecimals.
    assert_eq!(
        AssetMeta::new(0, "BTC", 5, false)
            .unwrap()
            .max_price_decimals,
        1
    );
    assert_eq!(
        AssetMeta::new(1, "ETH", 4, false)
            .unwrap()
            .max_price_decimals,
        2
    );
    assert_eq!(
        AssetMeta::new(10_000, "PURR/USDC", 0, true)
            .unwrap()
            .max_price_decimals,
        8
    );
    assert!(AssetMeta::new(2, "X", 7, false).is_err());
}

#[test]
fn asset_meta_turns_decimal_strings_into_ticks_and_lots() {
    let m = btc();
    assert_eq!(m.px_ticks("100.5").unwrap(), 1005);
    assert_eq!(m.px_ticks("100").unwrap(), 1000);
    assert_eq!(m.px_ticks("100.50").unwrap(), 1005);
    assert!(m.px_ticks("100.55").is_err());
    assert_eq!(m.sz_lots("0.00012").unwrap(), 12);
    assert_eq!(m.sz_lots("1").unwrap(), 100_000);
}

#[test]
fn decimal_parsing_is_strict() {
    use hl_dataset::decimal::{format_units, parse_units, DecimalError};
    assert_eq!(parse_units("0", 3).unwrap(), 0);
    assert_eq!(parse_units("1.234", 3).unwrap(), 1234);
    assert_eq!(parse_units("1.2", 3).unwrap(), 1200);
    assert_eq!(parse_units("1.2340", 3).unwrap(), 1234);
    assert_eq!(parse_units("1.2345", 3), Err(DecimalError::TooPrecise));
    for bad in [
        "", ".", "1.", ".5", "-1", "+1", "1e3", "1,0", " 1", "1.2.3", "NaN",
    ] {
        assert_eq!(parse_units(bad, 3), Err(DecimalError::Invalid), "{bad:?}");
    }
    assert_eq!(
        parse_units("18446744073709551616", 0),
        Err(DecimalError::Overflow)
    );
    assert_eq!(
        parse_units("18446744073709551.616", 3),
        Err(DecimalError::Overflow)
    );
    assert_eq!(format_units(1005, 1), "100.5");
    assert_eq!(format_units(1000, 1), "100");
    assert_eq!(format_units(12, 5), "0.00012");
    assert_eq!(format_units(0, 2), "0");
}

proptest::proptest! {
    #[test]
    fn decimal_format_then_parse_round_trips(v in 0u64..u64::MAX, d in 0u32..10) {
        use hl_dataset::decimal::{format_units, parse_units};
        proptest::prop_assert_eq!(parse_units(&format_units(v, d), d), Ok(v));
    }
}

// ---- reader validation ----

#[test]
fn reader_rejects_a_file_without_a_header() {
    let err = Reader::new(Cursor::new("{\"kind\":\"block\",\"height\":1}\n"))
        .err()
        .unwrap();
    assert!(matches!(err, ReadError::MissingHeader), "{err:?}");
    let err = Reader::new(Cursor::new("")).err().unwrap();
    assert!(matches!(err, ReadError::MissingHeader), "{err:?}");
}

#[test]
fn reader_rejects_unsupported_versions_including_v1() {
    for v in [1, 99] {
        let line =
            header_line(1).replace("\"format_version\":2", &format!("\"format_version\":{v}"));
        let err = Reader::new(Cursor::new(format!("{line}\n"))).err().unwrap();
        assert!(
            matches!(err, ReadError::UnsupportedVersion(x) if x == v),
            "{err:?}"
        );
    }
}

#[test]
fn reader_rejects_inconsistent_or_duplicate_asset_meta() {
    let line = header_line(1).replace("\"max_price_decimals\":1", "\"max_price_decimals\":2");
    let err = Reader::new(Cursor::new(format!("{line}\n"))).err().unwrap();
    assert!(
        matches!(err, ReadError::BadAssetMeta { asset: 0, .. }),
        "{err:?}"
    );
    let a =
        r#"{"asset_index":0,"name":"BTC","sz_decimals":5,"is_spot":false,"max_price_decimals":1}"#;
    let line = format!(
        r#"{{"kind":"header","format_version":2,"chain":"m","first_height":1,"source":"s","assets":[{a},{a}]}}"#
    );
    let err = Reader::new(Cursor::new(format!("{line}\n"))).err().unwrap();
    assert!(
        matches!(err, ReadError::BadAssetMeta { asset: 0, .. }),
        "{err:?}"
    );
}

#[test]
fn reader_rejects_a_height_gap_and_a_wrong_first_height() {
    let text = format!(
        "{}\n{{\"kind\":\"block\",\"height\":10}}\n{{\"kind\":\"block\",\"height\":12}}\n",
        header_line(10)
    );
    let err = read_all(&text).err().unwrap();
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
    let text = format!(
        "{}\n{{\"kind\":\"block\",\"height\":11}}\n",
        header_line(10)
    );
    let err = read_all(&text).err().unwrap();
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
fn reader_rejects_misplaced_or_duplicate_books() {
    let book =
        |h: u64| format!(r#"{{"kind":"l4_book","height":{h},"asset":0,"bids":[],"asks":[]}}"#);
    // Book for a height other than the block it follows.
    let text = format!(
        "{}\n{{\"kind\":\"block\",\"height\":10}}\n{}\n",
        header_line(10),
        book(11)
    );
    let err = read_all(&text).err().unwrap();
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
    // Book before any block.
    let text = format!("{}\n{}\n", header_line(10), book(10));
    let err = read_all(&text).err().unwrap();
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
    // Two books for the same asset after one block.
    let text = format!(
        "{}\n{{\"kind\":\"block\",\"height\":10}}\n{}\n{}\n",
        header_line(10),
        book(10),
        book(10)
    );
    let err = read_all(&text).err().unwrap();
    assert!(
        matches!(err, ReadError::DuplicateBook { asset: 0, line: 4 }),
        "{err:?}"
    );
}

#[test]
fn reader_rejects_undeclared_assets() {
    let text = format!(
        "{}\n{{\"kind\":\"block\",\"height\":10,\"ops\":[{{\"asset\":3,\"op\":\"cancel\",\"oid\":1}}]}}\n",
        header_line(10)
    );
    let err = read_all(&text).err().unwrap();
    assert!(
        matches!(err, ReadError::UnknownAsset { asset: 3, line: 2 }),
        "{err:?}"
    );
    let text = format!(
        "{}\n{{\"kind\":\"block\",\"height\":10}}\n{{\"kind\":\"l2_book\",\"height\":10,\"asset\":4,\"bids\":[],\"asks\":[]}}\n",
        header_line(10)
    );
    let err = read_all(&text).err().unwrap();
    assert!(
        matches!(err, ReadError::UnknownAsset { asset: 4, line: 3 }),
        "{err:?}"
    );
}

#[test]
fn reader_audits_decimal_strings_against_their_integers() {
    // "100.5" is 1005 ticks for BTC, not 1000.
    let text = format!(
        "{}\n{{\"kind\":\"block\",\"height\":10}}\n{{\"kind\":\"l2_book\",\"height\":10,\"asset\":0,\"bids\":[{{\"px_ticks\":1000,\"sz_lots\":1,\"n\":1,\"px\":\"100.5\"}}],\"asks\":[]}}\n",
        header_line(10)
    );
    let err = read_all(&text).err().unwrap();
    assert!(
        matches!(err, ReadError::DecimalMismatch { line: 3, .. }),
        "{err:?}"
    );
}

#[test]
fn reader_rejects_unsorted_levels() {
    // Bids must be strictly descending.
    let text = format!(
        "{}\n{{\"kind\":\"block\",\"height\":10}}\n{{\"kind\":\"l2_book\",\"height\":10,\"asset\":0,\"bids\":[{{\"px_ticks\":999,\"sz_lots\":1,\"n\":1}},{{\"px_ticks\":1000,\"sz_lots\":1,\"n\":1}}],\"asks\":[]}}\n",
        header_line(10)
    );
    let err = read_all(&text).err().unwrap();
    assert!(
        matches!(err, ReadError::UnsortedLevels { line: 3, .. }),
        "{err:?}"
    );
}

#[test]
fn reader_reports_malformed_json_and_unknown_kinds_with_line_numbers() {
    let text = format!(
        "{}\n{{\"kind\":\"block\",\"height\":10}}\nnot json\n",
        header_line(10)
    );
    let err = read_all(&text).err().unwrap();
    assert!(matches!(err, ReadError::Json { line: 3, .. }), "{err:?}");
    let text = format!("{}\n{{\"kind\":\"mystery\"}}\n", header_line(10));
    let err = read_all(&text).err().unwrap();
    assert!(
        matches!(err, ReadError::UnknownKind { line: 2, .. }),
        "{err:?}"
    );
}

#[test]
fn a_block_without_books_yields_a_step_with_none() {
    let text = format!(
        "{}\n{{\"kind\":\"block\",\"height\":10}}\n{{\"kind\":\"block\",\"height\":11}}\n",
        header_line(10)
    );
    let steps = read_all(&text).unwrap();
    assert_eq!(steps.len(), 2);
    assert!(steps.iter().all(|s| s.books.is_empty()));
}

// ---- writer validation ----

#[test]
fn writer_enforces_contiguity_book_placement_and_declared_assets() {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header()).unwrap();
    let err = w.write_book(&l4(100, &[], &[])).err().unwrap();
    assert!(matches!(err, WriteError::BookWithoutBlock), "{err:?}");
    let err = w.write_block(&block(101, vec![])).err().unwrap();
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
    let err = w
        .write_block(&block(
            100,
            vec![AssetOp {
                asset: 9,
                op: BookOp::Cancel { oid: Oid(1) },
            }],
        ))
        .err()
        .unwrap();
    assert!(matches!(err, WriteError::UnknownAsset(9)), "{err:?}");
    w.write_block(&block(100, vec![])).unwrap();
    let err = w.write_book(&l4(99, &[], &[])).err().unwrap();
    assert!(matches!(err, WriteError::BookWithoutBlock), "{err:?}");
    w.write_book(&l4(100, &[], &[])).unwrap();
    let err = w.write_book(&l4(100, &[], &[])).err().unwrap();
    assert!(matches!(err, WriteError::DuplicateBook(0)), "{err:?}");
}
