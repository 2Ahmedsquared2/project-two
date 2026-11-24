//! Dropping the blocks `hl-strict-pass` lists as scan-accepted failures
//! before pairing: the library step and the `--strict-pass` flag.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use hl_latency::{
    pair, summarize, Observation, Source, StrictDrop, StrictFailures, StrictReadError,
};
use proptest::prelude::*;

fn node(height: u64, time: u64, recv_ns: u64) -> Observation {
    Observation {
        source: Source::Node,
        block_time_ms: time,
        block_number: Some(height),
        recv_ns,
        file_date: None,
        file_hour: None,
        line_offset: None,
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

fn n(len: usize) -> u64 {
    u64::try_from(len).unwrap()
}

/// A `failure` line as `hl-strict-pass` writes it for a scan-accepted line.
fn failure(height: u64, time: u64) -> String {
    format!(
        "{{\"failure\":{{\"path\":\"f\",\"line\":3,\"offset\":10,\"len\":20,\"reason\":\"torn\",\"scan_accepted\":true,\"block_number\":{height},\"block_time_ms\":{time}}}}}\n"
    )
}
const UNSCANNED: &str = "{\"failure\":{\"path\":\"f\",\"line\":4,\"offset\":40,\"len\":2,\"reason\":\"not json\",\"scan_accepted\":false,\"block_number\":null,\"block_time_ms\":null}}\n";
const FILE: &str =
    "{\"file\":{\"path\":\"f\",\"records\":9,\"failed\":1,\"first_block_number\":1}}\n";
fn total(omitted: u64) -> String {
    format!(
        "{{\"total\":{{\"files\":1,\"records\":9,\"failed\":1,\"failures_omitted\":{omitted}}}}}\n"
    )
}
fn read(text: &str) -> Result<StrictFailures, StrictReadError> {
    StrictFailures::read(text.as_bytes())
}

#[test]
fn a_block_written_as_several_lines_is_dropped_whole() {
    // Block 10 came out as three lines; one was torn but scan-accepted. The
    // observations carry no offset, so all three stamps and the WS arrival go.
    let obs = vec![
        node(10, 1_000, 5),
        node(10, 1_000, 6),
        ws(1_000, 50),
        node(10, 1_000, 7),
        node(11, 1_070, 100),
        ws(1_070, 160),
    ];
    let drops = read(&[failure(10, 1_000), FILE.into(), total(0)].concat()).unwrap();
    let (kept, report) = drops.apply(obs);
    assert_eq!(kept, vec![node(11, 1_070, 100), ws(1_070, 160)]);
    assert_eq!(
        report,
        StrictDrop {
            strict_pass_applied: true,
            strict_dropped_observations: 3,
            strict_dropped_blocks: 1,
            strict_dropped_ws_arrivals: 1,
            strict_failures_listed: 1,
            strict_failures_unmatched: 0,
            strict_failures_omitted: 0,
            strict_drop_incomplete: false,
            strict_drop_by_line: false,
            strict_shifted_first_arrival: 0,
            ..StrictDrop::default()
        }
    );
    let s = summarize(&pair(kept));
    assert_eq!((s.matched, s.node_only, s.ws_only), (1, 0, 0));
    assert_eq!(s.max_ns, Some(60));
}

#[test]
fn a_listed_block_nobody_observed_drops_nothing_and_is_counted_unmatched() {
    let obs = vec![node(10, 1_000, 5), ws(1_000, 9), ws(5_000, 11)];
    let drops = read(&[failure(99, 5_000), total(0)].concat()).unwrap();
    let (kept, report) = drops.apply(obs.clone());
    assert_eq!(
        kept, obs,
        "the WS arrival at 5000 stays: no block was dropped"
    );
    assert_eq!(report.strict_failures_listed, 1);
    assert_eq!(report.strict_failures_unmatched, 1);
    assert_eq!(report.strict_dropped_blocks, 0);
    assert_eq!(report.strict_dropped_observations, 0);
    assert_eq!(report.strict_dropped_ws_arrivals, 0);
}

#[test]
fn the_key_is_height_and_block_time_together() {
    // Same block time, another height: not the listed block.
    let obs = vec![node(10, 1_000, 5), node(12, 2_000, 6)];
    let drops = read(&[failure(12, 1_000), failure(10, 2_000), total(0)].concat()).unwrap();
    let (kept, report) = drops.apply(obs.clone());
    assert_eq!(kept, obs);
    assert_eq!(report.strict_failures_unmatched, 2);
}

#[test]
fn the_same_block_listed_twice_is_one_dropped_block() {
    let drops = read(&[failure(10, 1_000), failure(10, 1_000), total(0)].concat()).unwrap();
    let (kept, report) = drops.apply(vec![node(10, 1_000, 5)]);
    assert_eq!(kept, Vec::new());
    assert_eq!(report.strict_failures_listed, 2);
    assert_eq!(report.strict_dropped_blocks, 1);
    assert_eq!(report.strict_dropped_observations, 1);
}

#[test]
fn a_failure_the_scan_did_not_accept_is_ignored() {
    let drops = read(&[UNSCANNED, &total(0)].concat()).unwrap();
    let obs = vec![node(10, 1_000, 5)];
    let (kept, report) = drops.apply(obs.clone());
    assert_eq!(kept, obs);
    assert_eq!(report.strict_failures_listed, 0);
    assert!(report.strict_pass_applied);
}

#[test]
fn a_capped_failure_list_marks_the_drop_incomplete() {
    let drops = read(&[failure(10, 1_000), total(7)].concat()).unwrap();
    let (_, report) = drops.apply(vec![node(10, 1_000, 5)]);
    assert_eq!(report.strict_failures_omitted, 7);
    assert!(report.strict_drop_incomplete);
}

#[test]
fn output_without_a_total_line_is_refused_as_incomplete() {
    for text in [
        String::new(),
        failure(10, 1_000),
        [&failure(10, 1_000), FILE].concat(),
    ] {
        assert!(
            matches!(read(&text), Err(StrictReadError::NoTotal)),
            "{text:?}"
        );
    }
}

#[test]
fn malformed_output_is_refused_with_its_line_number() {
    let cases = [
        // Not JSON.
        (["not json\n", &total(0)].concat(), 1),
        // Not a strict-pass line.
        ([FILE, "{\"other\":{}}\n", &total(0)].concat(), 2),
        // Scan-accepted, but no key to drop by.
        (
            [
                "{\"failure\":{\"scan_accepted\":true,\"block_number\":null,\"block_time_ms\":5}}\n",
                &total(0),
            ]
            .concat(),
            1,
        ),
        // Two runs joined, or a line after the end.
        ([total(0), failure(10, 1_000)].concat(), 2),
        ([total(0), total(0)].concat(), 2),
        // A total that does not say how many failures were left out.
        ("{\"total\":{\"files\":1}}\n".to_owned(), 1),
    ];
    for (text, want) in cases {
        match read(&text) {
            Err(StrictReadError::Bad { line, .. }) => assert_eq!(line, want, "{text:?}"),
            other => panic!("{text:?}: {other:?}"),
        }
    }
}

proptest! {
    /// Whatever is observed and listed: no listed block survives, nothing
    /// else from the node is lost, the rest keeps its order and the counters
    /// account for every observation.
    #[test]
    fn apply_removes_exactly_the_listed_blocks(
        raw in proptest::collection::vec((any::<bool>(), 0u64..6, 0u64..6, any::<u32>()), 0..60),
        listed in proptest::collection::vec((0u64..6, 0u64..6), 0..8),
    ) {
        let obs: Vec<Observation> = raw
            .iter()
            .map(|&(is_node, h, t, r)| if is_node { node(h, t, r.into()) } else { ws(t, r.into()) })
            .collect();
        let text: String = listed.iter().map(|&(h, t)| failure(h, t)).chain([total(0)]).collect();
        let (kept, report) = read(&text).unwrap().apply(obs.clone());

        let is_listed = |o: &Observation| {
            o.source == Source::Node && listed.contains(&(o.block_number.unwrap(), o.block_time_ms))
        };
        let dropped_times: Vec<u64> =
            obs.iter().filter(|o| is_listed(o)).map(|o| o.block_time_ms).collect();
        let want: Vec<Observation> = obs
            .iter()
            .filter(|o| match o.source {
                Source::Node => !is_listed(o),
                Source::PublicWs => !dropped_times.contains(&o.block_time_ms),
            })
            .copied()
            .collect();
        prop_assert_eq!(&kept, &want);
        prop_assert_eq!(
            report.strict_dropped_observations + report.strict_dropped_ws_arrivals,
            n(obs.len() - kept.len())
        );
        let mut distinct = listed.clone();
        distinct.sort_unstable();
        distinct.dedup();
        prop_assert_eq!(
            report.strict_dropped_blocks + report.strict_failures_unmatched,
            n(distinct.len())
        );
        prop_assert_eq!(report.strict_failures_listed, n(listed.len()));
    }
}

// ---- the binary ----

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hl-latency"))
}
fn tmp(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hl-latency-strict-drop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn summary(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap()
}
/// Block 10 (three node lines, one WS) and block 11 (one each).
fn observations(name: &str) -> PathBuf {
    let obs = [
        node(10, 1_000, 5),
        node(10, 1_000, 6),
        node(10, 1_000, 7),
        ws(1_000, 50),
        node(11, 1_070, 100),
        ws(1_070, 160),
    ];
    let body: String = obs
        .iter()
        .map(|o| serde_json::to_string(o).unwrap() + "\n")
        .collect();
    tmp(name, &body)
}

#[test]
fn without_the_flag_the_summary_says_no_strict_pass_was_applied() {
    let o = bin().arg(observations("plain.jsonl")).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let s = summary(&o);
    assert_eq!(s["matched"], 2);
    assert_eq!(s["strict_pass_applied"], false);
    assert_eq!(s["strict_dropped_observations"], 0);
    assert_eq!(s["strict_drop_incomplete"], false);
}

#[test]
fn the_flag_drops_the_listed_block_and_reports_the_counts() {
    let strict = tmp(
        "ok.strict.jsonl",
        &[failure(10, 1_000), FILE.into(), total(0)].concat(),
    );
    for args in [
        vec![
            observations("a.jsonl"),
            "--strict-pass".into(),
            strict.clone(),
        ],
        vec![
            "--strict-pass".into(),
            strict.clone(),
            observations("b.jsonl"),
        ],
    ] {
        let o = bin().args(&args).output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
        let s = summary(&o);
        assert_eq!(s["matched"], 1);
        assert_eq!(s["max_ns"], 60);
        assert_eq!(s["strict_pass_applied"], true);
        assert_eq!(s["strict_dropped_observations"], 3);
        assert_eq!(s["strict_dropped_blocks"], 1);
        assert_eq!(s["strict_dropped_ws_arrivals"], 1);
        assert_eq!(s["strict_failures_listed"], 1);
        assert_eq!(s["strict_failures_unmatched"], 0);
        assert_eq!(s["strict_failures_omitted"], 0);
        assert_eq!(s["strict_drop_incomplete"], false);
    }
}

#[test]
fn a_capped_failure_list_exits_1_with_the_summary_and_a_warning() {
    let strict = tmp(
        "capped.strict.jsonl",
        &[failure(10, 1_000), total(4)].concat(),
    );
    let o = bin()
        .arg(observations("capped.jsonl"))
        .arg("--strict-pass")
        .arg(&strict)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    let s = summary(&o);
    assert_eq!(s["matched"], 1);
    assert_eq!(s["strict_failures_omitted"], 4);
    assert_eq!(s["strict_drop_incomplete"], true);
    let e = stderr(&o);
    assert!(e.contains('4') && e.contains("--max-failures"), "{e}");
}

#[test]
fn truncated_strict_pass_output_exits_2_without_a_summary() {
    let strict = tmp(
        "cut.strict.jsonl",
        &[failure(10, 1_000), FILE.into()].concat(),
    );
    let o = bin()
        .arg(observations("cut.jsonl"))
        .arg("--strict-pass")
        .arg(&strict)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(o.stdout.len(), 0);
    let e = stderr(&o);
    assert!(
        e.contains("cut.strict.jsonl") && e.contains("no total line"),
        "{e}"
    );
}

#[test]
fn malformed_or_unreadable_strict_pass_output_exits_2_naming_the_file() {
    let bad = tmp("bad.strict.jsonl", &["{\"other\":1}\n", &total(0)].concat());
    for (path, want) in [
        (bad, "line 1"),
        (
            PathBuf::from("/nonexistent/hl-latency.strict.jsonl"),
            "read failed",
        ),
    ] {
        let o = bin()
            .arg(observations("bad.jsonl"))
            .arg("--strict-pass")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2));
        assert_eq!(o.stdout.len(), 0);
        let e = stderr(&o);
        assert!(
            e.contains(&path.display().to_string()) && e.contains(want),
            "{e}"
        );
    }
}

#[test]
fn the_flag_without_a_file_or_given_twice_is_usage_exit_2() {
    let obs = observations("usage.jsonl");
    let obs = obs.to_str().unwrap();
    for args in [
        vec![obs, "--strict-pass"],
        vec!["--strict-pass", "a", "--strict-pass", "b", obs],
        vec!["--strict-pass", "a"],
    ] {
        let o = bin().args(&args).output().unwrap();
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(stderr(&o).contains("usage: hl-latency"), "{args:?}");
        assert_eq!(o.stdout.len(), 0);
    }
}
