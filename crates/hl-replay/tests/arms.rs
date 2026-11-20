//! Every arm replays the same lines end to end: N lines in, N signed orders at
//! the loopback sink, and one histogram sample per measured line.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hl_replay::{
    generated_lines, load_lines, run_interleaved, run_round, Arm, LoopbackAddr, RoundConfig,
    RoundOutcome,
};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hl-replay-arms-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cfg(name: &str, measured: u64, warmup: u64) -> RoundConfig {
    RoundConfig {
        lines: Arc::new(generated_lines(64, 20)),
        measured,
        warmup,
        interval: Duration::from_micros(50),
        feeder_core: None,
        hot_core: None,
        tail_dir: tmp(name),
        sink_bind: LoopbackAddr::ephemeral(),
    }
}

fn check(arm: Arm) {
    let c = cfg(arm.name(), 250, 50);
    match run_round(arm, &c).unwrap() {
        RoundOutcome::Ran(r) => {
            assert_eq!(r.fed, 300, "{arm:?}");
            assert_eq!(
                r.sink.orders, 300,
                "{arm:?}: every line becomes one order at the sink"
            );
            assert!(r.sink.bytes > 300 * 200, "{arm:?}: {}", r.sink.bytes);
            assert_eq!(r.hists.e2e.count(), 250, "{arm:?}: warmup excluded");
            assert_eq!(r.hists.parse.count(), 250, "{arm:?}");
            assert_eq!(r.hists.detect.count(), 250, "{arm:?}");
            let (p, e) = (r.hists.parse.summary(), r.hists.e2e.summary());
            assert!(
                p.p50 > 0 && p.p50 <= e.p50,
                "{arm:?}: t1-t0 {p:?} within t2-t0 {e:?}"
            );
            assert!(
                e.p50 <= e.p99 && e.p99 <= e.p999 && e.p999 <= e.max,
                "{arm:?}: {e:?}"
            );
            assert_eq!(r.hot_pinned, None, "{arm:?}: no pinning was requested");
        }
        RoundOutcome::Unavailable(why) => {
            assert!(arm.is_io_uring(), "{arm:?} may not be unavailable: {why}");
            assert_ne!(why, "");
            eprintln!("{}: unavailable: {why}", arm.name());
        }
    }
}

#[test]
fn busy_poll_arm() {
    check(Arm::BusyPoll);
}
#[test]
fn tokio_current_thread_arm() {
    check(Arm::TokioCurrentThread);
}
#[test]
fn tokio_multi_thread_arm() {
    check(Arm::TokioMultiThread);
}
#[test]
fn compio_arm_runs_or_reports_unavailable() {
    check(Arm::Compio);
}
#[test]
fn compio_sqpoll_arm_runs_or_reports_unavailable() {
    check(Arm::CompioSqpoll);
}
#[test]
fn file_busy_poll_arm() {
    check(Arm::FileBusyPoll);
}
#[test]
fn file_poll_10us_arm() {
    check(Arm::FilePoll10us);
}
#[test]
fn file_poll_100us_arm() {
    check(Arm::FilePoll100us);
}
#[test]
fn file_inotify_arm() {
    check(Arm::FileInotify);
}

#[test]
fn arm_names_round_trip_and_are_distinct() {
    let mut names = std::collections::BTreeSet::new();
    for arm in Arm::RUNTIME.iter().chain(Arm::DETECT.iter()) {
        assert_eq!(arm.name().parse::<Arm>().unwrap(), *arm);
        assert!(names.insert(arm.name()));
        assert!(!arm.name().contains(' '), "names are file-name safe");
    }
    assert_eq!(names.len(), 9);
    assert!("epoll-magic".parse::<Arm>().is_err());
    assert_eq!(Arm::RUNTIME[0].name(), "busy-poll");
    assert_eq!(Arm::RUNTIME[4].name(), "compio-sqpoll");
    assert_eq!(Arm::DETECT[3].name(), "file-inotify");
}

#[test]
fn rounds_interleave_arms_and_merge_per_arm() {
    let arms = [Arm::BusyPoll, Arm::TokioCurrentThread, Arm::Compio];
    let reports = run_interleaved(&arms, 3, &cfg("interleave", 100, 20)).unwrap();
    assert_eq!(reports.len(), 3);
    for (r, arm) in reports.iter().zip(arms) {
        assert_eq!(r.arm, arm);
        if let Some(why) = &r.unavailable {
            assert!(arm.is_io_uring(), "{arm:?}: {why}");
            assert_eq!(r.rounds_run, 0);
            assert_eq!(r.hists.e2e.count(), 0);
        } else {
            assert_eq!(r.rounds_run, 3);
            assert_eq!(r.hists.e2e.count(), 300);
            assert_eq!(r.orders, 360);
        }
    }
}

#[test]
fn recorded_lines_replay_like_generated_ones() {
    let dir = tmp("recorded");
    let path = dir.join("node.jsonl");
    let mut text = String::new();
    for l in generated_lines(10, 3) {
        text.push_str(&l);
    }
    text.push('\n'); // a blank line is tolerated in a recording
    std::fs::write(&path, &text).unwrap();
    let lines = load_lines(&path).unwrap();
    assert_eq!(lines.len(), 10);
    assert!(lines
        .iter()
        .all(|l| l.ends_with('\n') && !l.trim().is_empty()));
    let mut c = cfg("recorded-run", 25, 5); // 30 lines from a 10-line file: it cycles
    c.lines = Arc::new(lines);
    let RoundOutcome::Ran(r) = run_round(Arm::BusyPoll, &c).unwrap() else {
        panic!("busy-poll is always available");
    };
    assert_eq!(r.sink.orders, 30);
    assert_eq!(r.hists.e2e.count(), 25);
}

#[test]
fn a_recording_with_no_lines_or_a_bad_line_is_refused_up_front() {
    let dir = tmp("bad-recording");
    let empty = dir.join("empty.jsonl");
    std::fs::write(&empty, "\n\n").unwrap();
    assert!(load_lines(&empty).is_err());
    assert!(load_lines(&dir.join("missing.jsonl")).is_err());
    let bad = dir.join("bad.jsonl");
    std::fs::write(&bad, "{\"not\":\"a node line\"}\n").unwrap();
    let err = load_lines(&bad).unwrap_err().to_string();
    assert!(err.contains("line 1"), "{err}");
}

#[test]
fn a_round_with_nothing_to_measure_is_refused() {
    let mut c = cfg("zero", 0, 10);
    assert!(run_round(Arm::BusyPoll, &c).is_err());
    c.measured = 10;
    c.lines = Arc::new(Vec::new());
    assert!(run_round(Arm::BusyPoll, &c).is_err());
}

#[test]
fn pinning_is_reported_when_requested() {
    let mut c = cfg("pin", 50, 10);
    c.hot_core = Some(0);
    c.feeder_core = Some(0);
    let RoundOutcome::Ran(r) = run_round(Arm::BusyPoll, &c).unwrap() else {
        panic!("busy-poll is always available");
    };
    assert_eq!(r.hot_pinned, Some(true));
    // The multi-thread runtime is the unpinned baseline by design.
    let RoundOutcome::Ran(r) = run_round(Arm::TokioMultiThread, &c).unwrap() else {
        panic!("tokio is always available");
    };
    assert_eq!(r.hot_pinned, None);
}
