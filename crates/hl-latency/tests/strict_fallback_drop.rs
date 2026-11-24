//! Dropping a stamped scan-fallback line the strict pass failed.
//!
//! hl-capture stamps a line its header scan refuses when the strict parse
//! accepts it (`scan_fallbacks`). `hl-strict-pass` can still fail such a
//! line (no `events`, a repeated key) and lists it with `scan_accepted`
//! false and no height or block time. When the observations carry their
//! file and offset, the observation of that line goes, under the block
//! rules of the scan-accepted drop. Semantics decided with the Latency
//! Reason.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use hl_latency::{
    pair, Observation, Source, StrictDrop, StrictDropError, StrictFailures, StrictReadError,
};

const DATE: u32 = 20_250_727;
const HOUR: u8 = 8;
/// The hourly file `(DATE, HOUR)` as `hl-strict-pass` prints it.
const PATH: &str = "/data/node_fills/hourly/20250727/8";

/// A node observation of the line at `offset` in the file `(DATE, HOUR)`.
fn node(height: u64, time: u64, recv_ns: u64, offset: u64) -> Observation {
    Observation {
        source: Source::Node,
        block_time_ms: time,
        block_number: Some(height),
        recv_ns,
        file_date: Some(DATE),
        file_hour: Some(HOUR),
        line_offset: Some(offset),
    }
}
/// A node observation from a capture made before the fields existed.
fn old_node(height: u64, time: u64, recv_ns: u64) -> Observation {
    Observation {
        file_date: None,
        file_hour: None,
        line_offset: None,
        ..node(height, time, recv_ns, 0)
    }
}
fn ws(time: u64, recv_ns: u64) -> Observation {
    Observation {
        source: Source::PublicWs,
        block_time_ms: time,
        block_number: None,
        recv_ns,
        file_date: None,
        file_hour: None,
        line_offset: None,
    }
}

/// A `failure` line as `hl-strict-pass` writes it for a line the scan does
/// not accept: no height, no block time.
fn fallback_in(path: &str, offset: u64) -> String {
    format!(
        "{{\"failure\":{{\"path\":\"{path}\",\"line\":3,\"offset\":{offset},\"len\":20,\"reason\":\"no events\",\"scan_accepted\":false,\"block_number\":null,\"block_time_ms\":null}}}}\n"
    )
}
fn fallback(offset: u64) -> String {
    fallback_in(PATH, offset)
}
/// A `failure` line for a line the scan accepts.
fn torn(offset: u64, height: u64, time: u64) -> String {
    format!(
        "{{\"failure\":{{\"path\":\"{PATH}\",\"line\":3,\"offset\":{offset},\"len\":20,\"reason\":\"torn\",\"scan_accepted\":true,\"block_number\":{height},\"block_time_ms\":{time}}}}}\n"
    )
}
const TOTAL: &str = "{\"total\":{\"files\":1,\"records\":9,\"failed\":1,\"failures_omitted\":0}}\n";

fn read(lines: &[String]) -> Result<StrictFailures, StrictReadError> {
    StrictFailures::read([&lines.concat(), TOTAL].concat().as_bytes())
}
fn drop_from(
    lines: &[String],
    obs: &[Observation],
) -> Result<(Vec<Observation>, StrictDrop), StrictDropError> {
    read(lines).unwrap().drop_from(obs.to_vec())
}

// ---- Latency's five ----

#[test]
fn a_fallback_line_the_strict_pass_failed_is_dropped_and_counted() {
    // Block 10 came out as two lines; the second went through the fallback
    // and the strict pass failed it. Only its stamp goes.
    let obs = [
        node(10, 1_000, 5, 0),
        node(10, 1_000, 6, 100),
        ws(1_000, 50),
        node(11, 1_070, 100, 200),
        ws(1_070, 160),
    ];
    let (kept, report) = drop_from(&[fallback(100)], &obs).unwrap();
    assert_eq!(kept, [obs[0], obs[2], obs[3], obs[4]]);
    assert_eq!(
        report,
        StrictDrop {
            strict_pass_applied: true,
            strict_dropped_observations: 0,
            strict_dropped_blocks: 0,
            strict_dropped_ws_arrivals: 0,
            strict_failures_listed: 0,
            strict_failures_unmatched: 0,
            strict_failures_omitted: 0,
            strict_drop_incomplete: false,
            strict_drop_by_line: true,
            strict_shifted_first_arrival: 0,
            strict_fallback_failures_listed: 1,
            strict_fallback_failures_unmatched: 0,
            strict_dropped_fallback_lines: 1,
            strict_fallback_lines_unchecked: false,
        }
    );
    assert_eq!(pair(kept).pairs.len(), 2);
}

