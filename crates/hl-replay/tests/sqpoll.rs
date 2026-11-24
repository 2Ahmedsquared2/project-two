//! The `compio-sqpoll` arm's kernel polling thread must not share the hot
//! thread's core. Created by a pinned thread with no CPU of its own, the
//! `iou-sqp` thread inherits that thread's affinity and spins on its core
//! (seen on the Tokyo box: t1-t0 p50 9 ms where every other arm has ~1 µs).
//! So the arm takes a core for it (`sqpoll_core`, `--sqpoll-core`), refuses
//! to run pinned without one, and reports where the thread may run.
#![cfg(all(target_os = "linux", feature = "compio"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use hl_replay::{generated_lines, run_round, Arm, LoopbackAddr, RoundConfig, RoundOutcome};

/// One ring with a polling thread at a time in this process: the arm finds
/// its thread as the `iou-sqp` task that appeared when its ring was made.
static ONE_RING: Mutex<()> = Mutex::new(());
fn one_ring() -> MutexGuard<'static, ()> {
    ONE_RING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hl-replay-sqpoll-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cfg(name: &str, hot_core: Option<usize>, sqpoll_core: Option<usize>) -> RoundConfig {
    RoundConfig {
        lines: Arc::new(generated_lines(64, 20)),
        measured: 250,
        warmup: 50,
        interval: Duration::from_micros(50),
        feeder_core: None,
        hot_core,
        sqpoll_core,
        tail_dir: tmp(name),
        sink_bind: LoopbackAddr::ephemeral(),
    }
}

/// Two cores this process may run on, or `None` on a one-core host.
fn two_cores() -> Option<(usize, usize)> {
    let ids = core_affinity::get_core_ids()?;
    Some((ids.first()?.id, ids.get(1)?.id))
}

/// The round's polling-thread CPUs, or `None` when `io_uring` (or its
/// polling thread) cannot be set up on this host, which the arm may say for
/// reasons that have nothing to do with the core.
fn sqpoll_cpus(c: &RoundConfig) -> Option<String> {
    match run_round(Arm::CompioSqpoll, c).unwrap() {
        RoundOutcome::Ran(r) => {
            assert_eq!(r.sink.orders, 300);
            Some(
                r.sqpoll_cpus
                    .expect("a round that ran says where its polling thread may run"),
            )
        }
        RoundOutcome::Unavailable(why) => {
            assert!(
                !why.contains("--sqpoll-core"),
                "refused for its cores, not for the host: {why}"
            );
            eprintln!("compio-sqpoll: unavailable on this host: {why}");
            None
        }
    }
}

fn refused_for_its_core(c: &RoundConfig) -> String {
    match run_round(Arm::CompioSqpoll, c).unwrap() {
        RoundOutcome::Unavailable(why) => {
            assert!(why.contains("--sqpoll-core"), "{why}");
            why
        }
        RoundOutcome::Ran(r) => panic!(
            "ran with the polling thread on {:?} beside a hot thread on {:?}",
            r.sqpoll_cpus, c.hot_core
        ),
    }
}

#[test]
fn a_pinned_hot_thread_with_no_core_for_the_polling_thread_is_unavailable() {
    let _ring = one_ring();
    let (hot, _) = two_cores().unwrap_or((0, 0));
    let why = refused_for_its_core(&cfg("none", Some(hot), None));
    assert!(why.contains(&format!("core {hot}")), "{why}");
}

#[test]
fn the_polling_thread_on_the_hot_core_is_unavailable() {
    let _ring = one_ring();
    let (hot, _) = two_cores().unwrap_or((0, 0));
    refused_for_its_core(&cfg("same", Some(hot), Some(hot)));
}

#[test]
fn the_polling_thread_runs_only_on_the_core_it_was_given() {
    let _ring = one_ring();
    let Some((hot, other)) = two_cores() else {
        eprintln!("skipped: one core");
        return;
    };
    for (name, hot, sq) in [("ab", hot, other), ("ba", other, hot)] {
        let Some(cpus) = sqpoll_cpus(&cfg(name, Some(hot), Some(sq))) else {
            return;
        };
        assert_eq!(cpus, sq.to_string(), "hot thread on core {hot}");
    }
}

