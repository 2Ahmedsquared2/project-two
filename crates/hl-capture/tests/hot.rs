//! The capture loop's view of the tailer: `poll_into` with rescans only on
//! request, and the counters that qualify its stamps.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use hl_capture::{NodeTailer, Switch};
use hl_latency::Observation;

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
fn poll_into_walks_the_directory_only_when_asked() {
    let t = tempfile::tempdir().unwrap();
    let old = touch(t.path(), "20261119", "8");
    let mut tail = NodeTailer::new(t.path());
    assert_eq!(poll(&mut tail), Vec::<u64>::new());
    assert_eq!(tail.current_file(), Some(old.as_path()));
    assert_eq!(tail.stats().rescans, 1, "the first poll looks for a file");

    let new = touch(t.path(), "20261119", "9");
    append(&new, &node_line(11));
    append(&old, &node_line(10));
    assert_eq!(poll(&mut tail), vec![10], "the current file is still read");
    for _ in 0..50 {
        assert_eq!(poll(&mut tail), Vec::<u64>::new());
    }
    assert_eq!(tail.stats().rescans, 1, "no walk on the poll path");
    assert_eq!(tail.current_file(), Some(old.as_path()));

    tail.request_rescan();
    assert_eq!(poll(&mut tail), vec![11], "new file from byte 0");
    assert_eq!(tail.current_file(), Some(new.as_path()));
    let s = tail.stats();
    assert_eq!((s.rescans, s.rotations), (2, 1));
    // One request is one walk.
    poll(&mut tail);
    assert_eq!(tail.stats().rescans, 2);
}

#[test]
fn a_rescan_request_waits_until_the_old_file_is_drained() {
    let t = tempfile::tempdir().unwrap();
    let old = touch(t.path(), "20261119", "8");
    let mut tail = NodeTailer::new(t.path());
    poll(&mut tail);
    // More than one poll's read budget in the old file.
    let mut body = String::new();
    let mut n = 0;
    while body.len() < (hl_capture::MAX_READS_PER_POLL + 2) * 64 * 1024 {
        body.push_str(&node_line(n));
        n += 1;
    }
    append(&old, &body);
    let new = touch(t.path(), "20261119", "9");
    append(&new, &node_line(n));
    tail.request_rescan();
    let mut heights = poll(&mut tail);
    assert!(tail.backlogged());
    assert_eq!(
        tail.stats().rescans,
        1,
        "no walk while the old file has data"
    );
    let mut polls = 0;
    while tail.current_file() != Some(new.as_path()) {
        heights.extend(poll(&mut tail));
        polls += 1;
        assert!(polls < 100, "the request was lost");
    }
    assert_eq!(heights, (0..=n).collect::<Vec<_>>());
    assert_eq!(tail.stats().rescans, 2);
}

#[test]
fn lines_read_by_the_switching_poll_are_reported() {
    let t = tempfile::tempdir().unwrap();
    let old = touch(t.path(), "20261119", "8");
    let mut tail = NodeTailer::new(t.path());
    poll(&mut tail);
    assert_eq!(
        tail.take_switch(),
        None,
        "the first file is followed from its end"
    );
    append(&old, &node_line(1));
    poll(&mut tail);
    assert_eq!(tail.stats().post_rotation_lines, 0);

    let new = touch(t.path(), "20261119", "9");
    append(&new, &node_line(2));
    append(&new, &node_line(3));
    tail.request_rescan();
    assert_eq!(poll(&mut tail), vec![2, 3]);
    assert_eq!(
        tail.take_switch(),
        Some(Switch {
            lines: 2,
            first_block_number: Some(2),
            last_block_number: Some(3),
        })
    );
    assert_eq!(tail.take_switch(), None, "reported once");
    append(&new, &node_line(4));
    assert_eq!(poll(&mut tail), vec![4]);
    assert_eq!(
        tail.stats().post_rotation_lines,
        2,
        "later lines are on time"
    );
    assert_eq!(tail.take_switch(), None);
}

