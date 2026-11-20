//! Contract of the `hl-capture` binary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write as _;

use std::fs;
use std::process::Command;
use std::time::Duration;

use hl_latency::{read_observations, Source};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hl-capture"))
}

#[test]
fn usage_errors_exit_2() {
    assert_eq!(bin().output().unwrap().status.code(), Some(2));
    assert_eq!(bin().arg("bogus").output().unwrap().status.code(), Some(2));
    assert_eq!(
        bin()
            .args(["node", "/tmp", "--out"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn node_arm_captures_lines_from_a_file_that_appears_during_the_run() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("node_raw_book_diffs");
    let out = t.path().join("obs.jsonl");
    let mut child = bin()
        .arg("node")
        .arg(&root)
        .arg("--out")
        .arg(&out)
        .args(["--duration-s", "2", "--poll-us", "1000"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("8"),
        "{\"local_time\":\"2026-11-19T08:00:00.071\",\"block_time\":\"2026-11-19T08:00:00.070\",\"block_number\":2,\"events\":[]}\n\
         {\"local_time\":\"2026-11-19T08:00:00.141\",\"block_time\":\"2026-11-19T08:00:00.140\",\"block_number\":3,\"events\":[]}\n",
    )
    .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
    let obs = read_observations(std::io::BufReader::new(fs::File::open(&out).unwrap())).unwrap();
    let heights: Vec<_> = obs.iter().map(|o| o.block_number.unwrap()).collect();
    assert_eq!(heights, vec![2, 3]);
    assert!(obs
        .iter()
        .all(|o| o.source == Source::Node && o.recv_ns > 0));
}

#[test]
fn duration_bounds_the_run_even_with_a_long_poll() {
    let t = tempfile::tempdir().unwrap();
    let start = std::time::Instant::now();
    let status = bin()
        .arg("node")
        .arg(t.path())
        .arg("--out")
        .arg(t.path().join("obs.jsonl"))
        .args(["--duration-s", "1", "--poll-us", "3000000"])
        .status()
        .unwrap();
    let took = start.elapsed();
    assert!(status.success());
    assert!(took < Duration::from_millis(1800), "ran {took:?}");
}

#[test]
fn max_line_bytes_flag_is_accepted() {
    let t = tempfile::tempdir().unwrap();
    let status = bin()
        .arg("node")
        .arg(t.path())
        .arg("--out")
        .arg(t.path().join("obs.jsonl"))
        .args(["--duration-s", "0", "--max-line-bytes", "4096"])
        .status()
        .unwrap();
    assert!(status.success());
}

/// A busy input must not hold the run past `--duration-s`: the deadline is
/// checked between bounded batches, not only around a whole drain.
#[test]
fn duration_bounds_the_run_under_a_busy_input() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("node_raw_book_diffs");
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    // 600,000 valid lines, staged outside the root and renamed in at once.
    let mut body = String::with_capacity(600_000 * 120);
    for h in 0..600_000u64 {
        writeln!(
            body,
            "{{\"local_time\":\"2026-11-19T08:00:00.071\",\"block_time\":\"2026-11-19T08:00:00.070\",\"block_number\":{h},\"events\":[]}}"
        )
        .unwrap();
    }
    let staged = t.path().join("staged");
    fs::write(&staged, body).unwrap();
    let out = t.path().join("obs.jsonl");
    let start = std::time::Instant::now();
    let mut child = bin()
        .arg("node")
        .arg(&root)
        .arg("--out")
        .arg(&out)
        .args(["--duration-s", "1", "--poll-us", "1000"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    fs::rename(&staged, dir.join("8")).unwrap();
    let status = child.wait().unwrap();
    let took = start.elapsed();
    assert!(status.success());
    assert!(took < Duration::from_millis(1800), "ran {took:?}");
    let obs = read_observations(std::io::BufReader::new(fs::File::open(&out).unwrap())).unwrap();
    let heights: Vec<_> = obs.iter().map(|o| o.block_number.unwrap()).collect();
    assert!(!heights.is_empty(), "some of the burst was captured");
    assert_eq!(
        heights,
        (0..u64::try_from(heights.len()).unwrap()).collect::<Vec<_>>(),
        "a prefix, in order"
    );
}