#[test]
fn an_unpinned_run_may_still_name_the_polling_threads_core() {
    let _ring = one_ring();
    let (_, core) = two_cores().unwrap_or((0, 0));
    let Some(cpus) = sqpoll_cpus(&cfg("free-hot", None, Some(core))) else {
        return;
    };
    assert_eq!(cpus, core.to_string());
    // With nothing pinned the thread is free, and the round still says where.
    let cpus = sqpoll_cpus(&cfg("free", None, None)).unwrap();
    assert_ne!(cpus, "");
}

#[test]
fn only_the_sqpoll_arm_has_a_polling_thread() {
    let _ring = one_ring();
    for arm in [Arm::Compio, Arm::BusyPoll] {
        // A polling-thread core does not make another arm unavailable.
        let c = cfg(arm.name(), Some(0), Some(0));
        match run_round(arm, &c).unwrap() {
            RoundOutcome::Ran(r) => assert_eq!(r.sqpoll_cpus, None, "{arm:?}"),
            RoundOutcome::Unavailable(why) => {
                assert!(arm.is_io_uring() && !why.contains("--sqpoll-core"), "{why}");
            }
        }
    }
}

// ---- the binary ----

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hl-replay"))
        .args(["--rounds", "1", "--lines", "100", "--warmup", "10"])
        .args(["--interval-us", "30"])
        .args(args)
        .output()
        .unwrap()
}
fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}
fn row<'a>(text: &'a str, arm: &str) -> &'a str {
    text.lines()
        .find(|l| l.starts_with(&format!("{arm} ")))
        .unwrap_or_else(|| panic!("no {arm} row in:\n{text}"))
}

#[test]
fn the_default_pinned_run_reports_the_sqpoll_arm_unavailable_and_says_how() {
    let _ring = one_ring();
    let o = run(&["--arms", "compio-sqpoll,busy-poll", "--hot-core", "0"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = stdout(&o);
    let sq = row(&text, "compio-sqpoll");
    assert!(
        sq.contains("unavailable") && sq.contains("--sqpoll-core"),
        "{sq}"
    );
    assert!(row(&text, "busy-poll").contains("n=90"));
    let pinning = text.lines().find(|l| l.starts_with("pinning:")).unwrap();
    assert!(pinning.contains("sqpoll thread unpinned"), "{pinning}");
}

#[test]
fn the_flag_pins_the_polling_thread_and_the_report_says_where() {
    let _ring = one_ring();
    let Some((hot, other)) = two_cores() else {
        eprintln!("skipped: one core");
        return;
    };
    let (hot, other) = (hot.to_string(), other.to_string());
    let o = run(&[
        "--arms",
        "compio-sqpoll",
        "--hot-core",
        &hot,
        "--feeder-core",
        &other,
        "--sqpoll-core",
        &other,
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = stdout(&o);
    let pinning = text.lines().find(|l| l.starts_with("pinning:")).unwrap();
    assert!(
        pinning.contains(&format!("sqpoll thread core {other}")),
        "{pinning}"
    );
    let sq = row(&text, "compio-sqpoll");
    assert!(!sq.contains("--sqpoll-core"), "{sq}");
    if !sq.contains("unavailable") {
        // What the kernel says, not what was asked for.
        assert!(sq.contains(&format!("sqpoll=cpus:{other}")), "{sq}");
    }
}

#[test]
fn the_polling_threads_core_equal_to_the_hot_core_is_a_usage_error() {
    let o = run(&["--hot-core", "1", "--sqpoll-core", "1"]);
    assert_eq!(o.status.code(), Some(2));
    let e = String::from_utf8_lossy(&o.stderr);
    assert!(
        e.contains("--sqpoll-core") && e.contains("--hot-core"),
        "{e}"
    );
    assert_eq!(o.stdout.len(), 0);
    // Not a number: also usage.
    assert_eq!(run(&["--sqpoll-core", "x"]).status.code(), Some(2));
    // With nothing else pinned it is fine.
    let o = run(&["--arms", "busy-poll", "--no-pin", "--sqpoll-core", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
