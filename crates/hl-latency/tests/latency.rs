//! Behaviour of the same-block latency pairing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::as_conversions)]

use hl_latency::{
    pair, parse_naive_utc_ms, parse_node_line, parse_ws_message, percentile_sorted, summarize,
    Observation, ParseError, Source,
};
use proptest::prelude::*;

fn node(t: u64, h: u64, ns: u64) -> Observation {
    Observation {
        source: Source::Node,
        block_time_ms: t,
        block_number: Some(h),
        recv_ns: ns,
    }
}
fn ws(t: u64, ns: u64) -> Observation {
    Observation {
        source: Source::PublicWs,
        block_time_ms: t,
        block_number: None,
        recv_ns: ns,
    }
}

// ---- timestamps -------------------------------------------------------------

#[test]
fn naive_timestamp_epoch_and_known_value() {
    assert_eq!(parse_naive_utc_ms("1970-01-01T00:00:00").unwrap(), 0);
    // 2025-07-27T08:50:10.051 UTC = 1753606210051 ms (date -u -d ... +%s%3N)
    assert_eq!(
        parse_naive_utc_ms("2025-07-27T08:50:10.051000").unwrap(),
        1_753_606_210_051
    );
}

#[test]
fn naive_timestamp_truncates_nanoseconds_and_handles_leap_day() {
    assert_eq!(
        parse_naive_utc_ms("2025-07-27T08:50:10.334741319").unwrap(),
        1_753_606_210_334
    );
    assert_eq!(
        parse_naive_utc_ms("2024-02-29T23:59:59.9").unwrap(),
        1_709_251_199_900
    );
    assert_eq!(
        parse_naive_utc_ms("2000-03-01T00:00:00").unwrap(),
        951_868_800_000
    );
}

#[test]
fn naive_timestamp_rejects_garbage() {
    for bad in [
        "",
        "2025-07-27",
        "2025-07-27 08:50:10",
        "2025-13-01T00:00:00",
        "2025-02-30T00:00:00",
        "2023-02-29T00:00:00",
        "2025-07-27T24:00:00",
        "2025-07-27T08:60:00",
        "2025-07-27T08:50:10.",
        "2025-07-27T08:50:10.1234567890",
        "2025-07-27T08:50:10Z",
        "1969-12-31T23:59:59",
        "+025-07-27T08:50:10",
    ] {
        assert!(
            matches!(parse_naive_utc_ms(bad), Err(ParseError::BadTimestamp(_))),
            "accepted {bad:?}"
        );
    }
}

proptest! {
    #[test]
    fn naive_timestamp_never_panics(s in "\\PC{0,40}") {
        let _ = parse_naive_utc_ms(&s);
    }

    #[test]
    fn naive_timestamp_roundtrips_seconds(secs in 0u64..253_402_300_799) {
        // Format with the inverse civil algorithm and parse back.
        let (y, m, d, hh, mm, ss) = civil(secs);
        let s = format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.123");
        prop_assert_eq!(parse_naive_utc_ms(&s).unwrap(), secs * 1000 + 123);
    }
}

/// Howard Hinnant's `civil_from_days`, test-side oracle.
fn civil(secs: u64) -> (i64, u64, u64, u64, u64, u64) {
    let days = i64::try_from(secs / 86_400).unwrap();
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        u64::try_from(m).unwrap(),
        u64::try_from(d).unwrap(),
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
    )
}

// ---- node lines ---------------------------------------------------------------

#[test]
fn node_line_block_info_schema() {
    let line = r#"{"local_time":"2025-07-27T08:50:10.334741319","block_time":"2025-07-27T08:50:10.051000","block_number":676607012,"events":[]}"#;
    let o = parse_node_line(line, 42).unwrap();
    assert_eq!(o, node(1_753_606_210_051, 676_607_012, 42));
}

