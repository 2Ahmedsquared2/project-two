//! Racer behaviour: known answers, a property test against a reference
//! model, and the real hl-node line key.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::items_after_statements,
    missing_docs
)]

use std::collections::BTreeMap;

use hl_race::{drain_round_robin, Item, Offer, Racer, SimSource, Source};
use proptest::prelude::*;

fn it(seq: u64, p: &[u8], t: u64) -> Item<'_> {
    Item {
        seq,
        payload: p,
        t_recv_ns: t,
    }
}

fn pop_all(r: &mut Racer) -> Vec<(u64, usize, Vec<u8>)> {
    let mut v = Vec::new();
    while let Some(x) = r.pop() {
        v.push((x.seq, x.winner, x.payload.to_vec()));
    }
    v
}

#[test]
fn first_copy_wins_and_later_copies_are_counted_with_lag() {
    let mut r = Racer::new(2, 8, 16, 100);
    assert_eq!(r.offer(1, it(100, b"a", 10)), Offer::Won);
    assert_eq!(
        r.offer(0, it(100, b"a", 25)),
        Offer::Duplicate { lag_ns: 15 }
    );
    assert_eq!(pop_all(&mut r), vec![(100, 1, b"a".to_vec())]);
    // A copy after release is still compared.
    assert_eq!(
        r.offer(0, it(100, b"a", 40)),
        Offer::Duplicate { lag_ns: 30 }
    );
    let s = r.stats();
    assert_eq!((s[1].wins, s[0].wins, s[0].duplicates), (1, 0, 2));
    assert_eq!(s[0].lag.count(), 2);
    assert_eq!(s[0].lag.max(), 30);
}

#[test]
fn divergent_copy_raises_alarm_before_and_after_release() {
    let mut r = Racer::new(2, 8, 16, 0);
    assert_eq!(r.offer(0, it(0, b"x", 1)), Offer::Won);
    assert_eq!(r.offer(1, it(0, b"y", 2)), Offer::Divergent);
    assert_eq!(pop_all(&mut r).len(), 1);
    assert_eq!(r.offer(1, it(0, b"xx", 3)), Offer::Divergent);
    assert_eq!(r.stats()[1].divergences, 2);
}

