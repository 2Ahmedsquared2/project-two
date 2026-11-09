//! CI/CD review of PR #1 (items 2, 3, 4). Written before the fix.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::io::Cursor;

use hl_dataset::convert::{assets_from_meta, convert, ConvertError};
use hl_dataset::{
    AssetMeta, Block, BookState, Header, L2Book, L2Level, L4Book, L4Level, L4Order, ReadError,
    Reader, RuleError, WriteError, Writer, FORMAT_VERSION,
};
use proptest::prelude::*;

fn btc() -> AssetMeta {
    AssetMeta::new(0, "BTC", 5, false).unwrap() // 1 price decimal
}

fn header(first_height: u64) -> Header {
    Header {
        format_version: FORMAT_VERSION,
        chain: "m".to_owned(),
        first_height,
        source: "s".to_owned(),
        assets: vec![btc()],
        extra: BTreeMap::default(),
    }
}

fn block(height: u64) -> Block {
    Block {
        height,
        time_ms: None,
        hash: None,
        proposer: None,
        txs: vec![],
        ops: vec![],
    }
}

fn read_all(bytes: &[u8]) -> Result<usize, hl_dataset::ReadError> {
    let r = Reader::new(Cursor::new(bytes))?;
    let mut n = 0;
    for s in r.steps() {
        s?;
        n += 1;
    }
    Ok(n)
}

// ---- item 3: the maximum height ----

#[test]
fn the_writer_accepts_a_block_at_u64_max_and_refuses_one_after_it() {
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header(u64::MAX)).unwrap();
    w.write_block(&block(u64::MAX)).unwrap();
    let err = w.write_block(&block(u64::MAX)).unwrap_err();
    assert!(
        matches!(err, WriteError::Rule(RuleError::HeightOverflow(u64::MAX))),
        "{err:?}"
    );
    w.finish().unwrap();
    assert_eq!(read_all(&out).unwrap(), 1);
}

#[test]
fn the_reader_accepts_a_block_at_u64_max_and_refuses_one_after_it() {
    let max = u64::MAX;
    let text = format!(
        "{{\"kind\":\"header\",\"format_version\":2,\"chain\":\"m\",\"first_height\":{max},\"source\":\"s\",\"assets\":[]}}\n{{\"kind\":\"block\",\"height\":{max}}}\n{{\"kind\":\"block\",\"height\":{max}}}\n"
    );
    let err = read_all(text.as_bytes()).unwrap_err();
    assert!(
        matches!(
            err,
            ReadError::Rule {
                line: 3,
                source: RuleError::HeightOverflow(u64::MAX)
            }
        ),
        "{err:?}"
    );
}

// ---- item 4: the writer never emits what the reader rejects ----

fn l2(bids: &[(u64, &str)]) -> BookState {
    BookState::L2(L2Book {
        height: 1,
        asset: 0,
        time_ms: None,
        depth: None,
        bids: bids
            .iter()
            .map(|&(px_ticks, px)| L2Level {
                px_ticks,
                sz_lots: 1,
                n: 1,
                px: Some(px.to_owned()),
                sz: None,
            })
            .collect(),
        asks: vec![],
    })
}

fn writer_after_block() -> Writer<Vec<u8>> {
    let mut w = Writer::new(Vec::new(), &header(1)).unwrap();
    w.write_block(&block(1)).unwrap();
    w
}

#[test]
fn the_writer_rejects_unsorted_levels() {
    let book = l2(&[(1000, "100"), (1010, "101")]);
    let err = writer_after_block().write_book(&book).unwrap_err();
    assert!(
        matches!(
            err,
            WriteError::Rule(RuleError::UnsortedLevels {
                side: "bids",
                px_ticks: 1010
            })
        ),
        "{err:?}"
    );
    // The reader reports the same rule for the same record.
    let BookState::L2(b) = book else {
        unreachable!()
    };
    let text = format!(
        "{{\"kind\":\"header\",\"format_version\":2,\"chain\":\"m\",\"first_height\":1,\"source\":\"s\",\"assets\":[{}]}}\n{{\"kind\":\"block\",\"height\":1}}\n{{\"kind\":\"l2_book\",{}\n",
        serde_json::to_string(&btc()).unwrap(),
        &serde_json::to_string(&b).unwrap()[1..]
    );
    let err = read_all(text.as_bytes()).unwrap_err();
    assert!(
        matches!(
            err,
            ReadError::Rule {
                line: 3,
                source: RuleError::UnsortedLevels {
                    side: "bids",
                    px_ticks: 1010
                }
            }
        ),
        "{err:?}"
    );
}

#[test]
fn the_writer_rejects_a_decimal_that_disagrees_with_its_integer() {
    let err = writer_after_block()
        .write_book(&l2(&[(1000, "99")]))
        .unwrap_err();
    assert!(err.to_string().contains("decimal"), "{err}");
}

