//! Allocation gate for the node arm's steady-state tick: after warmup, one
//! tick (read + split + stamp + parse + ring push) must not touch the heap.
//! Linking `hl-alloc-gate` swaps this test binary's global allocator for a
//! counting one (per-thread counts).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::{self, File, OpenOptions};
use std::hint::black_box;
use std::io::Write;

use hl_alloc_gate::assert_no_alloc_after_warmup;
use hl_capture::{ring, scout, NodeTailer};

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

fn gate(what: &str, events: usize, scouted: bool) {
    let t = tempfile::tempdir().unwrap();
    let mut file = hourly_file(t.path());
    // Every line is built before measuring: the feed is not the code under test.
    let lines: Vec<Vec<u8>> = (0..WARMUP + TICKS)
        .map(|i| node_line(1000 + i, events))
        .collect();
    // With a scout the tick also asks for a look and takes from the slot:
    // both must stay off the heap. The scout runs on its own thread, as in
    // the binary (its walk allocates there, which this gate does not count).
    let (mut scout, arrivals) = scout(t.path(), std::time::Duration::from_millis(1));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut tail, scout_thread) = if scouted {
        let stop = std::sync::Arc::clone(&stop);
        let th = std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                scout.tick(std::time::Instant::now()).unwrap();
                std::thread::yield_now();
            }
            scout.looks()
        });
        (
            NodeTailer::with_scout(t.path(), 1 << 20, arrivals),
            Some(th),
        )
    } else {
        (NodeTailer::new(t.path()), None)
    };
    let mut ns = 0u64;
    let mut clock = move || {
        ns += 1;
        ns
    };
    // The reader's whole tick: the record goes into the SPSC ring. The
    // writer's side is drained here (a pop, no serialising) so the ring
    // never fills.
    let (mut sink, mut source) = ring(64);
    let mut seen = 0u64;
    // The first poll finds the file and follows it from its end.
    tail.poll_into(&mut clock, &mut sink).unwrap();
    assert_no_alloc_after_warmup(what, WARMUP, TICKS, |i| {
        file.write_all(&lines[usize::try_from(i).unwrap()]).unwrap();
        if scouted {
            tail.request_rescan();
            black_box(tail.look_pending());
        }
        tail.poll_into(&mut clock, &mut sink).unwrap();
        let o = black_box(source.pop()).expect("the tick pushed its record");
        assert_eq!(o.block_number, Some(1000 + i));
        assert!(source.pop().is_none());
        seen += 1;
    });
    assert_eq!(seen, WARMUP + TICKS, "every tick delivered its line");
    assert_eq!(sink.dropped(), 0);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(th) = scout_thread {
        assert!(
            th.join().unwrap() > 0,
            "the scout looked while the reader ticked"
        );
    }
    let s = tail.stats();
    assert_eq!(
        (s.rescans, s.scan_fallbacks),
        (1, 0),
        "no walk, no strict parse"
    );
    assert_eq!(tail.stats().rotations, 0, "steady state: no rotation");
}

#[test]
fn steady_state_tick_does_not_allocate_on_small_lines() {
    gate("hl-capture node tick, 2 events", 2, false);
}

#[test]
fn steady_state_tick_does_not_allocate_on_busy_lines() {
    gate("hl-capture node tick, 600 events", 600, false);
}

#[test]
fn steady_state_tick_with_a_scout_does_not_allocate() {
    gate("hl-capture node tick with scout, 2 events", 2, true);
    gate("hl-capture node tick with scout, 600 events", 600, true);
}
