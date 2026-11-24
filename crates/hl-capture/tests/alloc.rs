//! Allocation gate for the node arm's steady-state tick: after warmup, one
//! tick (read + split + stamp + parse + hand-off) must not touch the heap.
//! Linking `hl-alloc-gate` swaps this test binary's global allocator for a
//! counting one (per-thread counts).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::{self, File, OpenOptions};
use std::hint::black_box;
use std::io::Write;

use hl_alloc_gate::assert_no_alloc_after_warmup;
use hl_capture::{NodeTailer, ObservationSink};
use hl_latency::Observation;

const WARMUP: u64 = 64;
const TICKS: u64 = 2000;

/// A node line in hl-node's exact header shape with `events` events.
fn node_line(height: u64, events: usize) -> Vec<u8> {
    let event = r#"{"user":"0x31ca8395cf837de08b24da3f660e77761dfb974b","oid":90000000001,"coin":"BTC","side":"B","px":"104000.0","raw_book_diff":{"new":{"sz":"0.25"}}}"#;
    let body = vec![event; events].join(",");
    format!(
        "{{\"local_time\":\"2026-11-19T08:00:00.071123456\",\"block_time\":\"2026-11-19T08:00:00.070000\",\"block_number\":{height},\"events\":[{body}]}}\n"
    )
    .into_bytes()
}

/// An hourly file that exists before the tailer starts (so it is followed
/// from its end, with no rotation during the run), opened for appending.
fn hourly_file(root: &std::path::Path) -> File {
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("8"))
        .unwrap()
}

/// A sink that checks each observation and keeps nothing.
struct Count {
    seen: u64,
    last_height: Option<u64>,
}

impl ObservationSink for Count {
    fn observe(&mut self, obs: Observation) {
        self.seen += 1;
        self.last_height = black_box(obs).block_number;
    }
}

fn gate(what: &str, events: usize) {
    let t = tempfile::tempdir().unwrap();
    let mut file = hourly_file(t.path());
    // Every line is built before measuring: the feed is not the code under test.
    let lines: Vec<Vec<u8>> = (0..WARMUP + TICKS)
        .map(|i| node_line(1000 + i, events))
        .collect();
    let mut tail = NodeTailer::new(t.path());
    let mut ns = 0u64;
    let mut clock = move || {
        ns += 1;
        ns
    };
    let mut sink = Count {
        seen: 0,
        last_height: None,
    };
    // The first poll finds the file and follows it from its end.
    tail.poll_into(&mut clock, &mut sink).unwrap();
    assert_no_alloc_after_warmup(what, WARMUP, TICKS, |i| {
        file.write_all(&lines[usize::try_from(i).unwrap()]).unwrap();
        tail.poll_into(&mut clock, &mut sink).unwrap();
        assert_eq!(sink.last_height, Some(1000 + i));
    });
    assert_eq!(sink.seen, WARMUP + TICKS, "every tick delivered its line");
    assert_eq!(tail.stats().rotations, 0, "steady state: no rotation");
}

#[test]
fn steady_state_tick_does_not_allocate_on_small_lines() {
    gate("hl-capture node tick, 2 events", 2);
}

#[test]
fn steady_state_tick_does_not_allocate_on_busy_lines() {
    gate("hl-capture node tick, 600 events", 600);
}
