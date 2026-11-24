//! Issue #31: the look for a newer hourly file runs on another thread (the
//! scout); the reader only takes what it found.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hl_capture::{ring, scout, NodeTailer, WRITE_CHUNK};
use hl_latency::{Observation, Source};

const MAX_LINE: usize = 1 << 20;
const HOUR: Duration = Duration::from_secs(3600);

fn touch(root: &Path, date: &str, hour: &str) -> PathBuf {
    let dir = root.join("hourly").join(date);
    fs::create_dir_all(&dir).unwrap();
    let p = dir.join(hour);
    fs::write(&p, b"").unwrap();
    p
}

fn node_line(height: u64) -> String {
    format!(
        "{{\"local_time\":\"2026-11-19T08:00:00.071\",\"block_time\":\"2026-11-19T08:00:00.070\",\"block_number\":{height},\"events\":[]}}\n"
    )
}

fn append(p: &Path, s: &str) {
    OpenOptions::new()
        .append(true)
        .open(p)
        .unwrap()
        .write_all(s.as_bytes())
        .unwrap();
}

fn poll(tail: &mut NodeTailer) -> Vec<u64> {
    let mut out: Vec<Observation> = Vec::new();
    tail.poll_into(&mut || 1, &mut out).unwrap();
    out.iter().map(|o| o.block_number.unwrap()).collect()
}

#[test]
fn the_reader_takes_a_newer_file_from_the_scout_without_walking() {
    let t = tempfile::tempdir().unwrap();
    let old = touch(t.path(), "20261119", "8");
    let (mut scout, arrivals) = scout(t.path(), HOUR);
    let mut tail = NodeTailer::with_scout(t.path(), MAX_LINE, arrivals);
    assert_eq!(poll(&mut tail), Vec::<u64>::new());
    assert_eq!(tail.current_file(), Some(old.as_path()));
    assert_eq!(
        tail.stats().rescans,
        1,
        "the first poll walks: nothing to read yet"
    );

    let new = touch(t.path(), "20261119", "9");
    append(&new, &node_line(11));
    // However often it is asked, the reader itself never looks.
    for _ in 0..20 {
        tail.request_rescan();
        assert_eq!(poll(&mut tail), Vec::<u64>::new());
    }
    assert_eq!(tail.current_file(), Some(old.as_path()));

    // A line the node appends to the old file after the scout saw the new
    // one, before the reader switches, is kept and is an ordinary line.
    scout.look().unwrap();
    append(&old, &node_line(10));
    assert_eq!(poll(&mut tail), vec![10, 11]);
    assert_eq!(tail.current_file(), Some(new.as_path()));
    let s = tail.stats();
    assert_eq!((s.rescans, s.rotations, s.post_rotation_lines), (1, 1, 1));
    assert_eq!(scout.looks(), 1);
}

#[test]
fn the_file_the_reader_already_follows_is_not_a_rotation() {
    let t = tempfile::tempdir().unwrap();
    let only = touch(t.path(), "20261119", "8");
    append(&only, &node_line(1));
    let (mut scout, arrivals) = scout(t.path(), HOUR);
    let mut tail = NodeTailer::with_scout(t.path(), MAX_LINE, arrivals);
    assert_eq!(poll(&mut tail), Vec::<u64>::new(), "history is skipped");
    // The scout's first look finds the same file.
    scout.look().unwrap();
    append(&only, &node_line(2));
    assert_eq!(poll(&mut tail), vec![2]);
    assert_eq!(poll(&mut tail), Vec::<u64>::new());
    assert_eq!(tail.stats().rotations, 0);
    assert_eq!(tail.current_file(), Some(only.as_path()));
}

#[test]
fn a_first_file_that_appears_later_is_read_from_its_start() {
    let t = tempfile::tempdir().unwrap();
    let (mut scout, arrivals) = scout(t.path(), HOUR);
    let mut tail = NodeTailer::with_scout(t.path(), MAX_LINE, arrivals);
    assert_eq!(poll(&mut tail), Vec::<u64>::new());
    scout.look().unwrap();
    assert_eq!(poll(&mut tail), Vec::<u64>::new());
    let first = touch(t.path(), "20261119", "8");
    append(&first, &node_line(5));
    scout.look().unwrap();
    assert_eq!(poll(&mut tail), vec![5]);
    assert_eq!(tail.current_file(), Some(first.as_path()));
}

/// The slot holds one file. A newer one found while it is full does not
/// replace it: the reader takes files in the order they were found.
#[test]
fn a_file_found_while_the_slot_is_full_waits_its_turn() {
    let t = tempfile::tempdir().unwrap();
    let a = touch(t.path(), "20261119", "8");
    let (mut scout, arrivals) = scout(t.path(), HOUR);
    let mut tail = NodeTailer::with_scout(t.path(), MAX_LINE, arrivals);
    poll(&mut tail);
    assert_eq!(tail.current_file(), Some(a.as_path()));

    let b = touch(t.path(), "20261119", "9");
    append(&b, &node_line(20));
    scout.look().unwrap();
    let c = touch(t.path(), "20261119", "10");
    append(&c, &node_line(30));
    scout.look().unwrap();

    assert_eq!(poll(&mut tail), vec![20]);
    assert_eq!(tail.current_file(), Some(b.as_path()));
    assert_eq!(
        poll(&mut tail),
        Vec::<u64>::new(),
        "the slot was full: not handed over yet"
    );
    scout.look().unwrap();
    assert_eq!(poll(&mut tail), vec![30]);
    assert_eq!(tail.current_file(), Some(c.as_path()));
    assert_eq!(tail.stats().rotations, 2);
}

