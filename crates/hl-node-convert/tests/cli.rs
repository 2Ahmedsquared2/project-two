//! The `hl-node-convert` binary on the golden scenario: output, stats and exit codes.

use std::fs;

use std::path::PathBuf;
use std::process::{Command, Output};

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../golden/node")
        .join(name)
}

fn disposable(name: &str) -> PathBuf {
    let p = out_path(&format!("safety-{name}-{}", std::process::id()));
    if p.exists() {
        fs::remove_dir_all(&p).unwrap_or_else(|e| panic!("{e}"));
    }
    fs::create_dir_all(&p).unwrap_or_else(|e| panic!("{e}"));
    p
}

fn input_aliases_are_refused(kind: &str) {
    let d = disposable(kind);
    let inputs = [
        ("--meta", "meta.json"),
        ("--diffs", "node_raw_book_diffs"),
        ("--statuses", "node_order_statuses"),
        ("--seed", "seed.json"),
        ("--check", "check.json"),
    ];
    for (_, name) in inputs {
        fs::copy(golden(name), d.join(name)).unwrap_or_else(|e| panic!("{e}"));
    }
    for (_, name) in inputs {
        let input = d.join(name);
        let original = fs::read(&input).unwrap_or_else(|e| panic!("{e}"));
        let out = if kind == "exact" {
            input.clone()
        } else {
            d.join("alias")
        };
        if kind == "hardlink" {
            fs::hard_link(&input, &out).unwrap_or_else(|e| panic!("{e}"));
        }
        #[cfg(unix)]
        if kind == "symlink" {
            std::os::unix::fs::symlink(&input, &out).unwrap_or_else(|e| panic!("{e}"));
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hl-node-convert"));
        for (flag, name) in inputs {
            cmd.arg(flag).arg(d.join(name));
        }
        let result = cmd
            .arg("--out")
            .arg(&out)
            .output()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            !result.status.success(),
            "{kind}: {name} accepted as output"
        );
        assert_eq!(
            fs::read(&input).unwrap_or_else(|e| panic!("{e}")),
            original,
            "{kind}: {name} changed"
        );
        if kind != "exact" {
            fs::remove_file(&out).unwrap_or_else(|e| panic!("{e}"));
        }
    }
    fs::remove_dir_all(d).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn safety_exact_input_paths_are_never_overwritten() {
    input_aliases_are_refused("exact");
}

#[test]
fn safety_hardlink_input_aliases_are_never_overwritten() {
    input_aliases_are_refused("hardlink");
}

#[cfg(unix)]
#[test]
fn safety_symlink_input_aliases_are_never_overwritten() {
    input_aliases_are_refused("symlink");
}

#[test]
fn safety_failed_conversion_preserves_existing_output_and_cleans_temporary_file() {
    let d = disposable("failed-output");
    let out = d.join("out");
    let sentinel = b"previous complete dataset";
    fs::write(&out, sentinel).unwrap_or_else(|e| panic!("{e}"));
    let result = convert(&["--strict"], &out);
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(fs::read(&out).unwrap_or_else(|e| panic!("{e}")), sentinel);
    let diffs = d.join("malformed-diffs");
    fs::write(&diffs, "garbage\n").unwrap_or_else(|e| panic!("{e}"));
    let result = convert(
        &[
            "--diffs",
            diffs.to_str().unwrap_or_else(|| panic!("UTF-8 path")),
        ],
        &out,
    );
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(fs::read(&out).unwrap_or_else(|e| panic!("{e}")), sentinel);
    assert_eq!(
        fs::read_dir(&d).unwrap_or_else(|e| panic!("{e}")).count(),
        2
    );
    fs::remove_dir_all(d).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn safety_success_still_replaces_unrelated_existing_output() {
    let d = disposable("overwrite-output");
    let out = d.join("out");
    fs::write(&out, "old output").unwrap_or_else(|e| panic!("{e}"));
    let result = convert(&[], &out);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let actual = fs::read_to_string(&out).unwrap_or_else(|e| panic!("{e}"));
    let expected = fs::read_to_string(golden("expected.jsonl")).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(steps(&actual), steps(&expected));
    assert_eq!(
        fs::read_dir(&d).unwrap_or_else(|e| panic!("{e}")).count(),
        1
    );
    fs::remove_dir_all(d).unwrap_or_else(|e| panic!("{e}"));
}

fn out_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn convert(extra: &[&str], out: &PathBuf) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hl-node-convert"));
    cmd.arg("--meta")
        .arg(golden("meta.json"))
        .arg("--diffs")
        .arg(golden("node_raw_book_diffs"))
        .arg("--statuses")
        .arg(golden("node_order_statuses"))
        .arg("--seed")
        .arg(golden("seed.json"))
        .arg("--check")
        .arg(golden("check.json"))
        .args(["--coin", "BTC", "--coin", "PURR/USDC"])
        .arg("--source")
        .arg("hl-node node_raw_book_diffs + node_order_statuses (--stream-with-block-info), seeded from l4Snapshots")
        .args(extra)
        .arg("--out")
        .arg(out);
    cmd.output().unwrap_or_else(|e| panic!("{e}"))
}

fn steps(text: &str) -> (hl_dataset::Header, Vec<hl_dataset::Step>) {
    let r = hl_dataset::Reader::new(text.as_bytes()).unwrap_or_else(|e| panic!("{e}"));
    let h = r.header().clone();
    let s = r
        .steps()
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|e| panic!("{e}"));
    (h, s)
}

