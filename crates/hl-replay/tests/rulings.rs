//! Latency's rulings on the first sample: scheduled-time stamps as the
//! headline, an in-place reader for owned-buffer runtimes, a random arm order
//! per round and the machine's load recorded beside each round.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use hl_replay::{
    active_pipeline, generated_lines, node_line, run_round, run_shuffled, Arm, Clock, Core,
    LatencyHist, LineBuf, LoopbackAddr, RoundConfig, RoundOutcome, Step,
};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hl-replay-rulings-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cfg(name: &str, measured: u64, warmup: u64) -> RoundConfig {
    RoundConfig {
        lines: Arc::new(generated_lines(16, 5)),
        measured,
        warmup,
        interval: Duration::from_micros(50),
        feeder_core: None,
        hot_core: None,
        tail_dir: tmp(name),
        sink_bind: LoopbackAddr::ephemeral(),
    }
}

/// A line the feeder sent 1 ms late: measured from when it was *scheduled*,
/// the lateness is in the number; measured from the actual write, it is not.
#[test]
fn scheduled_time_charges_a_late_feeder_to_the_arm() {
    let clock = Clock::new();
    std::thread::sleep(Duration::from_millis(3));
    let wrote = clock.now_ns();
    let scheduled = wrote - 1_000_000;
    let stamps = |v: u64| -> Arc<[AtomicU64]> { [AtomicU64::new(v)].into_iter().collect() };
    let mut core = Core::new(active_pipeline(), clock, 0, 1 << 16)
        .with_feed_stamps(stamps(wrote))
        .with_schedule_stamps(stamps(scheduled));
    core.push(node_line(1, 5).as_bytes()).unwrap();
    let t0 = clock.now_ns();
    assert_eq!(core.step(t0).unwrap(), Step::Order);
    core.sent();
    let h = core.into_hists();
    assert_eq!(h.sched_total.count(), 1);
    assert_eq!(h.total.count(), 1);
    let (sched, actual) = (h.sched_total.max(), h.total.max());
    let extra = sched - actual;
    assert!(
        (900_000..=1_100_000).contains(&extra),
        "sched {sched} actual {actual}"
    );
}

#[test]
fn without_schedule_stamps_nothing_is_invented() {
    let clock = Clock::new();
    let mut core = Core::new(active_pipeline(), clock, 0, 1 << 16);
    core.push(node_line(1, 5).as_bytes()).unwrap();
    assert_eq!(core.step(clock.now_ns()).unwrap(), Step::Order);
    core.sent();
    let h = core.into_hists();
    assert_eq!(
        (h.sched_total.count(), h.total.count(), h.e2e.count()),
        (0, 0, 1)
    );
}

fn at_least(a: &LatencyHist, b: &LatencyHist, what: &str) {
    for q in [50_000, 90_000, 99_000, 100_000] {
        assert!(
            a.value_at_per_100k(q) >= b.value_at_per_100k(q),
            "{what} at {q}: {} < {}",
            a.value_at_per_100k(q),
            b.value_at_per_100k(q)
        );
    }
}

/// The schedule is never later than the write, so every arm's scheduled-time
/// distribution dominates its actual-time one.
#[test]
fn every_arm_records_the_scheduled_time_headline() {
    for arm in [
        Arm::BusyPoll,
        Arm::TokioCurrentThread,
        Arm::Compio,
        Arm::FilePoll10us,
    ] {
        let RoundOutcome::Ran(r) = run_round(arm, &cfg(arm.name(), 200, 20)).unwrap() else {
            assert!(arm.is_io_uring());
            continue;
        };
        assert_eq!(r.hists.sched_total.count(), 200, "{arm:?}");
        assert_eq!(r.hists.total.count(), 200, "{arm:?}");
        at_least(&r.hists.sched_total, &r.hists.total, arm.name());
        at_least(&r.hists.total, &r.hists.e2e, arm.name());
    }
}