/// More of the old file than one poll may read keeps the switch waiting,
/// and the file the scout handed over is not lost meanwhile.
#[test]
fn a_backlog_on_the_old_file_holds_the_scouts_file() {
    let t = tempfile::tempdir().unwrap();
    let old = touch(t.path(), "20261119", "8");
    let (mut scout, arrivals) = scout(t.path(), HOUR);
    let mut tail = NodeTailer::with_scout(t.path(), MAX_LINE, arrivals);
    poll(&mut tail);
    let new = touch(t.path(), "20261119", "9");
    scout.look().unwrap();
    // Over the per-poll read budget (16 x 64 KiB) in the old file.
    let per_line = node_line(1_000_000).len();
    let n = u64::try_from(3 * 1024 * 1024 / per_line).unwrap();
    let body: String = (0..n).map(node_line).collect();
    append(&old, &body);
    append(&new, &node_line(n));

    let mut got = Vec::new();
    let mut polls = 0;
    while tail.current_file() != Some(new.as_path()) || tail.backlogged() || polls == 0 {
        got.extend(poll(&mut tail));
        polls += 1;
        assert!(polls < 100, "never rotated");
    }
    got.extend(poll(&mut tail));
    assert!(polls > 1, "the backlog took more than one poll");
    assert_eq!(got, (0..=n).collect::<Vec<_>>());
    assert_eq!(scout.looks(), 1, "the scout was not asked again");
}

#[test]
fn the_scout_looks_when_asked_and_on_its_timer() {
    let t = tempfile::tempdir().unwrap();
    touch(t.path(), "20261119", "8");
    let (mut scout, arrivals) = scout(t.path(), Duration::from_secs(60));
    let mut tail = NodeTailer::with_scout(t.path(), MAX_LINE, arrivals);
    poll(&mut tail);
    let t0 = Instant::now();
    assert!(scout.tick(t0).unwrap(), "the first tick looks");
    assert!(!scout.tick(t0 + Duration::from_secs(59)).unwrap());
    assert!(scout.tick(t0 + Duration::from_secs(60)).unwrap(), "timer");
    assert!(!scout.tick(t0 + Duration::from_secs(61)).unwrap());

    assert!(!tail.look_pending());
    tail.request_rescan();
    assert!(tail.look_pending(), "asked, not answered yet");
    assert!(scout.tick(t0 + Duration::from_secs(61)).unwrap(), "asked");
    assert!(!tail.look_pending());
    assert!(!scout.tick(t0 + Duration::from_secs(62)).unwrap());
    assert_eq!(scout.looks(), 3);
}

/// Until it has found a file the scout looks every 10 ms at most, whatever
/// its interval.
#[test]
fn without_a_file_the_scout_looks_often() {
    let t = tempfile::tempdir().unwrap();
    let (mut scout, _arrivals) = scout(t.path(), Duration::from_secs(60));
    let t0 = Instant::now();
    assert!(scout.tick(t0).unwrap());
    assert!(!scout.tick(t0 + Duration::from_millis(9)).unwrap());
    assert!(scout.tick(t0 + Duration::from_millis(10)).unwrap());
}

/// The writer thread calls its hook between chunks of a backlog, so the
/// scout waits for one chunk's write at most.
#[test]
fn the_writer_calls_its_hook_between_chunks() {
    let chunks = 5;
    let (mut sink, source) = ring(chunks * WRITE_CHUNK);
    for i in 0..chunks * WRITE_CHUNK {
        hl_capture::ObservationSink::observe(
            &mut sink,
            Observation {
                source: Source::Node,
                block_time_ms: 1,
                block_number: Some(u64::try_from(i).unwrap()),
                recv_ns: 1,
            },
        );
    }
    drop(sink);
    let mut calls = 0_usize;
    let mut out = Vec::new();
    let written = source
        .run_with(&mut out, Duration::from_millis(1), || {
            calls += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(usize::try_from(written).unwrap(), chunks * WRITE_CHUNK);
    assert!(calls >= chunks, "{calls} hook calls for {chunks} chunks");
}

#[test]
fn a_hook_error_ends_the_writer() {
    // The reader's end is gone, so a writer that ignores its hook returns
    // instead of idling for ever.
    let (sink, source) = ring(4);
    drop(sink);
    let err = source
        .run_with(Vec::new(), Duration::from_millis(1), || {
            Err(std::io::Error::other("walk failed"))
        })
        .unwrap_err();
    assert_eq!(err.to_string(), "walk failed");
}
