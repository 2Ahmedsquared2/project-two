//! Behaviour of the capture arms, against fake node files and synthetic frames.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::as_conversions,
    clippy::assert_is_empty
)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use hl_capture::{hourly_key, newest_hourly_file, LineSplitter, NodeTailer, StampedLine, WsFilter};
use hl_latency::{Observation, Source};
use proptest::prelude::*;

// ---- line splitter ----------------------------------------------------------

fn split(chunks: &[(&[u8], u64)]) -> (Vec<StampedLine>, usize) {
    let mut s = LineSplitter::new();
    let mut out = Vec::new();
    for (c, ns) in chunks {
        s.push(c, *ns, &mut out);
    }
    (out, s.pending())
}

#[test]
fn whole_lines_take_their_read_stamp() {
    let (out, pending) = split(&[(b"a\nbb\n", 7)]);
    assert_eq!(
        out,
        vec![
            StampedLine {
                bytes: b"a".to_vec(),
                recv_ns: 7
            },
            StampedLine {
                bytes: b"bb".to_vec(),
                recv_ns: 7
            },
        ]
    );
    assert_eq!(pending, 0);
}

#[test]
fn split_line_takes_the_stamp_of_the_read_that_completed_it() {
    let (out, pending) = split(&[(b"{\"x\":", 1), (b"1}", 2), (b"\nnext", 3)]);
    assert_eq!(
        out,
        vec![StampedLine {
            bytes: b"{\"x\":1}".to_vec(),
            recv_ns: 3
        }]
    );
    assert_eq!(pending, 4, "`next` is held until its newline arrives");
}

#[test]
fn crlf_and_empty_lines() {
    let (out, _) = split(&[(b"a\r\n\n", 5)]);
    assert_eq!(out.len(), 1, "empty lines are skipped");
    assert_eq!(out[0].bytes, b"a");
}

proptest! {
    /// However a stream is cut into reads, the lines are the same, in order.
    #[test]
    fn chunking_does_not_change_lines(
        lines in prop::collection::vec("[a-z0-9{}\":,]{1,20}", 0..20),
        cuts in prop::collection::vec(1usize..15, 0..40),
    ) {
        let stream: Vec<u8> = lines.iter().flat_map(|l| format!("{l}\n").into_bytes()).collect();
        let mut s = LineSplitter::new();
        let mut out = Vec::new();
        let mut rest = &stream[..];
        let mut ns = 0;
        for c in cuts.into_iter().chain(std::iter::repeat(7)) {
            if rest.is_empty() { break; }
            let n = c.min(rest.len());
            ns += 1;
            s.push(&rest[..n], ns, &mut out);
            rest = &rest[n..];
        }
        let got: Vec<String> = out.iter().map(|l| String::from_utf8(l.bytes.clone()).unwrap()).collect();
        prop_assert_eq!(got, lines);
        prop_assert_eq!(s.pending(), 0);
        // Stamps never go backwards.
        prop_assert!(out.windows(2).all(|w| w[0].recv_ns <= w[1].recv_ns));
    }
}

// ---- hourly file discovery --------------------------------------------------

#[test]
fn hourly_key_parses_date_and_unpadded_hour() {
    assert_eq!(
        hourly_key(Path::new("/d/hourly/20261119/9")),
        Some((20_261_119, 9))
    );
    assert_eq!(
        hourly_key(Path::new("x/20261119/23")),
        Some((20_261_119, 23))
    );
    assert_eq!(hourly_key(Path::new("x/20261119/24")), None);
    assert_eq!(hourly_key(Path::new("x/2026111/9")), None);
    assert_eq!(hourly_key(Path::new("x/20261119/a")), None);
    assert_eq!(hourly_key(Path::new("9")), None);
}

fn touch(root: &Path, date: &str, hour: &str) -> PathBuf {
    let dir = root.join("hourly").join(date);
    fs::create_dir_all(&dir).unwrap();
    let p = dir.join(hour);
    fs::write(&p, b"").unwrap();
    p
}

