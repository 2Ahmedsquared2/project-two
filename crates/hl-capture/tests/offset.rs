//! Issue #44: every node observation says which hourly file and which byte
//! of it its line starts at, so an offline check of the files can be matched
//! to exactly one observation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use hl_capture::{ring, NodeTailer, ObservationSink};
use hl_latency::{read_observations, Observation, Source};

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

fn poll(tail: &mut NodeTailer) -> Vec<Observation> {
    let mut out: Vec<Observation> = Vec::new();
    tail.poll_into(&mut || 1, &mut out).unwrap();
    out
}

/// `(height, date, hour, offset)` of each observation.
fn places(obs: &[Observation]) -> Vec<(u64, u32, u8, u64)> {
    obs.iter()
        .map(|o| {
            (
                o.block_number.unwrap(),
                o.file_date.expect("file_date"),
                o.file_hour.expect("file_hour"),
                o.line_offset.expect("line_offset"),
            )
        })
        .collect()
}

#[test]
fn the_first_line_of_a_new_file_is_at_offset_0() {
    let t = tempfile::tempdir().unwrap();
    let mut tail = NodeTailer::new(t.path());
    assert_eq!(poll(&mut tail), Vec::new());
    let f = touch(t.path(), "20261119", "8");
    let (one, two, three) = (node_line(1), node_line(22), node_line(333));
    append(&f, &format!("{one}{two}{three}"));
    tail.request_rescan();
    let la = u64::try_from(one.len()).unwrap();
    let lb = u64::try_from(two.len()).unwrap();
    assert_eq!(
        places(&poll(&mut tail)),
        vec![
            (1, 20_261_119, 8, 0),
            (22, 20_261_119, 8, la),
            (333, 20_261_119, 8, la + lb)
        ]
    );
}

#[test]
fn a_file_followed_from_its_end_counts_from_the_real_file_position() {
    let t = tempfile::tempdir().unwrap();
    let f = touch(t.path(), "20261119", "8");
    let history = format!("{}{}", node_line(1), node_line(2));
    append(&f, &history);
    let mut tail = NodeTailer::new(t.path());
    assert_eq!(poll(&mut tail), Vec::new(), "history is not an arrival");
    let (third, fourth) = (node_line(3), node_line(4));
    append(&f, &third);
    let before = u64::try_from(history.len()).unwrap();
    assert_eq!(places(&poll(&mut tail)), vec![(3, 20_261_119, 8, before)]);
    // A line that arrives in two reads keeps the offset of its first byte.
    let (head, rest) = fourth.split_at(17);
    append(&f, head);
    assert_eq!(poll(&mut tail), Vec::new());
    append(&f, rest);
    assert_eq!(
        places(&poll(&mut tail)),
        vec![(
            4,
            20_261_119,
            8,
            before + u64::try_from(third.len()).unwrap()
        )]
    );
}

/// The node can be in the middle of a line when the first file is entered
/// at its end. The rest of that line is not an observation (its header is
/// gone), so no observation carries an offset that is not a line start, and
/// the next line is at its true position.
#[test]
fn a_line_the_node_was_writing_at_startup_gives_no_observation() {
    let t = tempfile::tempdir().unwrap();
    let f = touch(t.path(), "20261119", "8");
    let whole = node_line(1);
    let (written, rest) = whole.split_at(40);
    append(&f, &node_line(0));
    append(&f, written);
    let mut tail = NodeTailer::new(t.path());
    assert_eq!(poll(&mut tail), Vec::new(), "history is not an arrival");
    append(&f, rest);
    append(&f, &node_line(2));
    let got = poll(&mut tail);
    let true_start = u64::try_from(node_line(0).len() + whole.len()).unwrap();
    assert_eq!(places(&got), vec![(2, 20_261_119, 8, true_start)]);
    assert_eq!(tail.stats().bad_lines, 1, "the headless rest is counted");
}

#[test]
fn dropped_lines_still_advance_the_offset() {
    let t = tempfile::tempdir().unwrap();
    let f = touch(t.path(), "20261119", "8");
    // Room for a node line, not for the long one below.
    let mut tail = NodeTailer::with_max_line(t.path(), 200);
    poll(&mut tail);
    let good1 = node_line(1);
    let oversized = format!("{}\n", "x".repeat(500));
    let bad = "not a node line\n";
    let empty = "\n";
    let crlf = format!("{}\r\n", node_line(2).trim_end());
    let good3 = node_line(3);
    append(&f, &format!("{good1}{oversized}{bad}{empty}{crlf}{good3}"));
    let got = poll(&mut tail);
    let at2 = u64::try_from(good1.len() + oversized.len() + bad.len() + empty.len()).unwrap();
    assert_eq!(
        places(&got),
        vec![
            (1, 20_261_119, 8, 0),
            (2, 20_261_119, 8, at2),
            (3, 20_261_119, 8, at2 + u64::try_from(crlf.len()).unwrap())
        ]
    );
    let s = tail.stats();
    assert_eq!((s.oversized_lines, s.bad_lines), (1, 1));
}

