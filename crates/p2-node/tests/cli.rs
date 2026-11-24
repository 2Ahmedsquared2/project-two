//! `p2-node` CLI contract.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_p2-node"))
}

fn data(f: &str) -> String {
    format!("{}/tests/data/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("p2n-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn replay_null_engine_reports_coverage_and_writes_outputs() {
    let out = tmp("replay");
    let o = bin()
        .args([
            "replay",
            "--blocks",
            &data("three_blocks.jsonl"),
            "--diffs",
            &data("diffs_1000.jsonl"),
        ])
        .args([
            "--snapshot-height",
            "999",
            "--passes",
            "5",
            "--prewarm-passes",
            "1",
            "--window-blocks",
            "0",
        ])
        .args(["--core", "0", "--out", out.to_str().unwrap()])
        .output()
        .unwrap();
    let so = String::from_utf8_lossy(&o.stdout);
    assert!(
        o.status.success(),
        "{so}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    // The null engine executes nothing: only the empty block reproduces.
    assert!(so.contains("per-block outputs matched 1/3"), "{so}");
    assert!(so.contains("actions executed 0/3"), "{so}");
    assert!(so.contains("engine null"), "{so}");
    assert!(so.contains("NOT a like-for-like"), "{so}");
    for f in ["report.txt", "blocks.csv", "warm.txt"] {
        assert!(out.join(f).exists(), "{f}");
    }
    let warm = std::fs::read_to_string(out.join("warm.txt")).unwrap();
    assert!(warm.contains("pin passed"), "{warm}");
    assert!(warm.contains("mlockall"), "{warm}");
    std::fs::remove_dir_all(out).unwrap();
}

#[test]
fn too_few_passes_exit_2() {
    let o = bin()
        .args([
            "replay",
            "--blocks",
            &data("three_blocks.jsonl"),
            "--diffs",
            &data("diffs_1000.jsonl"),
        ])
        .args(["--snapshot-height", "999", "--passes", "3"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn gen_then_replay_synthetic() {
    let d = tmp("gen");
    std::fs::create_dir_all(&d).unwrap();
    let blocks = d.join("blocks.jsonl");
    let diffs = d.join("diffs.jsonl");
    let o = bin()
        .args(["gen", "--blocks", "200", "--actions", "8", "--seed", "7"])
        .args([
            "--out-blocks",
            blocks.to_str().unwrap(),
            "--out-diffs",
            diffs.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let n = std::fs::read_to_string(&blocks).unwrap().lines().count();
    assert_eq!(n, 200);
    // Every generated line decodes with the strict decoder.
    for l in std::fs::read_to_string(&blocks).unwrap().lines() {
        hl_block::decode_block(l).unwrap();
    }
    let o = bin()
        .args([
            "replay",
            "--blocks",
            blocks.to_str().unwrap(),
            "--diffs",
            diffs.to_str().unwrap(),
        ])
        .args([
            "--snapshot-height",
            "0",
            "--passes",
            "5",
            "--engine",
            "echo",
        ])
        .output()
        .unwrap();
    let so = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{so}");
    // The echo engine answers native's response (test oracle, not an engine).
    assert!(so.contains("per-block outputs matched 200/200"), "{so}");
    assert!(so.contains("engine echo"), "{so}");
    std::fs::remove_dir_all(d).unwrap();
}

#[test]
fn usage_exit_2() {
    assert_eq!(bin().output().unwrap().status.code(), Some(2));
    assert_eq!(
        bin().args(["replay"]).output().unwrap().status.code(),
        Some(2)
    );
}

#[test]
fn native_times_summary() {
    let o = bin()
        .args(["native-times", &data("fast_block_times_3.jsonl")])
        .output()
        .unwrap();
    let so = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{so}");
    assert!(
        so.contains("blocks 3, heights 1232430001..=1232430003, contiguous yes"),
        "{so}"
    );
    // nearest-rank p50 of [2629160, 12478919, 26845330] = 12478919
    assert!(so.contains("median 12478919"), "{so}");
    assert!(so.contains("max 26845330"), "{so}");
}