#[test]
fn node_line_rejects_missing_fields_and_bad_json() {
    for bad in [
        "",
        "not json",
        r#"{"block_time":"2025-07-27T08:50:10.051","events":[]}"#,
        r#"{"block_number":1,"events":[]}"#,
        r#"{"block_time":5,"block_number":1}"#,
        r#"{"block_time":"2025-07-27T08:50:10.051","block_number":-1}"#,
    ] {
        assert!(
            matches!(parse_node_line(bad, 0), Err(ParseError::Malformed(_))),
            "accepted {bad:?}"
        );
    }
    assert!(matches!(
        parse_node_line(r#"{"block_time":"yesterday","block_number":1}"#, 0),
        Err(ParseError::BadTimestamp(_))
    ));
}

// ---- public websocket ---------------------------------------------------------

#[test]
fn ws_l2book_gives_one_observation() {
    let m = r#"{"channel":"l2Book","data":{"coin":"BTC","time":1753606210051,"levels":[[],[]]}}"#;
    assert_eq!(
        parse_ws_message(m, 7).unwrap(),
        vec![ws(1_753_606_210_051, 7)]
    );
}

#[test]
fn ws_trades_gives_one_observation_per_distinct_time() {
    let m = r#"{"channel":"trades","data":[
        {"coin":"BTC","side":"B","px":"1","sz":"1","time":100,"hash":"0x","tid":1},
        {"coin":"BTC","side":"A","px":"1","sz":"1","time":100,"hash":"0x","tid":2},
        {"coin":"ETH","side":"A","px":"1","sz":"1","time":170,"hash":"0x","tid":3}]}"#;
    assert_eq!(
        parse_ws_message(m, 9).unwrap(),
        vec![ws(100, 9), ws(170, 9)]
    );
}

#[test]
fn ws_messages_without_block_time_are_empty() {
    for m in [
        r#"{"channel":"subscriptionResponse","data":{"method":"subscribe"}}"#,
        r#"{"channel":"pong"}"#,
    ] {
        assert_eq!(parse_ws_message(m, 0).unwrap(), vec![]);
    }
}

#[test]
fn ws_malformed_block_message_is_an_error() {
    for m in [
        "{",
        r#"{"channel":"l2Book","data":{"coin":"BTC"}}"#,
        r#"{"channel":"trades","data":[{"coin":"BTC","time":"x"}]}"#,
    ] {
        assert!(
            matches!(parse_ws_message(m, 0), Err(ParseError::Malformed(_))),
            "accepted {m:?}"
        );
    }
}

// ---- pairing ------------------------------------------------------------------

#[test]
fn pair_keeps_first_arrival_per_source_and_counts_singletons() {
    let p = pair([
        ws(100, 1_500),
        node(100, 10, 1_000),
        node(100, 10, 900), // later line for same block with earlier stamp: first by stamp wins
        ws(100, 1_400),
        node(170, 11, 2_000),
        ws(240, 3_000),
    ]);
    assert_eq!(p.pairs.len(), 1);
    let only = p.pairs[0];
    assert_eq!(
        (
            only.block_time_ms,
            only.block_number,
            only.node_ns,
            only.ws_ns
        ),
        (100, Some(10), 900, 1_400)
    );
    assert_eq!(only.node_lead_ns(), 500);
    assert_eq!((p.node_only, p.ws_only, p.key_collisions), (1, 1, 0));
}

#[test]
fn pair_orders_by_block_time_and_counts_key_collisions() {
    let p = pair([
        node(300, 12, 5),
        ws(300, 6),
        node(100, 10, 1),
        ws(100, 2),
        node(100, 99, 3),
    ]);
    assert_eq!(
        p.pairs.iter().map(|x| x.block_time_ms).collect::<Vec<_>>(),
        vec![100, 300]
    );
    assert_eq!(p.key_collisions, 1);
}

#[test]
fn negative_lead_when_ws_first() {
    let p = pair([node(1, 1, 2_000), ws(1, 500)]);
    assert_eq!(p.pairs[0].node_lead_ns(), -1_500);
}

// ---- percentiles and summary ----------------------------------------------------

#[test]
fn percentile_nearest_rank() {
    let v: Vec<i64> = (1..=1000).collect();
    assert_eq!(percentile_sorted(&v, 0), Some(1));
    assert_eq!(percentile_sorted(&v, 500), Some(500));
    assert_eq!(percentile_sorted(&v, 900), Some(900));
    assert_eq!(percentile_sorted(&v, 999), Some(999));
    assert_eq!(percentile_sorted(&v, 1000), Some(1000));
    assert_eq!(percentile_sorted(&[7], 500), Some(7));
    assert_eq!(percentile_sorted(&[1, 2, 3, 4], 500), Some(2));
    assert_eq!(percentile_sorted(&[], 500), None);
}

