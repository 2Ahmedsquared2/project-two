//! Node output to dataset: the hand-derived golden scenario in `golden/node/`,
//! the error and skip rules, and a model-based property test whose output
//! `book-check` must replay cleanly.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::BufReader;
use std::path::PathBuf;

use book_check::{check, CheckError, Divergence, Field};
use hl_book_ref::{Book, BookOp, Lots, Oid, Order, Px, Side};
use hl_dataset::convert::assets_from_meta;
use hl_dataset::decimal::format_units;
use hl_dataset::{AssetMeta, AssetOp, Reader, Step};
use hl_node_convert::convert::{convert, ConvertError, Options, Stats};
use hl_node_convert::snapshot::L4Snapshot;
use proptest::prelude::*;

const GOLDEN_SOURCE: &str = "hl-node node_raw_book_diffs + node_order_statuses (--stream-with-block-info), seeded from l4Snapshots";

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../golden/node")
        .join(name)
}

fn read(name: &str) -> String {
    fs::read_to_string(golden(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn snapshot(name: &str) -> L4Snapshot {
    L4Snapshot::from_json(&read(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn golden_assets() -> Vec<AssetMeta> {
    assets_from_meta(&read("meta.json")).unwrap_or_else(|e| panic!("{e}"))
}

fn golden_opts() -> Options {
    Options {
        source: GOLDEN_SOURCE.to_owned(),
        coins: Some(BTreeSet::from(["BTC".to_owned(), "PURR/USDC".to_owned()])),
        ..Options::default()
    }
}

fn run_golden(check_name: &str, opts: &Options) -> (Result<Stats, ConvertError>, Vec<u8>) {
    let mut out = Vec::new();
    let diffs = read("node_raw_book_diffs");
    let statuses = read("node_order_statuses");
    let res = convert(
        &golden_assets(),
        diffs.as_bytes(),
        Some(statuses.as_bytes()),
        Some(&snapshot("seed.json")),
        &[snapshot(check_name)],
        opts,
        &mut out,
    );
    (res, out)
}

fn parse(bytes: &[u8]) -> (hl_dataset::Header, Vec<Step>) {
    let reader = Reader::new(bytes).unwrap_or_else(|e| panic!("{e}"));
    let header = reader.header().clone();
    let steps = reader
        .steps()
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| panic!("{e}"));
    (header, steps)
}

fn book_check(bytes: &[u8]) -> Result<book_check::Report, CheckError> {
    let reader = Reader::new(BufReader::new(bytes)).unwrap_or_else(|e| panic!("{e}"));
    check(reader, |_| Book::new())
}

#[test]
fn golden_scenario_matches_the_hand_derived_dataset() {
    let (res, out) = run_golden("check.json", &golden_opts());
    let stats = res.unwrap_or_else(|e| panic!("{e}"));
    let expected = read("expected.jsonl");
    assert_eq!(parse(&out), parse(expected.as_bytes()));
    assert_eq!(
        Stats {
            first_height: 1000,
            last_height: 1003,
            blocks: 4,
            gap_blocks: 0,
            seed_orders: 7,
            seed_triggers_skipped: 1,
            diffs_before_seed: 1,
            ops: 8,
            inserts: 2,
            inserts_before: 1,
            resizes: 3,
            cancels: 2,
            zero_size_updates: 0,
            orig_sz_mismatches: 1,
            unknown_coin_diffs: 1,
            filtered_diffs: 1,
            untracked_diffs: 0,
            status_only_blocks: 1,
            check_books: 2,
            checks_out_of_range: 0,
            examples: stats.examples.clone(),
        },
        stats
    );
    assert_eq!(stats.examples.len(), 2, "{:?}", stats.examples);
    assert!(stats.examples.iter().any(|e| e.contains("xyz:FOO")));
    assert!(stats.examples.iter().any(|e| e.contains("origSz")));
}

#[test]
fn golden_output_passes_book_check() {
    let (res, out) = run_golden("check.json", &golden_opts());
    res.unwrap_or_else(|e| panic!("{e}"));
    let report = book_check(&out).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(report.blocks, 4);
    assert_eq!(report.ops, 15);
    assert_eq!(report.l4_books_compared, 2);
}

#[test]
fn corrupted_check_snapshot_is_caught_at_the_exact_queue_slot() {
    let (res, out) = run_golden("check-corrupt-queue.json", &golden_opts());
    res.unwrap_or_else(|e| panic!("{e}"));
    match book_check(&out) {
        Err(CheckError::Divergence(d)) => assert_eq!(
            d,
            Divergence {
                height: 1003,
                asset: 0,
                coin: "BTC".to_owned(),
                side: Side::Bid,
                px: Px(1_000_000),
                field: Field::QueuePosition {
                    position: 0,
                    recorded: Oid(32),
                    candidate: Oid(11),
                },
            }
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn strict_turns_soft_findings_into_errors() {
    let opts = Options {
        strict: true,
        ..golden_opts()
    };
    let (res, _) = run_golden("check.json", &opts);
    assert!(matches!(res, Err(ConvertError::Strict(_))), "{res:?}");
}

#[test]
fn no_gaps_accepts_a_block_only_the_status_stream_has() {
    // Block 1002 has no diff line but has a status line: it exists, not a gap.
    let opts = Options {
        allow_gaps: false,
        ..golden_opts()
    };
    let (res, _) = run_golden("check.json", &opts);
    res.unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn unknown_filter_coin_is_an_error() {
    let opts = Options {
        coins: Some(BTreeSet::from(["DOGE".to_owned()])),
        ..golden_opts()
    };
    let (res, _) = run_golden("check.json", &opts);
    assert!(
        matches!(res, Err(ConvertError::UnknownFilterCoin(ref c)) if c == "DOGE"),
        "{res:?}"
    );
}

// ---- small hand-made streams -------------------------------------------------

fn btc_assets() -> Vec<AssetMeta> {
    vec![AssetMeta::new(0, "BTC", 5, false).unwrap_or_else(|e| panic!("{e}"))]
}

fn ev_new(oid: u64, side: Option<&str>, px: &str, sz: &str, before: Option<u64>) -> String {
    let side = side.map_or(String::new(), |s| format!(r#""side":"{s}","#));
    let before = before.map_or(String::new(), |b| format!(r#","insertBefore":{b}"#));
    format!(
        r#"{{"user":"0xab","oid":{oid},"coin":"BTC",{side}"px":"{px}","raw_book_diff":{{"new":{{"sz":"{sz}"{before}}}}}}}"#
    )
}

fn ev_update(oid: u64, px: &str, orig: &str, new: &str) -> String {
    format!(
        r#"{{"user":"0xab","oid":{oid},"coin":"BTC","px":"{px}","raw_book_diff":{{"update":{{"origSz":"{orig}","newSz":"{new}"}}}}}}"#
    )
}

fn ev_remove(oid: u64, px: &str) -> String {
    format!(r#"{{"user":"0xab","oid":{oid},"coin":"BTC","px":"{px}","raw_book_diff":"remove"}}"#)
}

fn ev_status(oid: u64, side: &str, px: &str, status: &str, tif: &str) -> String {
    format!(
        r#"{{"time":"2026-11-19T12:00:00.100","user":"0xab","status":"{status}","order":{{"coin":"BTC","side":"{side}","limitPx":"{px}","sz":"1.0","oid":{oid},"timestamp":1,"triggerCondition":"N/A","isTrigger":false,"triggerPx":"0.0","children":[],"isPositionTpsl":false,"reduceOnly":false,"orderType":"Limit","origSz":"1.0","tif":"{tif}","cloid":null}}}}"#
    )
}

fn batch(block: u64, events: &[String]) -> String {
    format!(
        r#"{{"local_time":"2026-11-19T12:00:01","block_time":"2026-11-19T12:00:00.100","block_number":{block},"events":[{}]}}"#,
        events.join(",")
    ) + "\n"
}

fn run(
    diffs: &str,
    statuses: Option<&str>,
    seed: Option<&L4Snapshot>,
    checks: &[L4Snapshot],
    opts: &Options,
) -> (Result<Stats, ConvertError>, Vec<u8>) {
    let mut out = Vec::new();
    let res = convert(
        &btc_assets(),
        diffs.as_bytes(),
        statuses.map(str::as_bytes),
        seed,
        checks,
        opts,
        &mut out,
    );
    (res, out)
}

fn ops(out: &[u8]) -> Vec<(u64, Vec<AssetOp>)> {
    parse(out)
        .1
        .into_iter()
        .map(|s| (s.block.height, s.block.ops))
        .collect()
}

fn ins(oid: u64, side: Side, px: u64, sz: u64) -> AssetOp {
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

fn op(o: BookOp) -> AssetOp {
    AssetOp { asset: 0, op: o }
}

#[test]
fn unseeded_skips_diffs_on_orders_it_never_saw() {
    let diffs = batch(
        50,
        &[
            ev_new(1, Some("Bid"), "100.0", "1.0", None),
            ev_update(7, "100.0", "2.0", "1.0"),
            ev_remove(8, "101.0"),
            ev_new(2, Some("Bid"), "100.0", "0.5", Some(9)),
            ev_update(2, "100.0", "0.5", "0.25"),
            ev_remove(1, "100.0"),
        ],
    );
    let (res, out) = run(&diffs, None, None, &[], &Options::default());
    let stats = res.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(stats.untracked_diffs, 4, "{stats:?}");
    assert_eq!(stats.first_height, 50);
    assert_eq!(
        ops(&out),
        vec![(
            50,
            vec![
                ins(1, Side::Bid, 1000, 100_000),
                op(BookOp::Cancel { oid: Oid(1) })
            ]
        )]
    );
    book_check(&out).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn seeded_emits_every_diff_faithfully() {
    // With a seed the converter does not filter: an unknown oid reaches book-check,
    // which reports it at its block.
    let seed = L4Snapshot::from_json(r#"[49,[["BTC",[[],[]]]]]"#).unwrap_or_else(|e| panic!("{e}"));
    let diffs = batch(50, &[ev_remove(8, "101.0")]);
    let (res, out) = run(&diffs, None, Some(&seed), &[], &Options::default());
    let stats = res.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(stats.untracked_diffs, 0);
    assert_eq!(
        ops(&out),
        vec![(49, vec![]), (50, vec![op(BookOp::Cancel { oid: Oid(8) })])]
    );
    assert!(matches!(
        book_check(&out),
        Err(CheckError::Apply { height: 50, .. })
    ));
}

#[test]
fn side_comes_from_the_inserting_status_of_the_same_block() {
    let diffs = batch(5, &[ev_new(3, None, "100.0", "1.0", None)]);
    let statuses = batch(
        5,
        &[
            ev_status(3, "A", "100.0", "filled", "Ioc"),
            ev_status(3, "A", "100.0", "open", "Gtc"),
        ],
    );
    let (res, out) = run(&diffs, Some(&statuses), None, &[], &Options::default());
    res.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ops(&out), vec![(5, vec![ins(3, Side::Ask, 1000, 100_000)])]);
}

#[test]
fn new_without_any_side_is_an_error() {
    let diffs = batch(5, &[ev_new(3, None, "100.0", "1.0", None)]);
    // A status in another block, and a non-inserting one in this block, do not count.
    let statuses = batch(4, &[ev_status(3, "B", "100.0", "open", "Gtc")])
        + &batch(5, &[ev_status(3, "B", "100.0", "open", "Ioc")]);
    let (res, _) = run(&diffs, Some(&statuses), None, &[], &Options::default());
    assert!(
        matches!(res, Err(ConvertError::NoSide { height: 5, oid: 3 })),
        "{res:?}"
    );
    let (res, _) = run(&diffs, None, None, &[], &Options::default());
    assert!(matches!(res, Err(ConvertError::NoSide { .. })), "{res:?}");
}

#[test]
fn diff_side_and_status_side_must_agree() {
    let diffs = batch(5, &[ev_new(3, Some("Bid"), "100.0", "1.0", None)]);
    let statuses = batch(5, &[ev_status(3, "A", "100.0", "open", "Alo")]);
    let (res, _) = run(&diffs, Some(&statuses), None, &[], &Options::default());
    assert!(
        matches!(res, Err(ConvertError::SideMismatch { height: 5, oid: 3 })),
        "{res:?}"
    );
}

#[test]
fn gaps_fill_with_empty_blocks_or_fail() {
    let diffs = batch(5, &[ev_new(1, Some("Bid"), "100.0", "1.0", None)])
        + &batch(8, &[ev_remove(1, "100.0")]);
    let (res, out) = run(&diffs, None, None, &[], &Options::default());
    let stats = res.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(stats.gap_blocks, 2);
    assert_eq!(
        ops(&out)
            .iter()
            .map(|(h, o)| (*h, o.len()))
            .collect::<Vec<_>>(),
        vec![(5, 1), (6, 0), (7, 0), (8, 1)]
    );
    let no_gaps = Options {
        allow_gaps: false,
        ..Options::default()
    };
    let (res, _) = run(&diffs, None, None, &[], &no_gaps);
    assert!(
        matches!(
            res,
            Err(ConvertError::Gap {
                expected: 6,
                found: 8
            })
        ),
        "{res:?}"
    );
}

#[test]
fn zero_new_size_update_is_a_cancel_and_counted() {
    let diffs = batch(
        5,
        &[
            ev_new(1, Some("Ask"), "100.0", "1.0", None),
            ev_update(1, "100.0", "1.0", "0.0"),
        ],
    );
    let (res, out) = run(&diffs, None, None, &[], &Options::default());
    let stats = res.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(stats.zero_size_updates, 1);
    assert_eq!(
        ops(&out),
        vec![(
            5,
            vec![
                ins(1, Side::Ask, 1000, 100_000),
                op(BookOp::Cancel { oid: Oid(1) })
            ]
        )]
    );
}

#[test]
fn bad_decimal_in_a_diff_is_an_error() {
    let diffs = batch(5, &[ev_new(1, Some("Ask"), "100.05", "1.0", None)]);
    let (res, _) = run(&diffs, None, None, &[], &Options::default());
    assert!(
        matches!(
            res,
            Err(ConvertError::Decimal {
                height: 5,
                oid: 1,
                what: "price",
                ..
            })
        ),
        "{res:?}"
    );
}

#[test]
fn check_outside_the_range_is_counted_or_strict_error() {
    let diffs = batch(5, &[ev_new(1, Some("Ask"), "100.0", "1.0", None)]);
    let late = L4Snapshot::from_json(r#"[9,[["BTC",[[],[]]]]]"#).unwrap_or_else(|e| panic!("{e}"));
    let (res, _) = run(
        &diffs,
        None,
        None,
        std::slice::from_ref(&late),
        &Options::default(),
    );
    assert_eq!(res.unwrap_or_else(|e| panic!("{e}")).checks_out_of_range, 1);
    let strict = Options {
        strict: true,
        ..Options::default()
    };
    let (res, _) = run(&diffs, None, None, &[late], &strict);
    assert!(matches!(res, Err(ConvertError::Strict(_))), "{res:?}");
}

#[test]
fn stream_errors_name_the_stream() {
    let diffs = batch(5, &[]) + &batch(4, &[]);
    let (res, _) = run(&diffs, None, None, &[], &Options::default());
    assert!(
        matches!(
            res,
            Err(ConvertError::Node {
                stream: "diffs",
                ..
            })
        ),
        "{res:?}"
    );
    let statuses = "garbage\n";
    let (res, _) = run(
        &batch(5, &[]),
        Some(statuses),
        None,
        &[],
        &Options::default(),
    );
    assert!(
        matches!(
            res,
            Err(ConvertError::Node {
                stream: "statuses",
                ..
            })
        ),
        "{res:?}"
    );
}

#[test]
fn nothing_to_convert_is_an_error() {
    let (res, _) = run("", None, None, &[], &Options::default());
    assert!(matches!(res, Err(ConvertError::Empty)), "{res:?}");
}

// ---- property: a model book rendered as node output converts to a dataset ----
// ---- that book-check replays cleanly against the model's own snapshots.   ----

/// Model of one coin: per side, price ticks to queue of (oid, lots).
#[derive(Default, Clone)]
struct Model {
    bids: BTreeMap<u64, Vec<(u64, u64)>>,
    asks: BTreeMap<u64, Vec<(u64, u64)>>,
    next_oid: u64,
}

impl Model {
    fn side(&mut self, s: Side) -> &mut BTreeMap<u64, Vec<(u64, u64)>> {
        match s {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        }
    }

    fn all(&self) -> Vec<(Side, u64, u64, u64)> {
        let mut v = Vec::new();
        for (s, book) in [(Side::Bid, &self.bids), (Side::Ask, &self.asks)] {
            for (&px, q) in book {
                for &(oid, sz) in q {
                    v.push((s, px, oid, sz));
                }
            }
        }
        v
    }

    fn snapshot_json(&self, height: u64) -> String {
        let side_json = |s: Side, book: &BTreeMap<u64, Vec<(u64, u64)>>| {
            let mut levels: Vec<_> = book.iter().collect();
            if s == Side::Bid {
                levels.reverse();
            }
            let tag = if s == Side::Bid { "B" } else { "A" };
            levels
                .into_iter()
                .flat_map(|(&px, q)| {
                    q.iter().map(move |&(oid, sz)| {
                        format!(
                            r#"["0xab",{{"coin":"BTC","side":"{tag}","limitPx":"{}","sz":"{}","oid":{oid},"isTrigger":false}}]"#,
                            format_units(px, 1),
                            format_units(sz, 5)
                        )
                    })
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            r#"[{height},[["BTC",[[{}],[{}]]]]]"#,
            side_json(Side::Bid, &self.bids),
            side_json(Side::Ask, &self.asks)
        )
    }
}

fn side_name(s: Side) -> &'static str {
    match s {
        Side::Bid => "B",
        Side::Ask => "A",
    }
}

/// Apply one random step to the model and render it as (diff, status) events.
fn step(m: &mut Model, kind: u8, a: u64, b: u64, with_side: bool) -> (String, Option<String>) {
    let resting = m.all();
    if kind.is_multiple_of(4) || resting.is_empty() {
        let side = if a.is_multiple_of(2) {
            Side::Bid
        } else {
            Side::Ask
        };
        // Bids 90.0-99.9, asks 100.0-109.9: never crossing.
        let px = if side == Side::Bid {
            900 + a % 100
        } else {
            1000 + a % 100
        };
        let sz = 1 + b % 1_000_000;
        m.next_oid += 1;
        let oid = m.next_oid;
        let px_s = format_units(px, 1);
        let q = m.side(side).entry(px).or_default();
        let before = if !q.is_empty() && b.is_multiple_of(3) {
            let at = usize::try_from(b / 3).unwrap_or(0) % q.len();
            let anchor = q[at].0;
            q.insert(at, (oid, sz));
            Some(anchor)
        } else {
            q.push((oid, sz));
            None
        };
        let side_s = side_name(side);
        let diff_side = with_side.then_some(if side == Side::Bid { "Bid" } else { "Ask" });
        let status = (!with_side).then(|| ev_status(oid, side_s, &px_s, "open", "Alo"));
        return (
            ev_new(oid, diff_side, &px_s, &format_units(sz, 5), before),
            status,
        );
    }
    let pick = resting[usize::try_from(a).unwrap_or(0) % resting.len()];
    let (side, px, oid, sz) = pick;
    let px_s = format_units(px, 1);
    let q = m.side(side).get_mut(&px).unwrap_or_else(|| unreachable!());
    if kind % 4 == 1 {
        q.retain(|&(o, _)| o != oid);
        if q.is_empty() {
            m.side(side).remove(&px);
        }
        (ev_remove(oid, &px_s), None)
    } else {
        let new = 1 + b % 1_000_000;
        for e in q.iter_mut() {
            if e.0 == oid {
                e.1 = new;
            }
        }
        (
            ev_update(oid, &px_s, &format_units(sz, 5), &format_units(new, 5)),
            None,
        )
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn model_book_round_trips_through_converter_and_book_check(
        seed_steps in prop::collection::vec((any::<u8>(), any::<u64>(), any::<u64>()), 0..20),
        blocks in prop::collection::vec(
            (0u64..3, prop::collection::vec((any::<u8>(), any::<u64>(), any::<u64>(), any::<bool>()), 0..12)),
            1..15,
        ),
        split_lines in any::<bool>(),
    ) {
        let mut m = Model::default();
        for (k, a, b) in seed_steps {
            let _ = step(&mut m, k, a, b, true);
        }
        let seed_height = 100;
        let seed = L4Snapshot::from_json(&m.snapshot_json(seed_height)).unwrap_or_else(|e| panic!("{e}"));
        let (mut diffs, mut statuses, mut checks) = (String::new(), String::new(), Vec::new());
        let mut height = seed_height;
        for (skip, evs) in blocks {
            height += 1 + skip; // `skip` heights with no line at all: gaps
            let (mut d, mut s) = (Vec::new(), Vec::new());
            for (k, a, b, with_side) in evs {
                let (diff, status) = step(&mut m, k, a, b, with_side);
                d.push(diff);
                s.extend(status);
            }
            if split_lines && d.len() > 1 {
                let tail = d.split_off(d.len() / 2);
                diffs += &batch(height, &d);
                diffs += &batch(height, &tail);
            } else {
                diffs += &batch(height, &d);
            }
            statuses += &batch(height, &s);
            checks.push(L4Snapshot::from_json(&m.snapshot_json(height)).unwrap_or_else(|e| panic!("{e}")));
        }
        let (res, out) = run(&diffs, Some(&statuses), Some(&seed), &checks, &Options { strict: true, ..Options::default() });
        let stats = res.unwrap_or_else(|e| panic!("{e}"));
        prop_assert_eq!(stats.last_height, height);
        prop_assert_eq!(stats.check_books, u64::try_from(checks.len()).unwrap_or(0));
        let report = book_check(&out).unwrap_or_else(|e| panic!("{e}"));
        prop_assert_eq!(report.l4_books_compared, u64::try_from(checks.len()).unwrap_or(0));
    }
}
