//! Native per-block `apply_duration` from `node_fast_block_times` (real lines,
//! our node, heights 1232430001..=1232430003).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use p2_node::native_times::{parse_duration_ns, parse_line, NativeTime};

const LINES: &str = include_str!("data/fast_block_times_3.jsonl");

#[test]
fn parses_real_lines() {
    let v: Vec<NativeTime> = LINES.lines().map(|l| parse_line(l).unwrap()).collect();
    assert_eq!(v[0].height, 1_232_430_001);
    assert_eq!(v[0].apply_ns, 2_629_160); // 0.00262916 s
    assert_eq!(v[1].apply_ns, 12_478_919); // 0.012478919 s
    assert_eq!(v[2].apply_ns, 26_845_330); // 0.02684533 s
    assert_eq!(v[0].block_time, "2026-11-24T00:19:54.889311617");
}

#[test]
fn duration_known_answers() {
    assert_eq!(parse_duration_ns("0.00262916"), Some(2_629_160));
    assert_eq!(parse_duration_ns("1"), Some(1_000_000_000));
    assert_eq!(parse_duration_ns("1.5"), Some(1_500_000_000));
    assert_eq!(parse_duration_ns("0.000000001"), Some(1));
    // Sub-nanosecond digits are truncated, not rounded.
    assert_eq!(parse_duration_ns("0.0000000019"), Some(1));
    assert_eq!(parse_duration_ns("2.5e-05"), Some(25_000));
    assert_eq!(parse_duration_ns("1e-9"), Some(1));
    assert_eq!(parse_duration_ns("-0.1"), None);
    assert_eq!(parse_duration_ns("abc"), None);
    assert_eq!(parse_duration_ns(""), None);
}

#[test]
fn unknown_field_is_an_error() {
    let l = LINES
        .lines()
        .next()
        .unwrap()
        .replacen("\"height\"", "\"x\":1,\"height\"", 1);
    assert!(parse_line(&l).is_err());
}