#[test]
fn summary_counts_and_distribution() {
    let mut obs = Vec::new();
    for i in 0..10u64 {
        obs.push(node(i * 70, i, 1_000));
        // lead = i*100 - 200 : -200, -100, 0, 100, ..., 700
        obs.push(ws(i * 70, 800 + i * 100));
    }
    obs.push(node(9_999, 99, 0));
    let s = summarize(&pair(obs));
    assert_eq!(
        (s.matched, s.node_only, s.ws_only, s.node_first),
        (10, 1, 0, 7)
    );
    assert_eq!(s.min_ns, Some(-200));
    assert_eq!(s.p50_ns, Some(200));
    assert_eq!(s.p90_ns, Some(600));
    assert_eq!(s.max_ns, Some(700));
}

#[test]
fn summary_of_nothing_is_empty() {
    let s = summarize(&pair([]));
    assert_eq!(s.matched, 0);
    assert_eq!(s.p50_ns, None);
}

proptest! {
    #[test]
    fn pairing_conserves_blocks(
        node_times in proptest::collection::btree_set(0u64..500, 0..50),
        ws_times in proptest::collection::btree_set(0u64..500, 0..50),
    ) {
        let obs: Vec<_> = node_times.iter().map(|&t| node(t, t, t))
            .chain(ws_times.iter().map(|&t| ws(t, t + 1))).collect();
        let p = pair(obs);
        let both = node_times.intersection(&ws_times).count() as u64;
        prop_assert_eq!(p.pairs.len() as u64, both);
        prop_assert_eq!(p.node_only, node_times.len() as u64 - both);
        prop_assert_eq!(p.ws_only, ws_times.len() as u64 - both);
        prop_assert!(p.pairs.iter().all(|x| x.node_lead_ns() == 1));
    }

    #[test]
    fn percentiles_are_monotone_and_bounded(mut v in proptest::collection::vec(any::<i32>(), 1..200)) {
        let mut v: Vec<i64> = v.drain(..).map(i64::from).collect();
        v.sort_unstable();
        let ps: Vec<i64> = [0, 500, 900, 990, 999, 1000].iter().map(|&q| percentile_sorted(&v, q).unwrap()).collect();
        prop_assert!(ps.windows(2).all(|w| w[0] <= w[1]));
        prop_assert_eq!(ps[0], v[0]);
        prop_assert_eq!(ps[5], *v.last().unwrap());
    }
}

// ---- observation files ----------------------------------------------------------

#[test]
fn read_observations_jsonl_skips_blank_lines_and_names_bad_line() {
    use hl_latency::read_observations;
    let good = "{\"source\":\"node\",\"block_time_ms\":100,\"block_number\":10,\"recv_ns\":5}\n\n{\"source\":\"public_ws\",\"block_time_ms\":100,\"recv_ns\":9}\n";
    let v = read_observations(good.as_bytes()).unwrap();
    assert_eq!(v, vec![node(100, 10, 5), ws(100, 9)]);
    let bad = "{\"source\":\"node\",\"block_time_ms\":1,\"recv_ns\":1}\n{\"source\":\"mars\"}\n";
    let err = read_observations(bad.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("line 2"), "{err}");
}

#[test]
fn ws_real_captured_frames_parse() {
    let l2 = include_str!("fixtures/ws_l2Book.json");
    let o = parse_ws_message(l2, 1).unwrap();
    assert_eq!(o.len(), 1);
    assert_eq!(o[0].block_time_ms, 1_795_128_590_474);
    let trades = include_str!("fixtures/ws_trades.json");
    let o = parse_ws_message(trades, 1).unwrap();
    assert!(o.len() >= 2, "{o:?}");
    assert!(o
        .iter()
        .all(|x| x.source == Source::PublicWs && x.block_time_ms > 1_700_000_000_000));
}
