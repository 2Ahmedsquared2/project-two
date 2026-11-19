//! Contract of the `hl-capture` binary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

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