#[test]
fn an_l4_level_whose_total_overflows_is_rejected_on_write_and_read() {
    let book = L4Book {
        height: 1,
        asset: 0,
        time_ms: None,
        bids: vec![L4Level {
            px_ticks: 1000,
            px: None,
            orders: vec![
                L4Order {
                    oid: 1,
                    sz_lots: u64::MAX,
                    user: None,
                },
                L4Order {
                    oid: 2,
                    sz_lots: 1,
                    user: None,
                },
            ],
        }],
        asks: vec![],
    };
    let err = writer_after_block()
        .write_book(&BookState::L4(book.clone()))
        .unwrap_err();
    assert!(err.to_string().contains("overflow"), "{err}");
    let text = format!(
        "{{\"kind\":\"header\",\"format_version\":2,\"chain\":\"m\",\"first_height\":1,\"source\":\"s\",\"assets\":[{}]}}\n{{\"kind\":\"block\",\"height\":1}}\n{{\"kind\":\"l4_book\",{}\n",
        serde_json::to_string(&btc()).unwrap(),
        &serde_json::to_string(&book).unwrap()[1..]
    );
    let err = read_all(text.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("overflow"), "{err}");
}

const META: &str = r#"{"meta":{"universe":[{"szDecimals":5,"name":"BTC","maxLeverage":40}]},"spotMeta":{"universe":[],"tokens":[]}}"#;
const BLOCKS: &str = concat!(
    r#"{"height": 100, "blockTime": 1000, "hash": "0xb", "proposer": "0xp", "numTxs": 0, "txs": []}"#,
    "\n"
);
// Bids 74668.0 then 74690.0: worse before better.
const UNSORTED_L2: &str = concat!(
    r#"{"height":100,"coin":"BTC","block_time_ms":1000,"src":"ws","levels":[[{"px":"74668.0","sz":"1","n":1},{"px":"74690.0","sz":"1","n":1}],[]]}"#,
    "\n"
);

#[test]
fn the_converter_refuses_an_unsorted_snapshot_instead_of_writing_it() {
    let mut out = Vec::new();
    let result = convert(
        &assets_from_meta(META).unwrap(),
        "t",
        Cursor::new(BLOCKS),
        UNSORTED_L2,
        &mut out,
    );
    // Same error class the reader gives for this record (RuleError::UnsortedLevels).
    let err = result.unwrap_err();
    assert!(
        matches!(
            err,
            ConvertError::Write(WriteError::Rule(RuleError::UnsortedLevels {
                side: "bids",
                ..
            }))
        ),
        "{err:?}"
    );
}

// ---- item 4: round-trip property ----

#[derive(Clone, Debug)]
enum Rec {
    Block(u64),
    L2 { height: u64, bids: Vec<(u64, bool)> },
    L4 { height: u64, sizes: Vec<u64> },
}

fn rec() -> impl Strategy<Value = Rec> {
    prop_oneof![
        (0u64..6).prop_map(Rec::Block),
        (
            0u64..6,
            prop::collection::vec((995u64..1005, any::<bool>()), 0..4)
        )
            .prop_map(|(height, bids)| Rec::L2 { height, bids }),
        (
            0u64..6,
            prop::collection::vec(prop_oneof![1u64..10, Just(u64::MAX)], 0..3)
        )
            .prop_map(|(height, sizes)| Rec::L4 { height, sizes }),
    ]
}

fn to_book(r: &Rec) -> Option<BookState> {
    match r {
        Rec::Block(_) => None,
        Rec::L2 { height, bids } => Some(BookState::L2(L2Book {
            height: *height,
            asset: 0,
            time_ms: None,
            depth: None,
            bids: bids
                .iter()
                .map(|&(px_ticks, honest)| L2Level {
                    px_ticks,
                    sz_lots: 1,
                    n: 1,
                    // An honest or a wrong audit string.
                    px: Some(if honest {
                        format!("{}.{}", px_ticks / 10, px_ticks % 10)
                    } else {
                        "1".to_owned()
                    }),
                    sz: None,
                })
                .collect(),
            asks: vec![],
        })),
        Rec::L4 { height, sizes } => Some(BookState::L4(L4Book {
            height: *height,
            asset: 0,
            time_ms: None,
            bids: vec![L4Level {
                px_ticks: 1000,
                px: None,
                orders: sizes
                    .iter()
                    .enumerate()
                    .map(|(i, &sz_lots)| L4Order {
                        oid: u64::try_from(i).unwrap(),
                        sz_lots,
                        user: None,
                    })
                    .collect(),
            }],
            asks: vec![],
        })),
    }
}

proptest! {
    /// write(x) ok => read(write(x)) ok: whatever the writer accepts, the
    /// reader accepts, record for record.
    #[test]
    fn whatever_the_writer_accepts_the_reader_reads(recs in prop::collection::vec(rec(), 0..12)) {
        let mut w = Writer::new(Vec::new(), &header(0)).unwrap();
        let mut blocks = 0usize;
        for r in &recs {
            match r {
                Rec::Block(h) => if w.write_block(&block(*h)).is_ok() { blocks += 1 },
                other => { let _ = w.write_book(&to_book(other).unwrap()); }
            }
        }
        let out = w.finish().unwrap();
        prop_assert_eq!(read_all(&out).map_err(|e| e.to_string()), Ok(blocks));
    }
}
