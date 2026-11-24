//! Contract of the `hl-strict-pass` binary: input order, report lines and
//! exit codes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hl-strict-pass"))
}

fn line(height: u64) -> String {
    format!(
        "{{\"local_time\":\"2025-07-27T08:50:10.334741319\",\"block_time\":\"2025-07-27T08:{:02}:{:02}.000000\",\"block_number\":{height},\"events\":[{{\"a\":{{\"b\":1}}}}]}}\n",
        (height / 60) % 60,
        height % 60
    )
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// stdout as JSON lines, each `{"kind": {...}}`.
fn lines(out: &Output, kind: &str) -> Vec<Value> {
    String::from_utf8(out.stdout.clone())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).expect("stdout is JSON lines"))
        .filter_map(|v| v.get(kind).cloned())
        .collect()
}

fn total(out: &Output) -> Value {
    let mut t = lines(out, "total");
    assert_eq!(t.len(), 1, "one total line");
    t.remove(0)
}

#[test]
fn clean_directory_is_read_in_date_and_hour_order_and_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    // Numeric order: hour 9 is before hour 10, and the next day follows.
    write(dir.path(), "hourly/20250727/9", &(line(1) + &line(2)));
    write(dir.path(), "hourly/20250727/10", &(line(3) + &line(4)));
    write(dir.path(), "hourly/20250728/0", &line(5));
    write(
        dir.path(),
        "hourly/20250728/notes.txt",
        "not an hourly file",
    );
    let out = bin().arg(dir.path()).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let files = lines(&out, "file");
    let ends: Vec<_> = files
        .iter()
        .map(|f| {
            let p = f["path"].as_str().unwrap();
            assert!(p.starts_with(dir.path().to_str().unwrap()), "{p}");
            (
                p.rsplit('/').next().unwrap().to_owned(),
                f["first_block_number"].as_u64().unwrap(),
                f["last_block_number"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ends,
        [
            ("9".to_owned(), 1, 2),
            ("10".to_owned(), 3, 4),
            ("0".to_owned(), 5, 5)
        ]
    );
    assert_eq!(files[0]["records"], 2);
    let t = total(&out);
    assert_eq!(t["files"], 3);
    assert_eq!(t["records"], 5);
    for zero in [
        "failed",
        "scan_accepted_failed",
        "unterminated_tail",
        "height_breaks",
        "height_repeats",
        "height_gaps",
        "height_backwards",
        "block_time_backwards",
        "duplicate_block_numbers",
        "failures_omitted",
    ] {
        assert_eq!(t[zero], 0, "{zero}");
    }
    assert_eq!(lines(&out, "failure"), Vec::<Value>::new());
}

#[test]
fn torn_line_exits_1_and_is_listed_with_its_capture_key() {
    let dir = tempfile::tempdir().unwrap();
    let whole = line(2);
    let torn = format!("{}\n", whole.trim_end().strip_suffix("}]}").unwrap());
    assert!(torn.trim_end().ends_with('}'));
    write(dir.path(), "20250727/9", &(line(1) + &torn + &line(3)));
    let file = dir.path().join("20250727/9");
    let out = bin().arg(&file).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let failures = lines(&out, "failure");
    assert_eq!(failures.len(), 1);
    let f = &failures[0];
    assert_eq!(f["path"], file.to_str().unwrap());
    assert_eq!(f["line"], 2);
    assert_eq!(f["offset"], line(1).len());
    assert_eq!(f["len"], torn.len() - 1);
    assert_eq!(f["scan_accepted"], true);
    assert_eq!(f["block_number"], 2);
    assert!(f["block_time_ms"].is_u64());
    assert!(f["reason"].is_string());
    let t = total(&out);
    assert_eq!(
        (t["records"].clone(), t["failed"].clone()),
        (3.into(), 1.into())
    );
    assert_eq!(t["scan_accepted_failed"], 1);
}

#[test]
fn height_gap_is_reported_and_fails_only_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "20250727/9", &(line(1) + &line(1) + &line(5)));
    let out = bin().arg(dir.path()).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let t = total(&out);
    assert_eq!(t["height_breaks"], 2);
    assert_eq!(
        (t["height_repeats"].clone(), t["height_gaps"].clone()),
        (1.into(), 1.into())
    );
    let out = bin()
        .arg("--fail-on-height-gap")
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(total(&out)["height_gaps"], 1);

    // A repeat alone never fails: one block can span several lines.
    let repeat = tempfile::tempdir().unwrap();
    write(
        repeat.path(),
        "20250727/9",
        &(line(1) + &line(1) + &line(2)),
    );
    let out = bin()
        .arg("--fail-on-height-gap")
        .arg(repeat.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");

    // Going back fails too.
    let back = tempfile::tempdir().unwrap();
    write(back.path(), "20250727/9", &(line(2) + &line(1)));
    let out = bin()
        .arg("--fail-on-height-gap")
        .arg(back.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn unterminated_tail_is_allowed_only_for_the_newest_file() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "20250727/9", &line(1));
    write(dir.path(), "20250727/10", &(line(2) + line(3).trim_end()));
    let out = bin().arg(dir.path()).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(lines(&out, "failure")[0]["reason"], "unterminated");
    let out = bin()
        .arg("--allow-unterminated-tail")
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let t = total(&out);
    assert_eq!(
        (t["records"].clone(), t["unterminated_tail"].clone()),
        (2.into(), 1.into())
    );

    // An older file with an open tail is damaged, flag or not.
    write(dir.path(), "20250727/9", line(1).trim_end());
    let out = bin()
        .arg("--allow-unterminated-tail")
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
}

#[test]
fn max_failures_caps_the_list_not_the_counts() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "20250727/9", "{\n{\n{\n");
    let out = bin()
        .args(["--max-failures", "1"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(lines(&out, "failure").len(), 1);
    let t = total(&out);
    assert_eq!(
        (t["failed"].clone(), t["failures_omitted"].clone()),
        (3.into(), 2.into())
    );
}

#[test]
fn max_line_bytes_makes_a_longer_line_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "20250727/9", &line(1));
    let out = bin()
        .args(["--max-line-bytes", "10"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(lines(&out, "failure")[0]["reason"], "oversized");
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        &[][..],
        &["--max-failures"],
        &["--max-failures", "x", "p"],
        &["--max-line-bytes", "0", "p"],
        &["--no-such-flag", "p"],
        &["--allow-unterminated-tail"],
    ] {
        let out = bin().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("usage:"),
            "{args:?}"
        );
        assert_eq!(out.stdout, b"");
    }
}

