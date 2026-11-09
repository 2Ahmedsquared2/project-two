//! CI/CD review of PR #1 at 0e10c31 (items 1 and 2). Written before the fix.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Cursor;

use book_check::replay::{ReplayConfig, Replayer};
use book_check::{check, compare_l4, Candidate, CheckError, Field, LevelDiff, Which};
use hl_book_ref::{BookOp, BookSnapshot, Level, Lots, Oid, Px, Side};
use hl_dataset::{
    AssetMeta, Block, BookState, Header, L2Book, L2Level, RawJson, Reader, Tx, Writer,
    FORMAT_VERSION,
};

fn assets() -> Vec<AssetMeta> {
    vec![AssetMeta::new(0, "GOLD", 5, false).unwrap()]
}

fn tx(user: &str, action: &str) -> Tx {
    Tx {
        time_ms: None,
        user: user.to_owned(),
        hash: "0x".to_owned(),
        error: None,
        action: RawJson::from_string(action.to_owned()).unwrap(),
    }
}

fn block(height: u64, txs: Vec<Tx>) -> Block {
    Block {
        height,
        time_ms: None,
        hash: None,
        proposer: None,
        txs,
        ops: vec![],
    }
}

fn gtc(px: &str, sz: &str, cloid: &str) -> String {
    format!(
        r#"{{"type":"order","orders":[{{"a":0,"b":true,"p":"{px}","s":"{sz}","r":false,"t":{{"limit":{{"tif":"Gtc"}}}},"c":"{cloid}"}}],"grouping":"na"}}"#
    )
}

fn modify(oid_ref: &str, a: u32, px: &str, sz: &str, tif: &str) -> String {
    format!(
        r#"{{"type":"modify","oid":{oid_ref},"order":{{"a":{a},"b":true,"p":"{px}","s":"{sz}","r":false,"t":{{"limit":{{"tif":"{tif}"}}}},"c":"0x99"}}}}"#
    )
}

/// Two resting bids at 99.0: oid 1 (user A, cloid 0x01) then oid 2 (user B).
fn two_resting() -> Replayer {
    let mut r = Replayer::new(&assets(), ReplayConfig::default());
    r.apply_block(&block(
        1,
        vec![
            tx("0xa", &gtc("99", "1", "0x01")),
            tx("0xb", &gtc("99", "2", "0x02")),
        ],
    ));
    r
}

fn snapshot(r: &Replayer) -> BookSnapshot {
    r.book(0).unwrap().snapshot()
}

// ---- item 1: an invalid modify leaves the original order alone ----