#[test]
fn a_fallback_line_the_strict_pass_passed_is_kept() {
    // The strict pass lists another line of the file, not the one at 100.
    let obs = [
        node(10, 1_000, 5, 0),
        node(10, 1_000, 6, 100),
        ws(1_000, 50),
    ];
    let (kept, report) = drop_from(&[fallback(0)], &obs).unwrap();
    assert_eq!(kept, [obs[1], obs[2]]);
    assert_eq!(report.strict_dropped_fallback_lines, 1);
}

#[test]
fn a_line_neither_parser_accepts_is_counted_apart_from_unmatched_torn_lines() {
    // Offset 40 is a line hl-capture never stamped (its `bad_lines`).
    let obs = [node(10, 1_000, 5, 0), ws(1_000, 50)];
    let (kept, report) = drop_from(&[fallback(40)], &obs).unwrap();
    assert_eq!(kept, obs);
    assert_eq!(report.strict_fallback_failures_listed, 1);
    assert_eq!(report.strict_fallback_failures_unmatched, 1);
    assert_eq!(report.strict_failures_unmatched, 0);
    assert_eq!(report.strict_dropped_fallback_lines, 0);
    assert!(report.strict_drop_by_line);
}

#[test]
fn a_scan_accepted_failure_with_no_observation_is_still_unmatched() {
    let obs = [node(10, 1_000, 5, 0), ws(1_000, 50)];
    let (kept, report) = drop_from(&[torn(40, 12, 1_140), fallback(60)], &obs).unwrap();
    assert_eq!(kept, obs);
    assert_eq!(report.strict_failures_listed, 1);
    assert_eq!(report.strict_failures_unmatched, 1);
    assert_eq!(report.strict_fallback_failures_listed, 1);
    assert_eq!(report.strict_fallback_failures_unmatched, 1);
}

#[test]
fn the_same_unstamped_line_listed_twice_is_one_unmatched_line() {
    let obs = [node(10, 1_000, 5, 0)];
    let (_, report) = drop_from(&[fallback(40), fallback(40)], &obs).unwrap();
    assert_eq!(report.strict_fallback_failures_listed, 2);
    assert_eq!(report.strict_fallback_failures_unmatched, 1);
}

// ---- the block rules of the scan-accepted drop ----

#[test]
fn a_block_whose_only_line_was_a_failed_fallback_is_neither_matched_nor_missed() {
    let obs = [
        node(10, 1_000, 5, 0),
        ws(1_000, 50),
        node(11, 1_070, 100, 100),
        ws(1_070, 160),
    ];
    let (kept, report) = drop_from(&[fallback(100)], &obs).unwrap();
    assert_eq!(kept, [obs[0], obs[1]]);
    assert_eq!(report.strict_dropped_fallback_lines, 1);
    assert_eq!(report.strict_dropped_observations, 0);
    assert_eq!(report.strict_dropped_blocks, 1);
    assert_eq!(report.strict_dropped_ws_arrivals, 1);
    let p = pair(kept);
    assert_eq!((p.pairs.len(), p.node_only, p.ws_only), (1, 0, 0));
}

#[test]
fn a_failed_fallback_first_arrival_is_counted_as_a_shifted_block() {
    let obs = [
        node(10, 1_000, 5, 0),
        node(10, 1_000, 9, 100),
        ws(1_000, 50),
    ];
    let (kept, report) = drop_from(&[fallback(0)], &obs).unwrap();
    assert_eq!(kept, [obs[1], obs[2]]);
    assert_eq!(report.strict_shifted_first_arrival, 1);
    assert_eq!(report.strict_dropped_blocks, 0);
}

#[test]
fn a_block_that_lost_a_torn_line_and_a_fallback_line_is_one_dropped_block() {
    let obs = [
        node(10, 1_000, 5, 0),
        node(10, 1_000, 9, 100),
        ws(1_000, 50),
    ];
    let (kept, report) = drop_from(&[torn(0, 10, 1_000), fallback(100)], &obs).unwrap();
    assert_eq!(kept, []);
    assert_eq!(report.strict_dropped_observations, 1);
    assert_eq!(report.strict_dropped_fallback_lines, 1);
    assert_eq!(report.strict_dropped_blocks, 1);
    assert_eq!(report.strict_dropped_ws_arrivals, 1);
    assert_eq!(report.strict_shifted_first_arrival, 0);
}

#[test]
fn a_line_listed_both_ways_is_dropped_once_as_scan_accepted() {
    // Not something one strict pass writes; the counts must still add up.
    let obs = [node(10, 1_000, 5, 0), node(10, 1_000, 9, 100)];
    let (kept, report) = drop_from(&[fallback(0), torn(0, 10, 1_000)], &obs).unwrap();
    assert_eq!(kept, [obs[1]]);
    assert_eq!(report.strict_dropped_observations, 1);
    assert_eq!(report.strict_dropped_fallback_lines, 0);
}

// ---- refusals and old captures ----