#[test]
fn offsets_restart_in_the_next_file() {
    let t = tempfile::tempdir().unwrap();
    let old = touch(t.path(), "20261119", "23");
    let mut tail = NodeTailer::new(t.path());
    poll(&mut tail);
    append(&old, &node_line(1));
    // An unfinished line left in the old file is abandoned at the switch.
    append(&old, "{\"local_time\":\"2026");
    let new = touch(t.path(), "20261120", "0");
    append(&new, &format!("{}{}", node_line(2), node_line(3)));
    tail.request_rescan();
    let mut got = poll(&mut tail);
    got.extend(poll(&mut tail));
    assert_eq!(
        places(&got),
        vec![
            (1, 20_261_119, 23, 0),
            (2, 20_261_120, 0, 0),
            (3, 20_261_120, 0, u64::try_from(node_line(2).len()).unwrap())
        ]
    );
}

/// The point of the fields: a line the header scan accepts but the strict
/// pass fails (a torn line) is found again by `(date, hour, offset)`, and
/// only that line.
#[test]
fn a_torn_lines_offset_is_the_strict_passs_offset() {
    let t = tempfile::tempdir().unwrap();
    let mut tail = NodeTailer::new(t.path());
    poll(&mut tail);
    let f = touch(t.path(), "20261119", "8");
    // Same block as its neighbours (several lines per block), header
    // intact, body cut off: the scan stamps it, the strict pass fails it.
    let whole = r#"{"local_time":"2026-11-19T08:00:00.071123456","block_time":"2026-11-19T08:00:00.070000","block_number":7,"events":[{"user":"0x31ca8395cf837de08b24da3f660e77761dfb974b","oid":90000000001,"coin":"BTC","side":"B","px":"104000.0","raw_book_diff":{"new":{"sz":"0.25"}}}]}"#;
    let torn = format!("{}\n", whole.strip_suffix("}]}").unwrap());
    assert!(
        hl_latency::scan_node_header(hl_latency::CompleteLine::assume_newline_seen(
            torn.trim_end().as_bytes()
        ))
        .is_some(),
        "premise: the header scan accepts the torn line"
    );
    let body = format!("{}{}{torn}{}", node_line(6), node_line(7), node_line(7));
    append(&f, &body);
    tail.request_rescan();
    let got = poll(&mut tail);
    assert_eq!(got.len(), 4, "the scan accepts the torn line");

    let mut pass = hl_strict_pass::StrictPass::new(hl_strict_pass::Options::default());
    pass.check_file(f.to_str().unwrap(), body.as_bytes(), false)
        .unwrap();
    let failures = pass.failures();
    assert_eq!(failures.len(), 1);
    let failure = &failures[0];
    assert!(failure.scan_accepted);
    assert_eq!(
        hl_capture::hourly_key(Path::new(&failure.path)),
        Some((20_261_119, 8))
    );

    let matched: Vec<&Observation> = got
        .iter()
        .filter(|o| {
            (o.file_date, o.file_hour, o.line_offset)
                == (Some(20_261_119), Some(8), Some(failure.offset))
        })
        .collect();
    assert_eq!(matched.len(), 1, "exactly one observation is the torn line");
    assert_eq!(matched[0].block_number, Some(7));
    // The block's two good lines are other observations.
    let same_block = got.iter().filter(|o| o.block_number == Some(7)).count();
    assert_eq!(same_block, 3);
}

/// Through the ring to the output file: all three fields for a node line,
/// none for an observation without them, and old lines still parse.
#[test]
fn the_fields_reach_the_output_file_and_are_optional() {
    let node = Observation {
        source: Source::Node,
        block_time_ms: 1_795_075_200_070,
        block_number: Some(9),
        recv_ns: 5,
        file_date: Some(20_261_119),
        file_hour: Some(8),
        line_offset: Some(5_000_000_000),
    };
    let ws = Observation {
        source: Source::PublicWs,
        block_time_ms: 1_795_075_200_070,
        block_number: None,
        recv_ns: 6,
        file_date: None,
        file_hour: None,
        line_offset: None,
    };
    let (mut sink, mut source) = ring(4);
    sink.observe(node);
    sink.observe(ws);
    let mut out = Vec::new();
    source.write_available(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    let mut lines = text.lines();
    assert_eq!(
        lines.next().unwrap(),
        r#"{"source":"node","block_time_ms":1795075200070,"block_number":9,"recv_ns":5,"file_date":20261119,"file_hour":8,"line_offset":5000000000}"#
    );
    assert_eq!(
        lines.next().unwrap(),
        r#"{"source":"public_ws","block_time_ms":1795075200070,"recv_ns":6}"#
    );
    assert_eq!(read_observations(text.as_bytes()).unwrap(), vec![node, ws]);
    // A file captured before the fields existed.
    let old = r#"{"source":"node","block_time_ms":1795075200070,"block_number":9,"recv_ns":5}"#;
    let parsed = read_observations(old.as_bytes()).unwrap();
    assert_eq!(
        (
            parsed[0].file_date,
            parsed[0].file_hour,
            parsed[0].line_offset
        ),
        (None, None, None)
    );
}

#[test]
fn a_ring_record_is_40_bytes() {
    assert_eq!(hl_capture::RECORD_BYTES, 40);
}
