//! Allocation gates: after warmup, the success path of each parser must not
//! touch the heap. Linking `hl-alloc-gate` swaps this test binary's global
//! allocator for a counting one (per-thread counts).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod oracle;
mod support;

use std::hint::black_box;

use hl_alloc_gate::{assert_no_alloc_after_warmup, count_ticks};
use hl_latency::{
    parse_node_line, parse_ws_message_into, scan_node_header, CompleteLine, Observation,
};

const WARMUP: u64 = 16;
const TICKS: u64 = 1000;

/// `n` inputs, a different one per tick so no tick repeats its predecessor.
fn inputs(n: u64, make: impl Fn(u64) -> String) -> Vec<String> {
    (0..n).map(make).collect()
}

fn pick(v: &[String], tick: u64) -> &str {
    &v[usize::try_from(tick).unwrap() % v.len()]
}

fn small_lines() -> Vec<String> {
    inputs(64, |i| support::node_line(676_607_012 + i, 2))
}

/// Hundreds of events per line, over 100 KB each (asserted below).
fn busy_lines() -> Vec<String> {
    inputs(4, |i| support::node_line(676_607_012 + i, 600))
}

fn l2_frames() -> Vec<String> {
    inputs(64, |i| {
        support::l2book_frame(1_795_128_590_474 + i * 70, 20)
    })
}

fn trade_frames() -> Vec<String> {
    inputs(64, |i| {
        support::trades_frame(1_795_128_575_393 + i * 70, 12, 1 + i % 4)
    })
}

#[test]
fn strict_node_parse_does_not_allocate_on_small_lines() {
    let lines = small_lines();
    assert_no_alloc_after_warmup("parse_node_line, 2 events", WARMUP, TICKS, |i| {
        let o = parse_node_line(black_box(pick(&lines, i)), i).unwrap();
        assert!(black_box(o).block_number.is_some());
    });
}

#[test]
fn strict_node_parse_does_not_allocate_on_busy_lines() {
    let lines = busy_lines();
    assert!(
        lines[0].len() > 100_000,
        "busy line is {} bytes",
        lines[0].len()
    );
    assert_no_alloc_after_warmup("parse_node_line, 600 events", WARMUP, 100, |i| {
        let o = parse_node_line(black_box(pick(&lines, i)), i).unwrap();
        assert!(black_box(o).block_number.is_some());
    });
}

#[test]
fn header_scan_does_not_allocate() {
    for (what, lines) in [("2 events", small_lines()), ("600 events", busy_lines())] {
        assert_no_alloc_after_warmup(&format!("scan_node_header, {what}"), WARMUP, TICKS, |i| {
            let line = pick(&lines, i);
            let got = scan_node_header(CompleteLine::assume_newline_seen(black_box(
                line.as_bytes(),
            )));
            assert!(black_box(got).is_some(), "no header in a {what} line");
        });
    }
}

#[test]
fn ws_into_does_not_allocate_on_l2book_frames() {
    let frames = l2_frames();
    let mut out: Vec<Observation> = Vec::new();
    assert_no_alloc_after_warmup(
        "parse_ws_message_into, 20-level l2Book",
        WARMUP,
        TICKS,
        |i| {
            parse_ws_message_into(black_box(pick(&frames, i)), i, &mut out).unwrap();
            assert_eq!(black_box(&out).len(), 1);
        },
    );
}

#[test]
fn ws_into_does_not_allocate_on_trades_frames() {
    let frames = trade_frames();
    let mut out: Vec<Observation> = Vec::new();
    assert_no_alloc_after_warmup("parse_ws_message_into, 12 trades", WARMUP, TICKS, |i| {
        parse_ws_message_into(black_box(pick(&frames, i)), i, &mut out).unwrap();
        assert_ne!(black_box(&out).as_slice(), []);
    });
}

#[test]
fn ws_into_does_not_allocate_on_the_captured_fixtures() {
    let frames = [
        include_str!("fixtures/ws_l2Book.json"),
        include_str!("fixtures/ws_trades.json"),
        r#"{"channel":"subscriptionResponse","data":{"method":"subscribe"}}"#,
        r#"{"channel":"pong"}"#,
    ];
    let mut out: Vec<Observation> = Vec::new();
    assert_no_alloc_after_warmup("parse_ws_message_into, fixtures", WARMUP, TICKS, |i| {
        let f = frames[usize::try_from(i).unwrap() % frames.len()];
        parse_ws_message_into(black_box(f), i, &mut out).unwrap();
    });
}

/// The "before" numbers: what the old `Value` parsers allocate per tick on the
/// same inputs. Printed for the PR (`cargo test -p hl-latency --test alloc --
/// --nocapture`), and asserted non-zero so the gates above are known to see
/// allocations on this thread.
#[test]
fn oracle_allocations_per_tick_are_counted() {
    let report = |what: &str, ticks: u64, tick: &mut dyn FnMut(u64)| {
        let r = count_ticks(WARMUP, ticks, tick);
        println!(
            "oracle {what}: {} allocations and {} bytes per tick ({} ticks)",
            r.total.count / r.ticks,
            r.total.bytes / r.ticks,
            r.ticks
        );
        assert_eq!(
            r.allocating_ticks, r.ticks,
            "oracle {what} stopped allocating"
        );
    };
    let (small, busy, l2, trades) = (small_lines(), busy_lines(), l2_frames(), trade_frames());
    report("parse_node_line, 2 events", TICKS, &mut |i| {
        black_box(oracle::parse_node_line(pick(&small, i), i).unwrap());
    });
    report("parse_node_line, 600 events", 100, &mut |i| {
        black_box(oracle::parse_node_line(pick(&busy, i), i).unwrap());
    });
    report("parse_ws_message, 20-level l2Book", TICKS, &mut |i| {
        black_box(oracle::parse_ws_message(pick(&l2, i), i).unwrap());
    });
    report("parse_ws_message, 12 trades", TICKS, &mut |i| {
        black_box(oracle::parse_ws_message(pick(&trades, i), i).unwrap());
    });
}