#[test]
fn an_old_capture_keeps_fallback_lines_and_says_they_were_not_checked() {
    let obs = [
        old_node(10, 1_000, 5),
        old_node(11, 1_070, 100),
        ws(1_000, 50),
    ];
    let (kept, report) = drop_from(&[fallback(0)], &obs).unwrap();
    assert_eq!(kept, obs);
    assert!(!report.strict_drop_by_line);
    assert_eq!(report.strict_fallback_failures_listed, 1);
    assert_eq!(report.strict_dropped_fallback_lines, 0);
    assert_eq!(report.strict_fallback_failures_unmatched, 0);
    assert!(report.strict_fallback_lines_unchecked);
}

#[test]
fn nothing_is_unchecked_when_no_fallback_failure_is_listed() {
    let old = [old_node(10, 1_000, 5)];
    assert!(
        !drop_from(&[torn(0, 10, 1_000)], &old)
            .unwrap()
            .1
            .strict_fallback_lines_unchecked
    );
    // By line they are checked, so the note stays off.
    let new = [node(10, 1_000, 5, 0)];
    assert!(
        !drop_from(&[fallback(0)], &new)
            .unwrap()
            .1
            .strict_fallback_lines_unchecked
    );
}

#[test]
fn a_fallback_failure_whose_path_gives_no_date_and_hour_is_refused_by_line() {
    let obs = [node(10, 1_000, 5, 0)];
    let e = drop_from(&[fallback_in("f", 0)], &obs).unwrap_err();
    assert_eq!(
        e,
        StrictDropError::FailureWithoutFile {
            path: "f".to_owned()
        }
    );
    // By block the line is not looked for, so its path does not matter.
    assert!(drop_from(&[fallback_in("f", 0)], &[old_node(10, 1_000, 5)]).is_ok());
}

#[test]
fn a_fallback_failure_in_a_second_root_is_refused() {
    let obs = [node(10, 1_000, 5, 0)];
    let e = drop_from(
        &[fallback(0), fallback_in("/other/hourly/20250727/8", 40)],
        &obs,
    )
    .unwrap_err();
    assert!(matches!(e, StrictDropError::TwoPathsOneFile { .. }), "{e}");
}

#[test]
fn a_failure_without_its_path_or_offset_is_malformed() {
    for line in [
        "{\"failure\":{\"offset\":0,\"scan_accepted\":false,\"block_number\":null,\"block_time_ms\":null}}\n",
        "{\"failure\":{\"path\":\"/d/20250727/8\",\"scan_accepted\":false,\"block_number\":null,\"block_time_ms\":null}}\n",
    ] {
        let e = read(&[line.to_owned()]).unwrap_err();
        assert!(
            matches!(&e, StrictReadError::Bad { line: 1, msg } if msg.contains("path and offset")),
            "{e}"
        );
    }
}

// ---- the binary ----

fn tmp(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hl-latency-fallback-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}
fn run(name: &str, obs: &[Observation], lines: &[String]) -> Output {
    let body: String = obs
        .iter()
        .map(|o| serde_json::to_string(o).unwrap() + "\n")
        .collect();
    Command::new(env!("CARGO_BIN_EXE_hl-latency"))
        .arg(tmp(&format!("{name}.jsonl"), &body))
        .arg("--strict-pass")
        .arg(tmp(
            &format!("{name}.strict.jsonl"),
            &[&lines.concat(), TOTAL].concat(),
        ))
        .output()
        .unwrap()
}

#[test]
fn the_flag_drops_a_failed_fallback_line_and_reports_the_counts() {
    let obs = [
        node(10, 1_000, 5, 0),
        node(10, 1_000, 9, 100),
        ws(1_000, 50),
        node(11, 1_070, 100, 200),
        ws(1_070, 160),
    ];
    let o = run(
        "ok",
        &obs,
        &[fallback(0), fallback(999), torn(200, 11, 1_070)],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(s["matched"], 1);
    assert_eq!(s["ws_only"], 0);
    assert_eq!(s["max_ns"], 41, "block 10 pairs on its later line: 50 - 9");
    assert_eq!(s["strict_drop_by_line"], true);
    assert_eq!(s["strict_dropped_observations"], 1);
    assert_eq!(s["strict_dropped_fallback_lines"], 1);
    assert_eq!(s["strict_shifted_first_arrival"], 1);
    assert_eq!(s["strict_failures_unmatched"], 0);
    assert_eq!(s["strict_fallback_failures_listed"], 2);
    assert_eq!(s["strict_fallback_failures_unmatched"], 1);
    assert_eq!(s["strict_fallback_lines_unchecked"], false);
}

#[test]
fn an_old_capture_with_fallback_failures_says_so_in_the_summary_and_on_stderr() {
    let obs = [old_node(10, 1_000, 5), ws(1_000, 50)];
    let o = run("old", &obs, &[fallback(0)]);
    assert_eq!(o.status.code(), Some(0));
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(s["matched"], 1);
    assert_eq!(s["strict_fallback_lines_unchecked"], true);
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(e.contains("line_offset") && e.contains('1'), "{e}");
}