#[test]
fn a_file_that_appears_after_start_is_a_switch_too() {
    let t = tempfile::tempdir().unwrap();
    let mut tail = NodeTailer::new(t.path());
    poll(&mut tail);
    let f = touch(t.path(), "20261119", "8");
    append(&f, &node_line(7));
    tail.request_rescan();
    assert_eq!(poll(&mut tail), vec![7]);
    assert_eq!(tail.take_switch().map(|s| s.lines), Some(1));
    assert_eq!(tail.stats().post_rotation_lines, 1);
    assert_eq!(tail.stats().rotations, 0, "nothing was rotated away from");
}

#[test]
fn lines_only_the_strict_parser_accepts_are_counted_as_fallbacks() {
    let t = tempfile::tempdir().unwrap();
    let f = touch(t.path(), "20261119", "8");
    let mut tail = NodeTailer::new(t.path());
    poll(&mut tail);
    append(&f, &node_line(1));
    // Valid block-info line, keys in another order: not the header shape.
    append(
        &f,
        "{\"block_number\":2,\"block_time\":\"2026-11-19T08:00:00.140\",\"local_time\":\"x\",\"events\":[]}\n",
    );
    append(&f, "not json\n");
    assert_eq!(poll(&mut tail), vec![1, 2]);
    let s = tail.stats();
    assert_eq!((s.lines, s.scan_fallbacks, s.bad_lines), (2, 1, 1));
}

#[test]
fn height_breaks_count_block_numbers_that_do_not_follow_on() {
    let t = tempfile::tempdir().unwrap();
    let f = touch(t.path(), "20261119", "8");
    let mut tail = NodeTailer::new(t.path());
    poll(&mut tail);
    for h in [5, 6, 7] {
        append(&f, &node_line(h));
    }
    poll(&mut tail);
    assert_eq!(
        tail.stats().height_breaks,
        0,
        "the first height is no break"
    );
    for h in [9, 9, 10, 4] {
        append(&f, &node_line(h));
    }
    poll(&mut tail);
    assert_eq!(
        tail.stats().height_breaks,
        3,
        "a gap, a repeat, a step back"
    );
}

/// Issue #40: a repeated height is not the same finding as a missing or an
/// out-of-order one, so each kind has its own counter (the definitions
/// `hl-strict-pass` uses) and `height_breaks` stays their sum.
#[test]
fn height_breaks_are_split_into_repeats_gaps_and_backward_steps() {
    let t = tempfile::tempdir().unwrap();
    let f = touch(t.path(), "20261119", "8");
    let mut tail = NodeTailer::new(t.path());
    poll(&mut tail);
    let kinds = |tail: &NodeTailer| {
        let s = tail.stats();
        (
            s.height_repeats,
            s.height_gaps,
            s.height_backwards,
            s.height_breaks,
        )
    };
    // Several lines of one block, then the next block: repeats only.
    for h in [5, 5, 5, 6, 6, 7] {
        append(&f, &node_line(h));
    }
    poll(&mut tail);
    assert_eq!(kinds(&tail), (3, 0, 0, 3));
    // A gap of one block and a gap of many.
    for h in [9, 100] {
        append(&f, &node_line(h));
    }
    poll(&mut tail);
    assert_eq!(kinds(&tail), (3, 2, 0, 5));
    // A step back, by one and by many; the step forward after it is a gap.
    for h in [99, 4, 5, 50] {
        append(&f, &node_line(h));
    }
    poll(&mut tail);
    assert_eq!(kinds(&tail), (3, 3, 2, 8));
    // Carried across a rotation: the first line of the next file repeats.
    let g = touch(t.path(), "20261119", "9");
    append(&g, &node_line(50));
    append(&g, &node_line(51));
    tail.request_rescan();
    poll(&mut tail);
    assert_eq!(kinds(&tail), (4, 3, 2, 9));
}
