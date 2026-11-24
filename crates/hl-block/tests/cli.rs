//! `hl-block census` CLI contract.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hl-block"))
}

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/three_blocks.jsonl");

#[test]
fn census_text_on_file() {
    let o = bin().args(["census", FIXTURE]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let s = String::from_utf8(o.stdout).unwrap();
    assert!(
        s.starts_with("blocks 3 (empty 1), bundles 2, actions 3"),
        "{s}"
    );
}

#[test]
fn census_json_on_directory_tree_counts_every_file() {
    let d = std::env::temp_dir().join(format!("hlb-cli-{}", std::process::id()));
    let sub = d.join("20250908");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::copy(FIXTURE, sub.join("992814000")).unwrap();
    std::fs::copy(FIXTURE, sub.join("992815000")).unwrap();
    let o = bin()
        .args(["census", "--json", d.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_dir_all(&d).unwrap();
    assert!(o.status.success());
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["actions"], 6);
    assert_eq!(v["blocks"], 6);
}

#[test]
fn decode_errors_are_counted_and_exit_1() {
    let d = std::env::temp_dir().join(format!("hlb-bad-{}.jsonl", std::process::id()));
    std::fs::write(
        &d,
        format!("{}not json\n", std::fs::read_to_string(FIXTURE).unwrap()),
    )
    .unwrap();
    let o = bin()
        .args(["census", "--json", d.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&d).unwrap();
    assert_eq!(o.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["blocks"], 3);
    assert_eq!(
        v["decode_errors"]
            .as_object()
            .unwrap()
            .values()
            .map(|x| x.as_u64().unwrap())
            .sum::<u64>(),
        1
    );
}

#[test]
fn stdin_dash() {
    use std::io::Write;
    let mut c = bin()
        .args(["census", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin
        .take()
        .unwrap()
        .write_all(std::fs::read(FIXTURE).unwrap().as_slice())
        .unwrap();
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success());
    assert!(String::from_utf8(o.stdout).unwrap().contains("actions 3"));
}

#[test]
fn usage_exit_2() {
    assert_eq!(bin().output().unwrap().status.code(), Some(2));
    assert_eq!(
        bin().args(["census"]).output().unwrap().status.code(),
        Some(2)
    );
    assert_eq!(
        bin().args(["bogus", "x"]).output().unwrap().status.code(),
        Some(2)
    );
}