#[test]
fn gap_is_held_until_a_slower_source_fills_it() {
    let mut r = Racer::new(2, 8, 16, 0);
    assert_eq!(r.offer(0, it(0, b"0", 1)), Offer::Won);
    assert_eq!(r.offer(0, it(2, b"2", 3)), Offer::Won); // source 0 lost 1
    assert_eq!(
        pop_all(&mut r).iter().map(|x| x.0).collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(r.pop(), None);
    assert_eq!(r.offer(1, it(1, b"1", 9)), Offer::Won); // gap fill
    assert_eq!(
        pop_all(&mut r)
            .iter()
            .map(|x| (x.0, x.1))
            .collect::<Vec<_>>(),
        vec![(1, 1), (2, 0)]
    );
}

#[test]
fn skip_counts_a_gap_and_a_late_copy_is_never_released() {
    let mut r = Racer::new(1, 4, 16, 0);
    assert_eq!(r.offer(0, it(1, b"1", 1)), Offer::Won);
    assert_eq!(r.skip_missing(), Some(0));
    assert_eq!(r.skip_missing(), None); // 1 is held
    assert_eq!(pop_all(&mut r).len(), 1);
    assert_eq!(r.offer(0, it(0, b"0", 5)), Offer::Late);
    assert_eq!(r.pop(), None);
    assert_eq!(r.report().gaps, 1);
}

#[test]
fn window_limits_and_payload_limit() {
    let mut r = Racer::new(1, 4, 2, 10);
    assert_eq!(r.offer(0, it(14, b"a", 0)), Offer::TooFar);
    assert_eq!(r.offer(0, it(13, b"a", 0)), Offer::Won);
    assert_eq!(r.offer(0, it(12, b"abc", 0)), Offer::TooLarge);
    assert_eq!(r.offer(5, it(12, b"a", 0)), Offer::UnknownSource);
}

#[test]
fn wraps_cleanly_at_the_top_of_u64() {
    let start = u64::MAX - 2;
    let mut r = Racer::new(1, 8, 4, start);
    for s in [start + 2, start, start + 1] {
        assert_eq!(r.offer(0, it(s, &s.to_le_bytes()[..1], 0)), Offer::Won);
    }
    // u64::MAX - 2 and - 1 release; u64::MAX is held but next cannot advance
    // past it, so it is not released (documented: pop returns None).
    let got: Vec<u64> = pop_all(&mut r).iter().map(|x| x.0).collect();
    assert_eq!(got, vec![start, start + 1]);
    assert_eq!(r.next_seq(), u64::MAX);
}

#[test]
fn report_serialises() {
    let mut r = Racer::new(2, 4, 4, 0);
    let _ = r.offer(0, it(0, b"a", 1));
    let _ = r.offer(1, it(0, b"a", 3));
    let j = serde_json::to_value(r.report()).unwrap();
    assert_eq!(j["sources"][0]["wins"], 1);
    assert_eq!(j["sources"][1]["lag"]["count"], 1);
    assert_eq!(
        j["sources"][1]["lag"]["buckets"][0],
        serde_json::json!([2, 1])
    );
}

#[test]
fn simulated_sources_race_round_robin() {
    // Three sources carry heights 0..20; A loses every 3rd, B is slower, C
    // corrupts height 7.
    let mk = |skip: u64, delay: u64, bad: Option<u64>| {
        SimSource::new(
            (0..20u64)
                .filter(|h| skip == 0 || h % skip != 0)
                .map(|h| {
                    let p = if Some(h) == bad {
                        b"BAD".to_vec()
                    } else {
                        format!("blk{h}").into_bytes()
                    };
                    (h, p, h * 100 + delay)
                })
                .collect(),
        )
    };
    let (mut a, mut b, mut c) = (mk(3, 0, None), mk(0, 50, None), mk(0, 70, Some(7)));
    let mut srcs: [&mut dyn Source; 3] = [&mut a, &mut b, &mut c];
    let mut r = Racer::new(3, 64, 16, 0);
    let mut out = Vec::new();
    let mut alarms = Vec::new();
    drain_round_robin(
        &mut r,
        &mut srcs,
        |x| out.push((x.seq, x.payload.to_vec())),
        |s, k| alarms.push((s, k)),
    );
    assert_eq!(out.len(), 20);
    for (i, (s, p)) in out.iter().enumerate() {
        assert_eq!(*s, i as u64);
        assert_eq!(p, &format!("blk{i}").into_bytes());
    }
    assert_eq!(alarms, vec![(2, 7)]);
}

#[test]
fn node_line_key_is_the_block_height_on_real_by_block_data() {
    // 30 complete lines from Hyperliquid's public bucket,
    // node_fills_by_block/hourly/20261123/23 (written with --batch-by-block).
    let raw = include_str!("../../../golden/race/node_fills_by_block_head.jsonl");
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 30);
    fn line(l: &str) -> hl_latency::CompleteLine<'_> {
        hl_latency::CompleteLine::assume_newline_seen(l.as_bytes())
    }
    let heights: Vec<u64> = lines
        .iter()
        .map(|l| hl_race::node_line_seq(line(l)).unwrap())
        .collect();
    assert_eq!(heights[0], 1_232_362_950);
    assert!(heights.windows(2).all(|w| w[0] + 1 == w[1]), "{heights:?}");
    // The body excludes local_time and nothing else.
    let body = hl_race::node_line_body(line(lines[0])).unwrap();
    assert!(body.starts_with(
        br#","block_time":"2026-11-23T22:59:59.844258480","block_number":1232362950,"#
    ));
    assert!(lines[0].as_bytes().ends_with(body));
}

#[test]
fn stream_mode_lines_can_repeat_a_height_so_only_by_block_streams_are_raced() {
    // golden/node is a synthetic --stream-with-block-info style fixture:
    // block 1001 spans two lines. Such a stream must not be raced on height.
    let raw = include_str!("../../../golden/node/node_raw_book_diffs");
    let heights: Vec<u64> = raw
        .lines()
        .map(|l| {
            hl_race::node_line_seq(hl_latency::CompleteLine::assume_newline_seen(l.as_bytes()))
                .unwrap()
        })
        .collect();
    assert!(heights.windows(2).any(|w| w[0] == w[1]), "{heights:?}");
}

