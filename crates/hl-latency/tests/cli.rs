//! Contract tests for the `hl-latency` binary: exit codes, stdout and stderr.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hl-latency"))
}

fn tmp(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hl-latency-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn no_args_is_usage_exit_2() {
    let o = bin().output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("usage: hl-latency"));
    assert_eq!(o.stdout.len(), 0);
}

#[test]
fn two_args_is_usage_exit_2() {
    let o = bin().args(["a", "b"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("usage: hl-latency"));
}

#[test]
fn missing_file_exit_2_names_path() {
    let o = bin()
        .arg("/nonexistent/hl-latency-obs.jsonl")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("/nonexistent/hl-latency-obs.jsonl"));
}

#[test]
fn bad_line_exit_2_names_line_number() {
    let p = tmp(
        "bad.jsonl",
        "{\"source\":\"node\",\"block_time_ms\":1,\"recv_ns\":1}\nnot json\n",
    );
    let o = bin().arg(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("line 2"), "{}", stderr(&o));
}

#[cfg(unix)]
#[test]
fn non_utf8_path_is_an_error_not_a_panic() {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::OsStr::from_bytes(b"/nonexistent/obs-\xff.jsonl");
    let o = bin().arg(name).output().unwrap();
    assert_eq!(o.status.code(), Some(2), "stderr: {}", stderr(&o));
    assert!(!stderr(&o).contains("panicked"));
}

#[test]
fn happy_path_prints_summary_json_exit_0() {
    let body = [
        r#"{"source":"node","block_time_ms":100,"block_number":10,"recv_ns":1000}"#,
        r#"{"source":"public_ws","block_time_ms":100,"recv_ns":1500}"#,
        r#"{"source":"node","block_time_ms":170,"block_number":11,"recv_ns":2000}"#,
        "",
    ]
    .join("\n");
    let p = tmp("ok.jsonl", &body);
    let o = bin().arg(&p).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["matched"], 1);
    assert_eq!(v["node_only"], 1);
    assert_eq!(v["ws_only"], 0);
    assert_eq!(v["ambiguous"], 0);
    assert_eq!(v["p50_ns"], 500);
}