#[test]
fn converts_the_golden_scenario_and_prints_stats() {
    let out = out_path("golden-node.jsonl");
    let o = convert(&[], &out);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let written = std::fs::read_to_string(&out).unwrap_or_else(|e| panic!("{e}"));
    let expected =
        std::fs::read_to_string(golden("expected.jsonl")).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(steps(&written), steps(&expected));
    let stats: serde_json::Value =
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(stats["blocks"], 4);
    assert_eq!(stats["check_books"], 2);
    assert_eq!(stats["unknown_coin_diffs"], 1);
}

#[test]
fn strict_finding_exits_1() {
    let o = convert(&["--strict"], &out_path("golden-node-strict.jsonl"));
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn usage_errors_exit_2() {
    let o = Command::new(env!("CARGO_BIN_EXE_hl-node-convert"))
        .arg("--bogus")
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(o.status.code(), Some(2));
    let o = Command::new(env!("CARGO_BIN_EXE_hl-node-convert"))
        .args([
            "--meta",
            "/nonexistent/meta.json",
            "--diffs",
            "x",
            "--out",
            "y",
        ])
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn several_diff_files_are_read_in_order() {
    // Split the golden diff file in two at a line boundary, without a trailing
    // newline on the first part: the files must still be read as separate lines.
    let text =
        std::fs::read_to_string(golden("node_raw_book_diffs")).unwrap_or_else(|e| panic!("{e}"));
    let lines: Vec<&str> = text.lines().collect();
    let (a, b) = lines.split_at(2);
    let pa = out_path("diffs-part-a");
    let pb = out_path("diffs-part-b");
    std::fs::write(&pa, a.join("\n")).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(&pb, b.join("\n") + "\n").unwrap_or_else(|e| panic!("{e}"));
    let out = out_path("golden-node-split.jsonl");
    let o = Command::new(env!("CARGO_BIN_EXE_hl-node-convert"))
        .arg("--meta")
        .arg(golden("meta.json"))
        .arg("--diffs")
        .arg(&pa)
        .arg("--diffs")
        .arg(&pb)
        .arg("--statuses")
        .arg(golden("node_order_statuses"))
        .arg("--seed")
        .arg(golden("seed.json"))
        .arg("--check")
        .arg(golden("check.json"))
        .args(["--coin", "BTC", "--coin", "PURR/USDC"])
        .arg("--source")
        .arg("hl-node node_raw_book_diffs + node_order_statuses (--stream-with-block-info), seeded from l4Snapshots")
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let written = std::fs::read_to_string(&out).unwrap_or_else(|e| panic!("{e}"));
    let expected =
        std::fs::read_to_string(golden("expected.jsonl")).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(steps(&written), steps(&expected));
}