/// An owned-buffer runtime (`io_uring`) reads into the reader's own storage:
/// take it, fill it at the offset, give it back. No second buffer, no copy.
#[test]
fn the_readers_storage_can_be_lent_and_filled_in_place() {
    let mut b = LineBuf::with_capacity(32);
    b.push(b"ab").unwrap();
    let (mut storage, at) = b.take_storage().unwrap();
    assert_eq!((storage.len(), at), (32, 2));
    assert_eq!(
        b.buffered(),
        0,
        "nothing can be read while the storage is lent"
    );
    assert!(b.next_line().is_none());
    assert!(b.push(b"x").is_err());
    storage[at..at + 5].copy_from_slice(b"c\nde\n");
    b.restore_storage(storage, 5);
    assert_eq!(b.next_line().unwrap(), b"abc");
    assert_eq!(b.next_line().unwrap(), b"de");
    assert!(b.next_line().is_none());
    // A pending partial line is moved to the front before lending.
    b.push(b"zz\nq").unwrap();
    assert_eq!(b.next_line().unwrap(), b"zz");
    let (storage, at) = b.take_storage().unwrap();
    assert_eq!((at, storage[0]), (1, b'q'));
    b.restore_storage(storage, 0);
    assert_eq!(b.buffered(), 1);
    // Full of one unfinished line: nothing to lend.
    let mut full = LineBuf::with_capacity(4);
    full.push(b"abcd").unwrap();
    assert!(full.take_storage().is_err());
}

#[test]
fn each_round_runs_the_arms_in_its_own_seeded_random_order() {
    let arms = [
        Arm::BusyPoll,
        Arm::TokioCurrentThread,
        Arm::FileBusyPoll,
        Arm::FilePoll10us,
    ];
    let run = run_shuffled(&arms, 6, &cfg("shuffle", 40, 10), 0xC0FFEE).unwrap();
    assert_eq!(run.seed, 0xC0FFEE);
    assert_eq!(run.rounds.len(), 6);
    // Results come back in the order asked for, whatever order they ran in.
    assert_eq!(run.arms.iter().map(|a| a.arm).collect::<Vec<_>>(), arms);
    for a in &run.arms {
        assert_eq!(a.rounds_run, 6);
        assert_eq!(a.hists.e2e.count(), 240);
    }
    let mut distinct = std::collections::BTreeSet::new();
    for r in &run.rounds {
        let mut sorted: Vec<&str> = r.order.iter().map(|a| a.name()).collect();
        distinct.insert(sorted.clone());
        sorted.sort_unstable();
        let mut want: Vec<&str> = arms.iter().map(|a| a.name()).collect();
        want.sort_unstable();
        assert_eq!(sorted, want, "every round runs every arm exactly once");
        // The machine's load is recorded as the kernel printed it.
        for load in [&r.load_before, &r.load_after] {
            assert!(load.split_whitespace().count() >= 4, "{load:?}");
            assert!(load.contains('/'), "runnable/total field: {load:?}");
        }
    }
    assert!(
        distinct.len() >= 3,
        "6 rounds of 4 arms, all in one order: not shuffled"
    );
    let again = run_shuffled(&arms, 6, &cfg("shuffle2", 40, 10), 0xC0FFEE).unwrap();
    let orders = |r: &hl_replay::Run| r.rounds.iter().map(|x| x.order.clone()).collect::<Vec<_>>();
    assert_eq!(orders(&run), orders(&again), "same seed, same order");
    let other = run_shuffled(&arms, 6, &cfg("shuffle3", 40, 10), 1).unwrap();
    assert_ne!(orders(&run), orders(&other));
}

#[test]
fn the_cli_reports_seed_round_orders_load_and_the_headline_first() {
    let out = tmp("cli-out");
    let o = Command::new(env!("CARGO_BIN_EXE_hl-replay"))
        .args([
            "--arms",
            "busy-poll,file-poll-100us",
            "--rounds",
            "3",
            "--lines",
            "120",
        ])
        .args([
            "--warmup",
            "20",
            "--interval-us",
            "30",
            "--no-pin",
            "--seed",
            "42",
            "--tail-dir",
        ])
        .arg(tmp("cli-tail"))
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8(o.stdout).unwrap();
    for want in [
        "seed: 42",
        "round 1 order: ",
        "round 3 order: ",
        "loadavg before: ",
        "loadavg after: ",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    let first_row = text.lines().find(|l| l.contains("n=300")).unwrap();
    assert!(
        first_row.contains(" sched-t2 "),
        "headline row first: {first_row}"
    );
    assert!(text.contains("headline: sched-t2"), "{text}");
    for arm in ["busy-poll", "file-poll-100us"] {
        assert!(out.join(format!("{arm}.sched-t2.hdr")).exists());
    }
    // The seed is validated like every other number.
    let bad = Command::new(env!("CARGO_BIN_EXE_hl-replay"))
        .args(["--seed", "x"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
}