#[test]
fn an_invalid_modify_by_cloid_leaves_the_original_order_and_cloid_untouched() {
    // ACTION_ORDERING.md §4.5: the replacement leg is rejected per order, so
    // the original order keeps its place, oid and cloid (INFERENCE).
    let bad_legs = [
        modify(r#""0x01""#, 0, "99.05", "1", "Gtc"), // too many price decimals
        modify(r#""0x01""#, 0, "12345.6", "1", "Gtc"), // 6 significant figures
        modify(r#""0x01""#, 0, "99", "0.000001", "Gtc"), // finer than the lot
        modify(r#""0x01""#, 7, "99", "1", "Gtc"),    // unknown asset
        modify(r#""0x01""#, 0, "99", "1", "Xyz"),    // unknown tif
    ];
    for bad in &bad_legs {
        let mut r = two_resting();
        let before = snapshot(&r);
        r.apply_block(&block(2, vec![tx("0xa", bad)]));
        assert_eq!(snapshot(&r), before, "book changed by {bad}");
        assert_eq!(r.cloid_order("0xa", "0x01"), Some((0, Oid(1))), "{bad}");
        assert_eq!(r.cloid_order("0xa", "0x99"), None, "{bad}");
        // No oid consumed: the next accepted leg gets oid 3.
        r.apply_block(&block(3, vec![tx("0xc", &gtc("98", "1", "0x03"))]));
        assert_eq!(r.book(0).unwrap().get(Oid(3)).map(|o| o.px), Some(Px(980)));
        // The original still cancels by its cloid.
        r.apply_block(&block(
            4,
            vec![tx(
                "0xa",
                r#"{"type":"cancelByCloid","cancels":[{"asset":0,"cloid":"0x01"}]}"#,
            )],
        ));
        assert_eq!(r.stats().cancel_by_cloid_resolved, 1, "{bad}");
    }
}

#[test]
fn an_invalid_modify_by_oid_leaves_the_book_untouched() {
    let mut r = two_resting();
    let before = snapshot(&r);
    r.apply_block(&block(
        2,
        vec![tx("0xa", &modify("1", 0, "99.05", "1", "Gtc"))],
    ));
    assert_eq!(snapshot(&r), before);
    assert_eq!(r.cloid_order("0xa", "0x01"), Some((0, Oid(1))));
}

#[test]
fn a_valid_modify_still_cancels_and_places_with_a_new_oid() {
    let mut r = two_resting();
    r.apply_block(&block(
        2,
        vec![tx("0xa", &modify(r#""0x01""#, 0, "99", "0.5", "Gtc"))],
    ));
    let level = r.book(0).unwrap().level(Side::Bid, Px(990)).unwrap();
    assert_eq!(
        level.orders,
        vec![(Oid(2), Lots(200_000)), (Oid(3), Lots(50_000))]
    );
    assert_eq!(r.cloid_order("0xa", "0x01"), None);
    assert_eq!(r.cloid_order("0xa", "0x99"), Some((0, Oid(3))));
}

// ---- item 2: size sums never wrap into a false equality ----

/// A candidate whose only level holds `u64::MAX + 1` lots in two orders.
struct Overflowing;

impl Candidate for Overflowing {
    type Error = String;

    fn apply(&mut self, _: &BookOp) -> Result<(), String> {
        Ok(())
    }

    fn snapshot(&self) -> BookSnapshot {
        BookSnapshot {
            bids: vec![Level {
                px: Px(100),
                orders: vec![(Oid(1), Lots(u64::MAX)), (Oid(2), Lots(1))],
            }],
            asks: vec![],
        }
    }
}

#[test]
fn an_overflowing_candidate_level_is_never_certified_equal() {
    // Recorded: 0 lots in 2 orders, which is what u64::MAX + 1 wraps to.
    let header = Header {
        format_version: FORMAT_VERSION,
        chain: "m".to_owned(),
        first_height: 1,
        source: "s".to_owned(),
        assets: assets(),
        extra: std::collections::BTreeMap::default(),
    };
    let mut out = Vec::new();
    let mut w = Writer::new(&mut out, &header).unwrap();
    w.write_block(&block(1, vec![])).unwrap();
    w.write_book(&BookState::L2(L2Book {
        height: 1,
        asset: 0,
        time_ms: None,
        depth: None,
        bids: vec![L2Level {
            px_ticks: 100,
            sz_lots: 0,
            n: 2,
            px: None,
            sz: None,
        }],
        asks: vec![],
    }))
    .unwrap();
    w.finish().unwrap();
    let reader = Reader::new(Cursor::new(out)).unwrap();
    let result = check(reader, |_| Overflowing);
    match result {
        Err(CheckError::Divergence(d)) => {
            assert_eq!((d.height, d.side, d.px), (1, Side::Bid, Px(100)));
            assert_eq!(
                d.field,
                Field::SizeOverflow {
                    book: Which::Candidate
                }
            );
        }
        other => panic!("expected a size-overflow divergence, got {other:?}"),
    }
}

#[test]
fn identical_overflowing_l4_levels_are_not_equal() {
    let s = Overflowing.snapshot();
    assert_eq!(
        compare_l4(&s, &s.clone()),
        Some(LevelDiff {
            side: Side::Bid,
            px: Px(100),
            field: Field::SizeOverflow {
                book: Which::Recorded
            },
        })
    );
}
