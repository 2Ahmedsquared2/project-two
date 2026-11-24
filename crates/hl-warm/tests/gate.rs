//! `WarmGate`: closed until every required step passed AND the warm-up window
//! (time and event count) elapsed after that; samples before are excluded.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_warm::{GateConfig, GatedHistogram, Phase, WarmGate};

const MS: u64 = 1_000_000;

fn gate(steps: &[&'static str], min_ns: u64, min_events: u64) -> WarmGate {
    WarmGate::new(GateConfig {
        steps: steps.to_vec(),
        min_window_ns: min_ns,
        min_window_events: min_events,
    })
}

#[test]
fn closed_until_steps_and_window() {
    let mut g = gate(&["pin", "mlock"], 10 * MS, 3);
    // Events before any step: warm-up.
    assert_eq!(g.on_event(0), Phase::Warmup);
    g.pass("pin", MS).unwrap();
    assert_eq!(g.on_event(2 * MS), Phase::Warmup);
    g.pass("mlock", 3 * MS).unwrap(); // armed at 3 ms
    assert!(!g.is_open());
    // Window: >= 3 events and >= 10 ms after arming.
    assert_eq!(g.on_event(4 * MS), Phase::Warmup); // 1 event
    assert_eq!(g.on_event(5 * MS), Phase::Warmup); // 2
    assert_eq!(g.on_event(6 * MS), Phase::Warmup); // 3 events, only 3 ms
    assert_eq!(g.on_event(13 * MS), Phase::Measured); // 4 events, 10 ms: opens on this event
    assert!(g.is_open());
    assert_eq!(g.opened_at_ns(), Some(13 * MS));
    assert_eq!(g.warmup_events(), 5);
}

#[test]
fn time_alone_is_not_enough() {
    let mut g = gate(&["a"], MS, 5);
    g.pass("a", 0).unwrap();
    // 5 window events must be seen before the opening one.
    for t in 0..5 {
        assert_eq!(g.on_event(100 * MS + t), Phase::Warmup);
    }
    assert_eq!(g.on_event(200 * MS), Phase::Measured);
}

#[test]
fn events_alone_are_not_enough() {
    let mut g = gate(&["a"], 50 * MS, 1);
    g.pass("a", 0).unwrap();
    for _ in 0..1000 {
        assert_eq!(g.on_event(49 * MS), Phase::Warmup);
    }
    assert_eq!(g.on_event(50 * MS), Phase::Measured);
}

#[test]
fn failed_or_missing_step_keeps_it_closed_forever() {
    let mut g = gate(&["a", "b"], 0, 0);
    g.pass("a", 0).unwrap();
    for t in 0..100 {
        assert_eq!(g.on_event(t * MS), Phase::Warmup);
    }
    g.fail("b", "no first message from source", 5);
    g.pass("b", 6).unwrap_err(); // a failed step cannot be passed later
    assert_eq!(g.on_event(1_000 * MS), Phase::Warmup);
    let r = g.report();
    assert!(r.contains("b FAILED: no first message from source"), "{r}");
}

#[test]
fn unknown_step_and_double_pass_rejected() {
    let mut g = gate(&["a"], 0, 0);
    assert!(g.pass("nope", 0).is_err());
    g.pass("a", 0).unwrap();
    assert!(g.pass("a", 1).is_err());
}

#[test]
fn zero_window_opens_on_first_event_after_arming() {
    let mut g = gate(&["a"], 0, 0);
    assert_eq!(g.on_event(0), Phase::Warmup);
    g.pass("a", 1).unwrap();
    assert_eq!(g.on_event(1), Phase::Measured);
}

#[test]
fn stays_open() {
    let mut g = gate(&["a"], 0, 0);
    g.pass("a", 0).unwrap();
    assert_eq!(g.on_event(0), Phase::Measured);
    assert_eq!(g.on_event(0), Phase::Measured);
}

#[test]
fn gated_histogram_excludes_warmup_samples() {
    let mut g = gate(&["a"], 0, 2);
    let mut h = GatedHistogram::new().unwrap();
    // Huge warm-up samples must not show up in the measured histogram.
    for t in 0..3 {
        let p = g.on_event(t);
        h.record(p, 1_000_000_000).unwrap();
    }
    g.pass("a", 10).unwrap();
    let mut measured = 0;
    for t in 10..20 {
        let p = g.on_event(t);
        if p == Phase::Measured {
            measured += 1;
        }
        h.record(p, 500).unwrap();
    }
    assert_eq!(h.excluded(), 3 + 2); // 3 before arming, 2 window events
    assert_eq!(h.measured().len(), measured);
    assert_eq!(h.measured().len(), 8);
    assert!(h.measured().max() < 1_000);
}

#[test]
fn report_lists_steps_in_order_with_state() {
    let mut g = gate(&["pin", "mlock", "arena"], 0, 0);
    g.pass("pin", 0).unwrap();
    g.note("mlock", "ENOMEM, RLIMIT_MEMLOCK too low");
    let r = g.report();
    let lines: Vec<&str> = r.lines().collect();
    assert!(lines[0].starts_with("gate CLOSED"), "{r}");
    assert!(lines[1].starts_with("pin passed"), "{r}");
    assert!(lines[2].starts_with("mlock PENDING"), "{r}");
    assert!(lines[2].contains("ENOMEM"), "{r}");
    assert!(lines[3].starts_with("arena PENDING"), "{r}");
}

#[test]
fn failing_a_passed_step_after_arming_keeps_it_closed() {
    // Found while writing the Kani harness: the window was armed by the last
    // pass, and a later failure of an already-passed step must disarm it.
    let mut g = gate(&["a", "b"], 0, 0);
    g.pass("a", 0).unwrap();
    g.pass("b", 0).unwrap();
    g.fail("a", "source dropped", 1);
    assert_eq!(g.on_event(5), Phase::Warmup);
    assert!(!g.is_open());
}