#[test]
fn two_nodes_same_block_differing_only_in_local_time_do_not_alarm() {
    let real = include_str!("../../../golden/race/node_fills_by_block_head.jsonl")
        .lines()
        .next()
        .unwrap();
    let other = real.replacen(
        "2026-11-23T23:00:00.051529628",
        "2026-11-23T23:00:00.049000001",
        1,
    );
    assert_ne!(real, other);
    let line = |l: &str| {
        hl_latency::CompleteLine::assume_newline_seen(l.as_bytes())
            .as_bytes()
            .to_vec()
    };
    let (a, b) = (line(real), line(&other));
    let body = |v: &[u8]| {
        hl_race::node_line_body(hl_latency::CompleteLine::assume_newline_seen(v))
            .unwrap()
            .to_vec()
    };
    let mut r = Racer::new(2, 8, 4096, 1_232_362_950);
    assert_eq!(r.offer(1, it(1_232_362_950, &body(&b), 5)), Offer::Won);
    assert_eq!(
        r.offer(0, it(1_232_362_950, &body(&a), 9)),
        Offer::Duplicate { lag_ns: 4 }
    );
    // Whole lines would have alarmed.
    let mut r2 = Racer::new(2, 8, 4096, 1_232_362_950);
    assert_eq!(r2.offer(1, it(1_232_362_950, &b, 5)), Offer::Won);
    assert_eq!(r2.offer(0, it(1_232_362_950, &a, 9)), Offer::Divergent);
    // Not a node line.
    assert_eq!(
        hl_race::node_line_body(hl_latency::CompleteLine::assume_newline_seen(b"{}")),
        None
    );
}

// Reference model: unbounded memory, first arrival per key wins, release
// strictly in order once the next key is known.
#[derive(Debug, Clone)]
struct Ev {
    src: usize,
    seq: u64,
    bad: bool,
}

fn events() -> impl Strategy<Value = Vec<Ev>> {
    prop::collection::vec((0usize..3, 0u64..24, prop::bool::weighted(0.05)), 0..120).prop_map(|v| {
        v.into_iter()
            .map(|(src, seq, bad)| Ev { src, seq, bad })
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]
    #[test]
    fn matches_unbounded_reference(evs in events(), slots in 1usize..10) {
        let mut r = Racer::new(3, slots, 8, 0);
        let mut first: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let mut released: Vec<(u64, Vec<u8>)> = Vec::new();
        for e in &evs {
            let p = if e.bad { format!("B{}", e.seq) } else { format!("G{}", e.seq) }.into_bytes();
            let next = r.next_seq();
            let o = r.offer(e.src, Item { seq: e.seq, payload: &p, t_recv_ns: 0 });
            let in_window = e.seq >= next && e.seq - next < slots as u64;
            match first.get(&e.seq) {
                None if in_window => { prop_assert_eq!(o, Offer::Won); first.insert(e.seq, p.clone()); }
                None if e.seq >= next => prop_assert_eq!(o, Offer::TooFar),
                None => prop_assert_eq!(o, Offer::Late),
                // Known key: equal bytes never alarm; different bytes alarm
                // unless the slot was reused (Late).
                Some(f) if *f == p => { let ok = matches!(o, Offer::Duplicate { .. } | Offer::Late | Offer::TooFar); prop_assert!(ok, "same bytes gave {:?}", o); }
                Some(_) => { let ok = matches!(o, Offer::Divergent | Offer::Late); prop_assert!(ok, "different bytes gave {:?}", o); }
            }
            if o == Offer::Won && first.get(&e.seq) != Some(&p) { prop_assert!(false); }
            while let Some(x) = r.pop() { released.push((x.seq, x.payload.to_vec())); }
        }
        // In order, each once, and equal to the first copy of that key.
        prop_assert!(released.windows(2).all(|w| w[0].0 + 1 == w[1].0));
        for (s, p) in &released { prop_assert_eq!(first.get(s), Some(p)); }
        // Liveness on the trace: every key 0..k that arrived is released.
        let mut k = 0;
        while first.contains_key(&k) { k += 1; }
        prop_assert_eq!(released.len() as u64, k);
    }
}

#[test]
fn tlc_trace_offering_before_draining_drops_the_complete_sources_key() {
    // formal/race/Race.tla AllReleased counter-example without the driver
    // contract: window 1, source 0 is the only complete source.
    let mut r = Racer::new(2, 1, 4, 0);
    assert_eq!(r.offer(0, it(0, b"0", 1)), Offer::Won);
    // Not drained: key 0 still occupies the only slot, so key 1 is TooFar
    // and source 0 never sends it again.
    assert_eq!(r.offer(0, it(1, b"1", 2)), Offer::TooFar);
    assert_eq!(pop_all(&mut r).len(), 1);
    assert_eq!(r.pop(), None);
    // With the contract (drain first) the same arrivals release both.
    let mut r = Racer::new(2, 1, 4, 0);
    assert_eq!(r.offer(0, it(0, b"0", 1)), Offer::Won);
    assert_eq!(pop_all(&mut r).len(), 1);
    assert_eq!(r.offer(0, it(1, b"1", 2)), Offer::Won);
    assert_eq!(pop_all(&mut r).len(), 1);
}