#[test]
fn unreadable_input_and_nothing_to_check_exit_3() {
    let dir = tempfile::tempdir().unwrap();
    let out = bin().arg(dir.path().join("missing")).output().unwrap();
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    // A directory with no hourly file: a pass that checked nothing must not
    // look like a pass.
    write(dir.path(), "20250727/readme", "x");
    let out = bin().arg(dir.path()).output().unwrap();
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert_ne!(out.stderr, b"");
}

#[test]
fn a_directory_with_no_hourly_file_exits_3_even_beside_one_that_has_some() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "good/20250727/9", &line(1));
    write(dir.path(), "mistyped/20250727/readme", "x");
    fs::create_dir_all(dir.path().join("empty")).unwrap();
    let good = dir.path().join("good");
    for bad in ["empty", "mistyped"] {
        let bad = dir.path().join(bad);
        // Either order: the other path's files do not hide the empty one.
        for paths in [[&good, &bad], [&bad, &good]] {
            let out = bin().args(paths).output().unwrap();
            assert_eq!(out.status.code(), Some(3), "{out:?}");
            // Nothing is reported for a run that was not what was asked for.
            assert_eq!(out.stdout, b"", "{out:?}");
            let err = String::from_utf8(out.stderr).unwrap();
            assert!(err.contains(bad.to_str().unwrap()), "names the path: {err}");
        }
    }
    // The same directory with only paths that have files still passes.
    let out = bin().arg(&good).arg(&good).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
}