#[test]
fn newest_is_numeric_not_lexical() {
    let t = tempfile::tempdir().unwrap();
    assert_eq!(newest_hourly_file(t.path()).unwrap(), None);
    touch(t.path(), "20261119", "9");
    let ten = touch(t.path(), "20261119", "10");
    touch(t.path(), "20261118", "23");
    fs::write(t.path().join("hourly").join("README"), b"").unwrap();
    assert_eq!(newest_hourly_file(t.path()).unwrap(), Some(ten.clone()));
    let next_day = touch(t.path(), "20261120", "0");
    assert_eq!(newest_hourly_file(t.path()).unwrap(), Some(next_day));
}

// ---- node tailer ------------------------------------------------------------

fn node_line(time: &str, height: u64) -> String {
    format!(
        "{{\"local_time\":\"{time}\",\"block_time\":\"{time}\",\"block_number\":{height},\"events\":[]}}\n"
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

/// A clock that returns 100, 200, 300, ... and counts calls.
fn ticking() -> (impl FnMut() -> u64, std::rc::Rc<std::cell::Cell<u64>>) {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let c = calls.clone();
    (
        move || {
            c.set(c.get() + 1);
            c.get() * 100
        },
        calls,
    )
}

fn node_obs(ms: u64, h: u64, ns: u64) -> Observation {
    Observation {
        source: Source::Node,
        block_time_ms: ms,
        block_number: Some(h),
        recv_ns: ns,
    }
}

#[test]
fn first_file_is_read_from_its_end_then_follows_appends() {
    let t = tempfile::tempdir().unwrap();
    let f = touch(t.path(), "20261119", "8");
    append(&f, &node_line("2026-11-19T08:00:00.000", 1)); // history: not an arrival
    let mut tail = NodeTailer::new(t.path());
    let (mut clock, calls) = ticking();
    assert!(tail.poll(&mut clock).unwrap().is_empty());
    assert_eq!(tail.current_file(), Some(f.as_path()));
    assert_eq!(calls.get(), 0, "no data read, no stamp taken");

    append(&f, &node_line("2026-11-19T08:00:00.070", 2));
    let got = tail.poll(&mut clock).unwrap();
    assert_eq!(got, vec![node_obs(1_795_075_200_070, 2, 100)]);
    assert_eq!(calls.get(), 1, "one stamp per read that returned data");
    assert!(tail.poll(&mut clock).unwrap().is_empty());
}

#[test]
fn partial_line_is_stamped_when_completed() {
    let t = tempfile::tempdir().unwrap();
    let f = touch(t.path(), "20261119", "8");
    let mut tail = NodeTailer::new(t.path());
    let (mut clock, _) = ticking();
    tail.poll(&mut clock).unwrap();
    let line = node_line("2026-11-19T08:00:00.070", 2);
    let (a, b) = line.split_at(20);
    append(&f, a);
    assert!(tail.poll(&mut clock).unwrap().is_empty());
    append(&f, b);
    let got = tail.poll(&mut clock).unwrap();
    assert_eq!(
        got,
        vec![node_obs(1_795_075_200_070, 2, 200)],
        "second read's stamp"
    );
}

#[test]
fn rotation_drains_old_file_then_reads_new_one_from_start() {
    let t = tempfile::tempdir().unwrap();
    let old = touch(t.path(), "20261119", "8");
    let mut tail = NodeTailer::new(t.path());
    let (mut clock, _) = ticking();
    tail.poll(&mut clock).unwrap();
    append(&old, &node_line("2026-11-19T08:59:59.990", 10));
    let new = touch(t.path(), "20261119", "9");
    append(&new, &node_line("2026-11-19T09:00:00.060", 11));
    let got = tail.poll(&mut clock).unwrap();
    let heights: Vec<_> = got.iter().map(|o| o.block_number.unwrap()).collect();
    assert_eq!(
        heights,
        vec![10, 11],
        "old file's tail first, then the new file from byte 0"
    );
    assert_eq!(tail.current_file(), Some(new.as_path()));
    assert_eq!(tail.stats().rotations, 1);
}

#[test]
fn bad_lines_are_counted_not_fatal() {
    let t = tempfile::tempdir().unwrap();
    let f = touch(t.path(), "20261119", "8");
    let mut tail = NodeTailer::new(t.path());
    let (mut clock, _) = ticking();
    tail.poll(&mut clock).unwrap();
    append(&f, "not json\n");
    append(&f, &node_line("2026-11-19T08:00:00.070", 2));
    let got = tail.poll(&mut clock).unwrap();
    assert_eq!(got.len(), 1);
    let s = tail.stats();
    assert_eq!((s.lines, s.bad_lines), (1, 1));
}

#[test]
fn waits_for_a_file_to_appear() {
    let t = tempfile::tempdir().unwrap();
    let mut tail = NodeTailer::new(t.path());
    let (mut clock, _) = ticking();
    assert!(tail.poll(&mut clock).unwrap().is_empty());
    assert_eq!(tail.current_file(), None);
    // The first file created after start is new: read from its start.
    let f = touch(t.path(), "20261119", "8");
    append(&f, &node_line("2026-11-19T08:00:00.070", 2));
    assert_eq!(tail.poll(&mut clock).unwrap().len(), 1);
}

// ---- WS filter --------------------------------------------------------------

fn l2(coin: &str, time: u64) -> String {
    format!("{{\"channel\":\"l2Book\",\"data\":{{\"coin\":\"{coin}\",\"time\":{time},\"levels\":[[],[]]}}}}")
}
fn trades(coin: &str, times: &[u64]) -> String {
    let t: Vec<String> = times
        .iter()
        .map(|t| format!("{{\"coin\":\"{coin}\",\"side\":\"B\",\"px\":\"1\",\"sz\":\"1\",\"time\":{t},\"tid\":1}}"))
        .collect();
    format!("{{\"channel\":\"trades\",\"data\":[{}]}}", t.join(","))
}
fn ws_obs(ms: u64, ns: u64) -> Observation {
    Observation {
        source: Source::PublicWs,
        block_time_ms: ms,
        block_number: None,
        recv_ns: ns,
    }
}

#[test]
fn first_frame_per_channel_and_coin_is_dropped() {
    let mut f = WsFilter::new();
    assert!(f
        .on_frame(r#"{"channel":"subscriptionResponse","data":{}}"#, 1)
        .is_empty());
    assert!(
        f.on_frame(&l2("BTC", 10), 2).is_empty(),
        "BTC l2Book snapshot"
    );
    assert!(
        f.on_frame(&l2("ETH", 11), 3).is_empty(),
        "ETH l2Book snapshot"
    );
    assert!(
        f.on_frame(&trades("BTC", &[5, 6]), 4).is_empty(),
        "BTC trades snapshot"
    );
    assert_eq!(f.on_frame(&l2("BTC", 20), 5), vec![ws_obs(20, 5)]);
    assert_eq!(
        f.on_frame(&trades("BTC", &[21, 21, 22]), 6),
        vec![ws_obs(21, 6), ws_obs(22, 6)]
    );
    let s = f.stats();
    assert_eq!((s.frames, s.snapshots_dropped, s.bad_frames), (6, 3, 0));
}

#[test]
fn reset_makes_the_next_frame_a_snapshot_again() {
    let mut f = WsFilter::new();
    f.on_frame(&l2("BTC", 10), 1);
    assert_eq!(f.on_frame(&l2("BTC", 20), 2).len(), 1);
    f.reset();
    assert!(f.on_frame(&l2("BTC", 30), 3).is_empty());
    assert_eq!(f.on_frame(&l2("BTC", 40), 4), vec![ws_obs(40, 4)]);
}

#[test]
fn malformed_frames_are_counted() {
    let mut f = WsFilter::new();
    assert!(f.on_frame("{oops", 1).is_empty());
    assert!(f
        .on_frame(r#"{"channel":"l2Book","data":{"coin":"BTC"}}"#, 2)
        .is_empty());
    assert_eq!(f.stats().bad_frames, 2);
}

#[test]
fn real_captured_frames_pass_after_snapshot() {
    let book = include_str!("../../hl-latency/tests/fixtures/ws_l2Book.json").trim();
    let mut f = WsFilter::new();
    assert!(f.on_frame(book, 1).is_empty());
    assert_eq!(f.on_frame(book, 2), vec![ws_obs(1_795_128_590_474, 2)]);
}
