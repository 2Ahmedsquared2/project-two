//! `LatencyHist`: integer nearest-rank percentiles, merge and the raw dumps.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_replay::{LatencyHist, Summary, HIST_MAX_NS};

fn one_to_thousand() -> LatencyHist {
    let mut h = LatencyHist::new();
    for v in 1..=1000 {
        h.record(v);
    }
    h
}

#[test]
fn percentiles_are_nearest_rank_with_the_sample_count() {
    let s = one_to_thousand().summary();
    assert_eq!(
        s,
        Summary {
            count: 1000,
            p50: 500,
            p90: 900,
            p99: 990,
            p999: 999,
            p9999: 1000,
            max: 1000
        }
    );
}

#[test]
fn quantile_in_parts_per_100k() {
    let h = one_to_thousand();
    assert_eq!(h.value_at_per_100k(0), 1);
    assert_eq!(h.value_at_per_100k(50_000), 500);
    assert_eq!(h.value_at_per_100k(100_000), 1000);
    assert_eq!(h.value_at_per_100k(u32::MAX), 1000);
}

#[test]
fn an_empty_histogram_reports_zero_samples_not_a_latency() {
    let h = LatencyHist::new();
    assert_eq!(h.count(), 0);
    assert_eq!(h.summary(), Summary::default());
}

#[test]
fn a_single_sample_is_every_percentile() {
    let mut h = LatencyHist::new();
    h.record(777);
    let s = h.summary();
    assert_eq!((s.count, s.p50, s.p999, s.max), (1, 777, 777, 777));
}

#[test]
fn values_beyond_the_range_saturate_instead_of_vanishing() {
    let mut h = LatencyHist::new();
    h.record(u64::MAX);
    h.record(0);
    assert_eq!(h.count(), 2);
    assert!(h.max() >= HIST_MAX_NS, "{}", h.max());
}

#[test]
fn three_significant_figures_are_kept() {
    let mut h = LatencyHist::new();
    h.record(1_234_567);
    let got = h.max();
    assert!(got.abs_diff(1_234_567) <= 1_235, "{got}");
}

#[test]
fn merge_adds_distributions_rather_than_averaging_percentiles() {
    let mut a = one_to_thousand();
    let mut b = LatencyHist::new();
    for _ in 0..1000 {
        b.record(5000);
    }
    a.merge(&b);
    let s = a.summary();
    assert_eq!(s.count, 2000);
    assert_eq!(s.p50, 1000);
    assert!(s.p99.abs_diff(5000) <= 5, "{s:?}");
}

#[test]
fn hdr_v2_round_trips() {
    let h = one_to_thousand();
    let mut bytes = Vec::new();
    h.write_v2(&mut bytes).unwrap();
    assert!(bytes.len() > 16, "{}", bytes.len());
    let back = LatencyHist::read_v2(&mut bytes.as_slice()).unwrap();
    assert_eq!(back.summary(), h.summary());
}

#[test]
fn garbage_is_not_a_histogram() {
    assert!(LatencyHist::read_v2(&mut &b"not an hdr histogram"[..]).is_err());
}

#[test]
fn bucket_dump_is_value_and_count_per_line_and_sums_to_the_count() {
    let mut h = one_to_thousand();
    h.record(500);
    let mut out = Vec::new();
    h.write_buckets(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    let mut total = 0_u64;
    let mut last = 0_u64;
    let mut rows = 0;
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let (v, c) = line.split_once(' ').unwrap();
        let (v, c): (u64, u64) = (v.parse().unwrap(), c.parse().unwrap());
        assert!(v > last, "values ascend: {line}");
        if v == 500 {
            assert_eq!(c, 2);
        }
        last = v;
        total += c;
        rows += 1;
    }
    assert_eq!(rows, 1000);
    assert_eq!(total, 1001);
    assert!(text.starts_with("# value_ns count"));
}
