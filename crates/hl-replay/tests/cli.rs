//! Contract tests for the `hl-replay` binary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use hl_replay::{LatencyHist, GIT_SHA};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hl-replay"))
}
fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hl-replay-cli-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn the_build_knows_its_commit() {
    assert!(GIT_SHA.len() >= 40, "{GIT_SHA}");
    assert!(GIT_SHA[..40].bytes().all(|b| b.is_ascii_hexdigit()), "{GIT_SHA}");
}

#[test]
fn a_small_run_prints_the_report_and_writes_the_raw_histograms() {
    let out = tmp("run");
    let o = bin()
        .args(["--arms", "busy-poll,tokio-current-thread,file-poll-100us"])
        .args(["--rounds", "2", "--lines", "200", "--warmup", "20", "--interval-us", "30"])
        .args(["--no-pin", "--tail-dir"])
        .arg(tmp("tail"))
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    for want in [
        &format!("commit: {GIT_SHA}"),
        "kernel: ",
        "cpu: ",
        "cores: ",
        "pinning: none",
        "reader=real",
        "parser=real (hl_latency::parse_node_line)",
        "book=stub",
        "decision=stub",
        "signer=prototype",
        "send=prototype",
        "busy-poll",
        "tokio-current-thread",
        "file-poll-100us",
        "t2-t0",
        "t1-t0",
        "p50",
        "p99.9",
        "max",
        "n=360",
        "rounds: 2",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    let lower = text.to_lowercase();
    for banned in ["mean", "avg", "average"] {
        assert!(!lower.contains(banned), "no averages: found {banned:?}");
    }
    assert_eq!(std::fs::read_to_string(out.join("report.txt")).unwrap(), text);
    for arm in ["busy-poll", "tokio-current-thread", "file-poll-100us"] {
        for metric in ["t2-t0", "t1-t0", "feed-t0"] {
            let raw = std::fs::read(out.join(format!("{arm}.{metric}.hdr"))).unwrap();
            let h = LatencyHist::read_v2(&mut raw.as_slice()).unwrap();
            assert_eq!(h.count(), 360, "{arm}.{metric}");
            let dump = std::fs::read_to_string(out.join(format!("{arm}.{metric}.buckets.txt"))).unwrap();
            assert!(dump.starts_with("# value_ns count"));
        }
    }
}

#[test]
fn a_non_loopback_sink_is_refused_before_anything_runs() {
    for bad in ["8.8.8.8:443", "0.0.0.0:9000", "api.hyperliquid.xyz:443"] {
        let o = bin().args(["--sink-bind", bad, "--lines", "10"]).output().unwrap();
        assert_eq!(o.status.code(), Some(2), "{bad}");
        assert!(stderr(&o).contains("loopback"), "{bad}: {}", stderr(&o));
        assert!(stdout(&o).is_empty());
    }
}

#[test]
fn bad_arguments_are_usage_errors() {
    for args in [
        &["--arms", "nope"][..],
        &["--rounds", "0"],
        &["--lines", "x"],
        &["--frobnicate"],
        &["--lines"],
        &["--input", "/nonexistent/hl-replay-input.jsonl"],
    ] {
        let o = bin().args(args).output().unwrap();
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert!(!stderr(&o).is_empty());
    }
    let o = bin().arg("--help").output().unwrap();
    assert!(o.status.success());
    assert!(stdout(&o).contains("usage: hl-replay"));
}

#[test]
fn an_unavailable_arm_is_reported_not_fatal() {
    let o = bin()
        .args(["--arms", "compio,busy-poll", "--rounds", "1", "--lines", "100"])
        .args(["--warmup", "10", "--interval-us", "30", "--no-pin"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    let compio = text.lines().find(|l| l.starts_with("compio ")).unwrap();
    assert!(compio.contains("unavailable") || compio.contains("n=90"), "{compio}");
    assert!(text.contains("n=90"));
}
