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
        // A ring that holds the whole burst: nothing can be dropped, so the
        // output must be a gap-free prefix.
        .args(["--duration-s", "1", "--poll-us", "1000"])
        .args(["--ring-records", "1048576"])
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

// ---- ring to disk, counters ---------------------------------------------------

/// The counters object the binary prints on stderr at exit.
fn counters(stderr: &[u8]) -> serde_json::Value {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .rev()
        .find(|l| l.starts_with("{\"lines\""))
        .unwrap_or_else(|| panic!("no counters line in: {text}"));
    serde_json::from_str(line).unwrap()
}

fn burst(lines: u64) -> String {
    let mut body = String::new();
    for h in 0..lines {
        writeln!(
            body,
            "{{\"local_time\":\"2026-11-19T08:00:00.071\",\"block_time\":\"2026-11-19T08:00:00.070\",\"block_number\":{h},\"events\":[]}}"
        )
        .unwrap();
    }
    body
}

#[test]
fn counters_report_what_was_written_and_what_qualifies_it() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("node_raw_book_diffs");
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("8"), "").unwrap();
    let out = t.path().join("obs.jsonl");
    let child = bin()
        .arg("node")
        .arg(&root)
        .arg("--out")
        .arg(&out)
        .args(["--duration-s", "1", "--poll-us", "1000", "--rescan-ms", "5"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("8"))
        .unwrap();
    std::io::Write::write_all(&mut f, burst(3).as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let c = counters(&output.stderr);
    for (key, want) in [
        ("lines", 3),
        ("written", 3),
        ("ring_dropped", 0),
        ("scan_fallbacks", 0),
        ("bad_lines", 0),
        ("rotations", 0),
        ("post_rotation_lines", 0),
        ("height_breaks", 0),
    ] {
        assert_eq!(c[key], want, "{key} in {c}");
    }
    assert!(c["rescans"].as_u64().unwrap() >= 2, "timer rescans: {c}");
    let obs = read_observations(std::io::BufReader::new(fs::File::open(&out).unwrap())).unwrap();
    assert_eq!(obs.len(), 3);
}

/// Full-ring policy: the newest record is dropped and counted; the reader
/// is never blocked, so everything that was written is in order and the
/// counters add up.
#[test]
fn a_full_ring_drops_the_newest_and_counts_instead_of_blocking() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("node_raw_book_diffs");
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    let staged = t.path().join("staged");
    fs::write(&staged, burst(200_000)).unwrap();
    let out = t.path().join("obs.jsonl");
    let child = bin()
        .arg("node")
        .arg(&root)
        .arg("--out")
        .arg(&out)
        .args([
            "--duration-s",
            "2",
            "--poll-us",
            "1000",
            "--ring-records",
            "8",
        ])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    fs::rename(&staged, dir.join("8")).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let c = counters(&output.stderr);
    let (lines, written, dropped) = (
        c["lines"].as_u64().unwrap(),
        c["written"].as_u64().unwrap(),
        c["ring_dropped"].as_u64().unwrap(),
    );
    assert_eq!(lines, 200_000, "the reader was not held back: {c}");
    assert!(dropped > 0, "an 8-record ring cannot hold the burst: {c}");
    assert_eq!(written + dropped, lines, "{c}");
    let obs = read_observations(std::io::BufReader::new(fs::File::open(&out).unwrap())).unwrap();
    assert_eq!(u64::try_from(obs.len()).unwrap(), written);
    let heights: Vec<_> = obs.iter().map(|o| o.block_number.unwrap()).collect();
    assert!(heights.windows(2).all(|w| w[0] < w[1]), "order is kept");
}

#[test]
fn ring_and_rescan_flags_reject_zero() {
    for flag in ["--ring-records", "--rescan-ms"] {
        let status = bin()
            .args(["node", "/tmp", "--out", "/dev/null", "--duration-s", "0"])
            .args([flag, "0"])
            .output()
            .unwrap()
            .status;
        assert_eq!(status.code(), Some(2), "{flag} 0");
    }
}
