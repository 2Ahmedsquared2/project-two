//! Dropping one torn line, not its whole block, when the observations say
//! which line of which hourly file they came from (`file_date`, `file_hour`,
//! `line_offset`) and `hl-strict-pass` lists the failed line by the same
//! file and offset. Semantics decided with the Latency Reason.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

use hl_latency::{
    pair, read_observations, summarize, Observation, ReadError, Source, StrictDrop,
    StrictDropError, StrictFailures,
};
use proptest::prelude::*;

const DATE: u32 = 20_250_727;
const HOUR: u8 = 8;
/// The hourly file `(DATE, HOUR)` as `hl-strict-pass` prints it.
const PATH: &str = "/data/node_fills/hourly/20250727/8";

/// A node observation of the line at `offset` in the file `(DATE, HOUR)`.
fn node(height: u64, time: u64, recv_ns: u64, offset: u64) -> Observation {
    node_in(DATE, HOUR, height, time, recv_ns, offset)
}
fn node_in(date: u32, hour: u8, height: u64, time: u64, recv_ns: u64, offset: u64) -> Observation {
    Observation {
        source: Source::Node,
        block_time_ms: time,
        block_number: Some(height),
        recv_ns,
        file_date: Some(date),
        file_hour: Some(hour),
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

/// A `failure` line as `hl-strict-pass` writes it for a scan-accepted line.
fn failure_in(path: &str, offset: u64, height: u64, time: u64) -> String {
    format!(
        "{{\"failure\":{{\"path\":\"{path}\",\"line\":3,\"offset\":{offset},\"len\":20,\"reason\":\"torn\",\"scan_accepted\":true,\"block_number\":{height},\"block_time_ms\":{time}}}}}\n"
    )
}
fn failure(offset: u64, height: u64, time: u64) -> String {
    failure_in(PATH, offset, height, time)
}
fn file(path: &str) -> String {
    format!("{{\"file\":{{\"path\":\"{path}\",\"records\":9,\"failed\":1}}}}\n")
}
const TOTAL: &str = "{\"total\":{\"files\":1,\"records\":9,\"failed\":1,\"failures_omitted\":0}}\n";

fn strict(lines: &[String]) -> StrictFailures {
    StrictFailures::read([&lines.concat(), TOTAL].concat().as_bytes()).unwrap()
}
fn drop_from(
    lines: &[String],
    obs: &[Observation],
) -> Result<(Vec<Observation>, StrictDrop), StrictDropError> {
    strict(lines).drop_from(obs.to_vec())
}

// ---- Latency's five ----

#[test]
fn a_torn_line_of_a_multi_line_block_goes_and_the_block_stays() {
    // Block 10 came out as three lines; the second was torn. Only its stamp
    // goes: the block still pairs, on its first line, with its WS arrival.
    let obs = [
        node(10, 1_000, 5, 0),
        node(10, 1_000, 6, 100),
        ws(1_000, 50),
        node(10, 1_000, 7, 200),
        node(11, 1_070, 100, 300),
        ws(1_070, 160),
    ];
    let (kept, report) = drop_from(&[failure(100, 10, 1_000), file(PATH)], &obs).unwrap();
    assert_eq!(
        kept,
        [obs[0], obs[2], obs[3], obs[4], obs[5]],
        "only the line at offset 100 is gone"
    );
    assert_eq!(
        report,
        StrictDrop {
            strict_pass_applied: true,
            strict_dropped_observations: 1,
            strict_dropped_blocks: 0,
            strict_dropped_ws_arrivals: 0,
            strict_failures_listed: 1,
            strict_failures_unmatched: 0,
            strict_failures_omitted: 0,
            strict_drop_incomplete: false,
            strict_drop_by_line: true,
            strict_shifted_first_arrival: 0,
        }
    );
    let p = pair(kept);
    assert_eq!(p.pairs.len(), 2);
    assert_eq!((p.pairs[0].node_ns, p.pairs[0].ws_ns), (5, 50));
}

#[test]
fn a_block_whose_only_line_was_torn_is_neither_matched_nor_unmatched() {
    let obs = [
        node(10, 1_000, 5, 0),
        ws(1_000, 50),
        ws(1_000, 55),
        node(11, 1_070, 100, 100),
        ws(1_070, 160),
    ];
    let (kept, report) = drop_from(&[failure(0, 10, 1_000)], &obs).unwrap();
    assert_eq!(kept, [obs[3], obs[4]]);
    assert_eq!(report.strict_dropped_observations, 1);
    assert_eq!(report.strict_dropped_blocks, 1);
    assert_eq!(report.strict_dropped_ws_arrivals, 2);
    assert_eq!(report.strict_shifted_first_arrival, 0);
    let s = summarize(&pair(kept));
    assert_eq!(
        (s.matched, s.node_only, s.ws_only),
        (1, 0, 0),
        "block 10 is not counted as a block the node missed"
    );
}

#[test]
fn a_torn_first_arrival_is_counted_as_a_shifted_block() {
    let obs = [
        node(10, 1_000, 5, 0),
        node(10, 1_000, 9, 100),
        ws(1_000, 50),
        // Block 11 loses a later line: no shift.
        node(11, 1_070, 100, 200),
        node(11, 1_070, 101, 300),
        ws(1_070, 160),
    ];
    let (kept, report) =
        drop_from(&[failure(0, 10, 1_000), failure(300, 11, 1_070)], &obs).unwrap();
    assert_eq!(kept, [obs[1], obs[2], obs[3], obs[5]]);
    assert_eq!(report.strict_dropped_observations, 2);
    assert_eq!(report.strict_dropped_blocks, 0);
    assert_eq!(report.strict_dropped_ws_arrivals, 0);
    assert_eq!(report.strict_shifted_first_arrival, 1);
    let p = pair(kept);
    assert_eq!(
        p.pairs[0].node_ns, 9,
        "block 10 now pairs on its later line"
    );
    assert_eq!(p.pairs[1].node_ns, 100);
}

#[test]
fn a_shift_is_a_later_stamp_and_is_counted_once_per_block() {
    // The two earliest lines of block 10 are torn: one shifted block. Block
    // 11's torn line shares its stamp with a line that stays: no shift.
    let obs = [
        node(10, 1_000, 5, 0),
        node(10, 1_000, 6, 100),
        node(10, 1_000, 9, 200),
        node(11, 1_070, 100, 300),
        node(11, 1_070, 100, 400),
    ];
    let (kept, report) = drop_from(
        &[
            failure(0, 10, 1_000),
            failure(100, 10, 1_000),
            failure(300, 11, 1_070),
        ],
        &obs,
    )
    .unwrap();
    assert_eq!(kept, [obs[2], obs[4]]);
    assert_eq!(report.strict_shifted_first_arrival, 1);
}

#[test]
fn observations_with_and_without_offsets_in_one_file_are_refused() {
    let obs = [
        node(10, 1_000, 5, 0),
        old_node(11, 1_070, 100),
        old_node(12, 1_140, 200),
    ];
    assert_eq!(
        drop_from(&[failure(0, 10, 1_000)], &obs),
        Err(StrictDropError::Mixed {
            with: 1,
            without: 2
        })
    );
    // Also with nothing listed: the file itself is the problem.
    assert!(matches!(
        drop_from(&[], &obs),
        Err(StrictDropError::Mixed { .. })
    ));
}

#[test]
fn a_matched_offset_with_another_block_is_refused() {
    let obs = [node(10, 1_000, 5, 0), node(11, 1_070, 100, 100)];
    // Height differs; block time differs.
    for (height, time) in [(12, 1_070), (11, 1_071)] {
        assert_eq!(
            drop_from(&[failure(100, height, time)], &obs),
            Err(StrictDropError::KeyMismatch {
                path: PATH.to_owned(),
                offset: 100,
                listed_number: height,
                listed_time_ms: time,
                observed_number: Some(11),
                observed_time_ms: 1_070,
            })
        );
    }
}

// ---- the rest of the contract ----

#[test]
fn a_listed_line_nobody_observed_drops_nothing_and_is_counted_unmatched() {
    // Offset 50 is not a line the capture stamped, though block 10 was seen.
    // Listed twice: one unmatched line.
    let obs = [node(10, 1_000, 5, 0), ws(1_000, 50)];
    let (kept, report) =
        drop_from(&[failure(50, 10, 1_000), failure(50, 10, 1_000)], &obs).unwrap();
    assert_eq!(kept, obs);
    assert_eq!(report.strict_failures_listed, 2);
    assert_eq!(report.strict_failures_unmatched, 1);
    assert_eq!(report.strict_dropped_observations, 0);
    assert_eq!(report.strict_dropped_blocks, 0);
    assert!(report.strict_drop_by_line);
}

#[test]
fn the_line_is_named_by_date_hour_and_offset_together() {
    // Offset 0 exists in three files; only 20250727/9 is listed.
    let obs = [
        node_in(DATE, 8, 10, 1_000, 5, 0),
        node_in(DATE, 9, 11, 1_070, 6, 0),
        node_in(DATE + 1, 9, 12, 1_140, 7, 0),
    ];
    let (kept, report) =
        drop_from(&[failure_in("/data/hourly/20250727/9", 0, 11, 1_070)], &obs).unwrap();
    assert_eq!(kept, [obs[0], obs[2]]);
    assert_eq!(report.strict_dropped_observations, 1);
}

#[test]
fn the_hour_is_compared_as_a_number() {
    for path in ["/data/hourly/20250727/08", "20250727/8", "./20250727/08"] {
        let obs = [node(10, 1_000, 5, 0), node(10, 1_000, 6, 100)];
        let (kept, _) = drop_from(&[failure_in(path, 100, 10, 1_000)], &obs).unwrap();
        assert_eq!(kept, [obs[0]], "{path}");
    }
}

#[test]
fn the_same_line_listed_twice_is_one_dropped_observation() {
    let obs = [node(10, 1_000, 5, 0), node(10, 1_000, 6, 100)];
    let (kept, report) =
        drop_from(&[failure(100, 10, 1_000), failure(100, 10, 1_000)], &obs).unwrap();
    assert_eq!(kept, [obs[0]]);
    assert_eq!(report.strict_failures_listed, 2);
    assert_eq!(report.strict_dropped_observations, 1);
}

#[test]
fn a_failure_whose_path_gives_no_date_and_hour_is_refused() {
    let obs = [node(10, 1_000, 5, 0)];
    for path in [
        "f",
        "/data/hourly/20250727/24",
        "/data/hourly/2025072/8",
        "/data/hourly/20250727/008",
        "/data/hourly/20250727/8.gz",
        "8",
    ] {
        assert_eq!(
            drop_from(&[failure_in(path, 0, 10, 1_000)], &obs),
            Err(StrictDropError::FailureWithoutFile {
                path: path.to_owned()
            }),
            "{path}"
        );
    }
}

#[test]
fn two_paths_for_one_date_and_hour_are_refused() {
    // Two roots in one strict-pass run: an offset no longer names one line.
    let obs = [node(10, 1_000, 5, 0), node(11, 1_070, 6, 100)];
    let other = "/backup/node_fills/hourly/20250727/08";
    let want = Err(StrictDropError::TwoPathsOneFile {
        first: PATH.to_owned(),
        second: other.to_owned(),
    });
    // Two failures; and a failure against a file that only has a `file` line.
    assert_eq!(
        drop_from(
            &[failure(0, 10, 1_000), failure_in(other, 100, 11, 1_070)],
            &obs
        ),
        want
    );
    assert_eq!(
        drop_from(&[failure(0, 10, 1_000), file(PATH), file(other)], &obs),
        want
    );
    // The same path twice is one file.
    assert!(drop_from(&[file(PATH), failure(0, 10, 1_000), file(PATH)], &obs).is_ok());
}

#[test]
fn two_observations_of_one_line_are_refused() {
    let obs = [node(10, 1_000, 5, 100), node(11, 1_070, 6, 100)];
    assert_eq!(
        drop_from(&[], &obs),
        Err(StrictDropError::DuplicateLine {
            date: DATE,
            hour: HOUR,
            offset: 100
        })
    );
}

#[test]
fn an_old_capture_keeps_the_whole_block_drop() {
    // No observation carries an offset: every stamp of the listed block
    // goes, with its WS arrival, as before. The failure's path is not used.
    let obs = [
        old_node(10, 1_000, 5),
        old_node(10, 1_000, 6),
        ws(1_000, 50),
        old_node(11, 1_070, 100),
        ws(1_070, 160),
    ];
    for path in [PATH, "f"] {
        let lines = [failure_in(path, 100, 10, 1_000)];
        let (kept, report) = drop_from(&lines, &obs).unwrap();
        assert_eq!(kept, [obs[3], obs[4]]);
        assert_eq!(report.strict_dropped_observations, 2);
        assert_eq!(report.strict_dropped_blocks, 1);
        assert_eq!(report.strict_dropped_ws_arrivals, 1);
        assert!(!report.strict_drop_by_line);
        assert_eq!(report.strict_shifted_first_arrival, 0);
        assert_eq!((kept, report), strict(&lines).apply(obs.to_vec()));
    }
}

#[test]
fn an_observation_with_only_some_of_the_three_fields_is_refused() {
    let whole = node(10, 1_000, 5, 0);
    for partial in [
        Observation {
            file_date: None,
            ..whole
        },
        Observation {
            file_hour: None,
            ..whole
        },
        Observation {
            line_offset: None,
            ..whole
        },
        Observation {
            file_date: None,
            file_hour: None,
            ..whole
        },
    ] {
        assert_eq!(
            drop_from(&[], &[whole, partial]),
            Err(StrictDropError::PartialLocation {
                block_time_ms: 1_000,
                recv_ns: 5
            })
        );
        // Reading the observation file refuses it first, with its line.
        let text = format!(
            "{}\n{}\n",
            serde_json::to_string(&whole).unwrap(),
            serde_json::to_string(&partial).unwrap()
        );
        match read_observations(text.as_bytes()) {
            Err(ReadError::Bad { line: 2, msg }) => assert!(msg.contains("line_offset"), "{msg}"),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_scan_accepted_failure_without_its_path_or_offset_is_malformed() {
    for text in [
        "{\"failure\":{\"offset\":1,\"scan_accepted\":true,\"block_number\":1,\"block_time_ms\":5}}\n",
        "{\"failure\":{\"path\":\"20250727/8\",\"scan_accepted\":true,\"block_number\":1,\"block_time_ms\":5}}\n",
    ] {
        assert!(
            StrictFailures::read([text, TOTAL].concat().as_bytes()).is_err(),
            "{text}"
        );
    }
}

proptest! {
    /// Whatever is observed and listed: exactly the listed lines go, a WS
    /// arrival goes only when its block lost every node line, the rest keeps
    /// its order and the counters account for every observation.
    #[test]
    fn by_line_drop_removes_exactly_the_listed_lines(
        // Each observation: node (height and time from one small block id,
        // so heights and times agree) or WS; its index is its line.
        raw in proptest::collection::vec((any::<bool>(), 0u64..6, 0u64..40), 1..60),
        listed in proptest::collection::vec(0usize..70, 0..12),
    ) {
        let mut obs: Vec<Observation> = raw
            .iter()
            .zip(0u64..)
            .map(|(&(is_node, block, r), i)| {
                if is_node { node(block, block * 70, r, i * 10) } else { ws(block * 70, r) }
            })
            .collect();
        // At least one located node observation, or it is an old capture.
        obs.push(node(9, 630, 1, 10_000));

        // A listed index names that observation's line when it is a node
        // line, else an offset nobody stamped.
        let lines: Vec<String> = listed
            .iter()
            .map(|&i| match obs.get(i) {
                Some(o) if o.source == Source::Node => {
                    failure(o.line_offset.unwrap(), o.block_number.unwrap(), o.block_time_ms)
                }
                _ => failure(u64::try_from(i).unwrap() * 10 + 1, 1, 70),
            })
            .collect();
        let (kept, report) = drop_from(&lines, &obs).unwrap();

        let torn = |i: usize| listed.contains(&i) && obs[i].source == Source::Node;
        let left = |time: u64| {
            (0..obs.len()).any(|i| obs[i].source == Source::Node && obs[i].block_time_ms == time && !torn(i))
        };
        let had = |time: u64| obs.iter().any(|o| o.source == Source::Node && o.block_time_ms == time);
        let want: Vec<Observation> = (0..obs.len())
            .filter(|&i| match obs[i].source {
                Source::Node => !torn(i),
                Source::PublicWs => !had(obs[i].block_time_ms) || left(obs[i].block_time_ms),
            })
            .map(|i| obs[i])
            .collect();
        prop_assert_eq!(&kept, &want);

        let count = |n: usize| u64::try_from(n).unwrap();
        let nodes_gone = (0..obs.len()).filter(|&i| torn(i)).count();
        prop_assert_eq!(report.strict_dropped_observations, count(nodes_gone));
        prop_assert_eq!(
            report.strict_dropped_observations + report.strict_dropped_ws_arrivals,
            count(obs.len() - kept.len())
        );
        let times: std::collections::BTreeSet<u64> = obs.iter().map(|o| o.block_time_ms).collect();
        let (mut gone, mut shifted) = (0, 0);
        for &t in &times {
            let stamps = |keep_torn: bool| {
                (0..obs.len())
                    .filter(|&i| obs[i].source == Source::Node && obs[i].block_time_ms == t)
                    .filter(|&i| keep_torn || !torn(i))
                    .map(|i| obs[i].recv_ns)
                    .min()
            };
            match (stamps(true), stamps(false)) {
                (Some(_), None) => gone += 1,
                (Some(before), Some(after)) if after > before => shifted += 1,
                _ => {}
            }
        }
        prop_assert_eq!(report.strict_dropped_blocks, gone);
        prop_assert_eq!(report.strict_shifted_first_arrival, shifted);
        let mut unmatched: Vec<usize> =
            listed.iter().copied().filter(|&i| !torn(i)).collect();
        unmatched.sort_unstable();
        unmatched.dedup();
        prop_assert_eq!(report.strict_failures_unmatched, count(unmatched.len()));
        prop_assert_eq!(report.strict_failures_listed, count(listed.len()));
        prop_assert!(report.strict_drop_by_line);
    }
}

// ---- the binary ----

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hl-latency"))
}
fn tmp(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hl-latency-line-drop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn observations(name: &str, obs: &[Observation]) -> PathBuf {
    let body: String = obs
        .iter()
        .map(|o| serde_json::to_string(o).unwrap() + "\n")
        .collect();
    tmp(name, &body)
}
fn run(name: &str, obs: &[Observation], lines: &[String]) -> Output {
    let strict = tmp(
        &format!("{name}.strict.jsonl"),
        &[&lines.concat(), TOTAL].concat(),
    );
    bin()
        .arg(observations(&format!("{name}.jsonl"), obs))
        .arg("--strict-pass")
        .arg(strict)
        .output()
        .unwrap()
}

#[test]
fn the_flag_drops_one_line_and_reports_the_counts() {
    // Block 10: first line torn, a later one stays. Block 11: only line torn.
    let obs = [
        node(10, 1_000, 5, 0),
        node(10, 1_000, 9, 100),
        ws(1_000, 50),
        node(11, 1_070, 100, 200),
        ws(1_070, 160),
        node(12, 1_140, 200, 300),
        ws(1_140, 230),
    ];
    let o = run(
        "ok",
        &obs,
        &[
            failure(0, 10, 1_000),
            failure(200, 11, 1_070),
            failure(999, 13, 1_210),
            file(PATH),
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(s["matched"], 2);
    assert_eq!(s["node_only"], 0);
    assert_eq!(s["ws_only"], 0);
    assert_eq!(s["min_ns"], 30);
    assert_eq!(s["max_ns"], 41, "block 10 pairs on its later line: 50 - 9");
    assert_eq!(s["strict_pass_applied"], true);
    assert_eq!(s["strict_drop_by_line"], true);
    assert_eq!(s["strict_dropped_observations"], 2);
    assert_eq!(s["strict_dropped_blocks"], 1);
    assert_eq!(s["strict_dropped_ws_arrivals"], 1);
    assert_eq!(s["strict_shifted_first_arrival"], 1);
    assert_eq!(s["strict_failures_listed"], 3);
    assert_eq!(s["strict_failures_unmatched"], 1);
}

#[test]
fn files_that_cannot_be_matched_by_line_exit_2_without_a_summary() {
    let located = [node(10, 1_000, 5, 0), node(11, 1_070, 100, 100)];
    let mixed = [node(10, 1_000, 5, 0), old_node(11, 1_070, 100)];
    for (name, obs, lines, want) in [
        ("mixed", &mixed[..], vec![failure(0, 10, 1_000)], "mixed"),
        (
            "key",
            &located[..],
            vec![failure(100, 12, 1_070)],
            "wrong file or offset",
        ),
        (
            "nofile",
            &located[..],
            vec![failure_in("f", 0, 10, 1_000)],
            "YYYYMMDD",
        ),
    ] {
        let o = run(name, obs, &lines);
        assert_eq!(o.status.code(), Some(2), "{name}");
        assert_eq!(o.stdout.len(), 0, "{name}");
        let e = stderr(&o);
        assert!(
            e.contains(want) && e.contains(&format!("{name}.strict.jsonl")),
            "{name}: {e}"
        );
    }
}

#[test]
fn a_partial_location_in_the_observation_file_exits_2_naming_the_line() {
    let body = "{\"source\":\"node\",\"block_time_ms\":1,\"block_number\":1,\"recv_ns\":1}\n{\"source\":\"node\",\"block_time_ms\":2,\"block_number\":2,\"recv_ns\":2,\"file_date\":20250727,\"line_offset\":0}\n";
    let o = bin().arg(tmp("partial.jsonl", body)).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(o.stdout.len(), 0);
    assert!(stderr(&o).contains("line 2"), "{}", stderr(&o));
}
