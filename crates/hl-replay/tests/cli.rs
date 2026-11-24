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
    assert!(
        GIT_SHA[..40].bytes().all(|b| b.is_ascii_hexdigit()),
        "{GIT_SHA}"
    );
}

#[test]
fn a_small_run_prints_the_report_and_writes_the_raw_histograms() {
    let out = tmp("run");
    let o = bin()
        .args(["--arms", "busy-poll,tokio-current-thread,file-poll-100us"])
        .args([
            "--rounds",
            "2",
            "--lines",
            "200",
            "--warmup",
            "20",
            "--interval-us",
            "30",
        ])
        .args(["--no-pin", "--tail-dir"])
        .arg(tmp("tail"))
        .arg("--out")
        .arg(&out)
        .arg("--allow-missing-host-facts")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    for want in [
        &format!("commit: {GIT_SHA}"),
        "host facts: NOT CAPTURED (--allow-missing-host-facts)",
        "kernel: ",
        "cpu: ",
        "cores: ",
        "pinning: none",
        "reader=real",
        "parser=real (hl_latency::scan_node_header, falling back to strict parse_node_line)",
        "parser=header scan",
        "scan-vs-strict: validated=26 mismatches=0 fallbacks=0",
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
    assert_eq!(
        std::fs::read_to_string(out.join("report.txt")).unwrap(),
        text
    );
    for arm in ["busy-poll", "tokio-current-thread", "file-poll-100us"] {
        for metric in ["t2-t0", "t1-t0", "feed-t0"] {
            let raw = std::fs::read(out.join(format!("{arm}.{metric}.hdr"))).unwrap();
            let h = LatencyHist::read_v2(&mut raw.as_slice()).unwrap();
            assert_eq!(h.count(), 360, "{arm}.{metric}");
            let dump =
                std::fs::read_to_string(out.join(format!("{arm}.{metric}.buckets.txt"))).unwrap();
            assert!(dump.starts_with("# value_ns count"));
        }
    }
}

/// Runs `scripts/capture-host.sh` (Linux hosts only) and returns the file it wrote.
#[cfg(target_os = "linux")]
fn captured_facts(name: &str) -> PathBuf {
    let path = tmp(name).join("facts.txt");
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/capture-host.sh");
    let o = Command::new("bash")
        .arg(script)
        .arg(&path)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    path
}
fn tiny_run() -> Command {
    let mut c = bin();
    c.args(["--arms", "busy-poll", "--rounds", "1", "--lines", "60"])
        .args(["--warmup", "10", "--interval-us", "30", "--no-pin"]);
    c
}

#[test]
fn out_without_host_facts_is_refused_before_anything_runs() {
    let out = tmp("nofacts").join("results");
    let o = tiny_run().arg("--out").arg(&out).output().unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    let err = stderr(&o);
    assert!(err.contains("--out needs --host-facts"), "{err}");
    assert!(err.contains("scripts/capture-host.sh"), "{err}");
    assert!(err.contains("--allow-missing-host-facts"), "{err}");
    assert_eq!(stdout(&o), "");
    assert!(!out.exists(), "nothing may be written");
}

#[cfg(target_os = "linux")]
#[test]
fn a_run_with_host_facts_reports_them_and_keeps_a_copy() {
    let facts = captured_facts("facts-ok");
    let out = tmp("withfacts");
    let o = tiny_run()
        .arg("--out")
        .arg(&out)
        .arg("--host-facts")
        .arg(&facts)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    assert!(
        text.contains(&format!("host facts: {}, captured ", facts.display())),
        "{text}"
    );
    assert!(text.contains("18 sections checked"), "{text}");
    assert!(text.contains("rtt-manual: absent"), "{text}");
    assert!(!text.contains("NOT CAPTURED"), "{text}");
    assert_eq!(
        std::fs::read(out.join("host-facts.txt")).unwrap(),
        std::fs::read(&facts).unwrap()
    );
}

/// Regression: `--host-facts DIR/host-facts.txt --out DIR` copied the file
/// onto itself, which truncated the evidence to 0 bytes and still exited 0.
#[cfg(target_os = "linux")]
#[test]
fn host_facts_already_in_the_out_directory_survive_the_run() {
    let out = tmp("samepath");
    let facts = out.join("host-facts.txt");
    std::fs::copy(captured_facts("facts-same"), &facts).unwrap();
    let before = std::fs::read(&facts).unwrap();
    assert!(!before.is_empty());
    let o = tiny_run()
        .arg("--host-facts")
        .arg(&facts)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(std::fs::read(&facts).unwrap(), before);
    let left: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("host-facts") && n != "host-facts.txt")
        .collect();
    assert_eq!(
        left,
        Vec::<String>::new(),
        "no temporary file is left behind"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_bad_host_facts_file_is_refused_before_anything_runs() {
    let good = std::fs::read_to_string(captured_facts("facts-src")).unwrap();
    let dir = tmp("facts-bad");
    let missing_section = {
        let start = good.find("== begin thp").unwrap();
        let end = good.find("== end thp\n").unwrap() + "== end thp\n".len();
        format!("{}{}", &good[..start], &good[end..])
    };
    let other_kernel = {
        let line = good.lines().find(|l| l.starts_with("kernel: ")).unwrap();
        good.replace(line, "kernel: 0.0.0-not-this-host")
    };
    for (name, text, want) in [
        ("missing.txt", missing_section.as_str(), "thp"),
        ("kernel.txt", other_kernel.as_str(), "0.0.0-not-this-host"),
        ("empty.txt", "", "hl-host-facts v1"),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        let out = dir.join(format!("out-{name}"));
        let o = tiny_run()
            .arg("--host-facts")
            .arg(&path)
            .arg("--out")
            .arg(&out)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2), "{name}: {}", stderr(&o));
        assert!(stderr(&o).contains(want), "{name}: {}", stderr(&o));
        assert_eq!(stdout(&o), "", "{name}");
        assert!(!out.exists(), "{name}: nothing may be written");
    }
    let o = tiny_run()
        .args(["--host-facts", "/nonexistent/hl-replay-facts.txt"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
}

#[cfg(target_os = "linux")]
#[test]
fn host_facts_and_the_override_together_are_a_usage_error() {
    let facts = captured_facts("facts-both");
    let o = tiny_run()
        .arg("--host-facts")
        .arg(&facts)
        .arg("--allow-missing-host-facts")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
}

#[test]
fn a_run_that_writes_nothing_needs_no_host_facts() {
    let o = tiny_run().output().unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("host facts: not captured (nothing written to disk)"),
        "{}",
        stdout(&o)
    );
}

#[test]
fn a_non_loopback_sink_is_refused_before_anything_runs() {
    for bad in ["8.8.8.8:443", "0.0.0.0:9000", "api.hyperliquid.xyz:443"] {
        let o = bin()
            .args(["--sink-bind", bad, "--lines", "10"])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2), "{bad}");
        assert!(stderr(&o).contains("loopback"), "{bad}: {}", stderr(&o));
        assert_eq!(stdout(&o), "");
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
        assert_ne!(stderr(&o), "");
    }
    let o = bin().arg("--help").output().unwrap();
    assert!(o.status.success());
    assert!(stdout(&o).contains("usage: hl-replay"));
}

#[test]
fn an_unavailable_arm_is_reported_not_fatal() {
    let o = bin()
        .args([
            "--arms",
            "compio,busy-poll",
            "--rounds",
            "1",
            "--lines",
            "100",
        ])
        .args(["--warmup", "10", "--interval-us", "30", "--no-pin"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let text = stdout(&o);
    let compio = text.lines().find(|l| l.starts_with("compio ")).unwrap();
    assert!(
        compio.contains("unavailable") || compio.contains("n=90"),
        "{compio}"
    );
    assert!(text.contains("n=90"));
}
