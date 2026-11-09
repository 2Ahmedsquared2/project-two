//! Golden vector: the hand-built dataset in `golden/` replays cleanly through the
//! reference book, and each deliberately corrupted copy is caught at the exact
//! block, side, price and field. Also exercises the `book-check` binary's exit codes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::Command;

use book_check::{check, CheckError, Divergence, Field};
use hl_book_ref::{Book, Lots, Oid, Px, Side};
use hl_dataset::Reader;

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../golden")
        .join(name)
}

fn run(name: &str) -> Result<book_check::Report, CheckError> {
    let reader = Reader::new(BufReader::new(File::open(golden(name)).unwrap())).unwrap();
    check(reader, |_| Book::new())
}

fn divergence(name: &str) -> Divergence {
    match run(name).unwrap_err() {
        CheckError::Divergence(d) => d,
        other => panic!("{name}: expected a divergence, got {other:?}"),
    }
}

#[test]
fn the_golden_dataset_replays_cleanly() {
    let report = run("basic.jsonl").unwrap();
    assert_eq!(report.blocks, 6);
    assert_eq!(report.l4_books_compared, 6);
    assert_eq!(report.ops, 10);
}

#[test]
fn a_corrupted_size_is_caught() {
    assert_eq!(
        divergence("basic-corrupt-size.jsonl"),
        Divergence {
            height: 4,
            asset: 0,
            coin: "GOLD".to_owned(),
            side: Side::Bid,
            px: Px(1000),
            field: Field::Size {
                recorded: Lots(250),
                candidate: Lots(200),
            },
        }
    );
}

#[test]
fn a_corrupted_queue_order_is_caught() {
    assert_eq!(
        divergence("basic-corrupt-queue.jsonl"),
        Divergence {
            height: 2,
            asset: 0,
            coin: "GOLD".to_owned(),
            side: Side::Bid,
            px: Px(1000),
            field: Field::QueuePosition {
                position: 0,
                recorded: Oid(3),
                candidate: Oid(1),
            },
        }
    );
}

#[test]
fn a_missing_level_is_caught() {
    assert_eq!(
        divergence("basic-corrupt-level.jsonl"),
        Divergence {
            height: 5,
            asset: 0,
            coin: "GOLD".to_owned(),
            side: Side::Ask,
            px: Px(1005),
            field: Field::ExtraLevel {
                candidate: Lots(75),
                candidate_orders: 1,
            },
        }
    );
}

#[test]
fn the_l2_golden_dataset_replays_cleanly() {
    let report = run("l2.jsonl").unwrap();
    assert_eq!(report.blocks, 6);
    assert_eq!(report.l2_books_compared, 6);
    assert_eq!(report.l4_books_compared, 0);
}

#[test]
fn a_corrupted_l2_order_count_is_caught() {
    assert_eq!(
        divergence("l2-corrupt-count.jsonl"),
        Divergence {
            height: 3,
            asset: 0,
            coin: "GOLD".to_owned(),
            side: Side::Bid,
            px: Px(1000),
            field: Field::OrderCount {
                recorded: 1,
                candidate: 2,
            },
        }
    );
}

fn run_binary(name: &str) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_book-check"))
        .arg(golden(name))
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn the_binary_exits_zero_on_a_clean_replay() {
    let (code, stdout, stderr) = run_binary("basic.jsonl");
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("6 blocks"), "{stdout}");
}

#[test]
fn the_binary_exits_one_and_names_the_divergence() {
    let (code, stdout, stderr) = run_binary("basic-corrupt-size.jsonl");
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("block 4"), "{stderr}");
    assert!(stderr.contains("GOLD"), "{stderr}");
    assert!(stderr.contains("bid"), "{stderr}");
    assert!(stderr.contains("1000"), "{stderr}");
    assert!(stderr.contains("size"), "{stderr}");
}

#[test]
fn the_binary_exits_two_on_an_unreadable_dataset() {
    let out = Command::new(env!("CARGO_BIN_EXE_book-check"))
        .arg(golden("does-not-exist.jsonl"))
        .output()
        .unwrap();
    assert_eq!(out.status.code().unwrap(), 2);
}

// ---- Brief #2: raw-action replay golden vector (golden/replay.jsonl) ----

fn replay(
    name: &str,
    stp: bool,
) -> Result<book_check::ReplayReport, Box<book_check::ReplayFailure>> {
    let reader = Reader::new(BufReader::new(File::open(golden(name)).unwrap())).unwrap();
    let config = book_check::replay::ReplayConfig {
        self_trade_prevention: stp,
        ..Default::default()
    };
    book_check::check_replay(reader, config, true)
}

fn replay_divergence(name: &str, stp: bool) -> Divergence {
    match replay(name, stp).unwrap_err().error {
        CheckError::Divergence(d) => d,
        other => panic!("{name}: expected a divergence, got {other:?}"),
    }
}

#[test]
fn the_replay_golden_vector_exercises_every_brief2_rule_and_replays_cleanly() {
    let r = replay("replay.jsonl", true).unwrap();
    assert_eq!(r.compared.blocks, 6);
    assert_eq!(r.compared.l4_books_compared, 6);
    let s = &r.stats;
    assert_eq!(s.errored_txs_skipped, 1); // block 2: expired GTC changes nothing
    assert_eq!(s.legs_alo_rejected, 1); // block 2: crossing ALO rejected
    assert_eq!(s.fills, 4);
    assert_eq!(s.self_trade_cancels, 2); // block 4: C's own bids cancelled
    assert_eq!(s.modify_old_resolved, 1); // block 5: modify by cloid
    assert_eq!(s.cancel_by_cloid_resolved, 1); // block 6
}

#[test]
fn a_modify_that_kept_queue_position_is_caught() {
    assert_eq!(
        replay_divergence("replay-corrupt-modify-queue.jsonl", true),
        Divergence {
            height: 5,
            asset: 0,
            coin: "GOLD".to_owned(),
            side: Side::Ask,
            px: Px(990),
            field: Field::QueuePosition {
                position: 0,
                recorded: Oid(11),
                candidate: Oid(8),
            },
        }
    );
}

#[test]
fn replay_without_self_trade_prevention_diverges_at_the_self_trade_block() {
    assert_eq!(
        replay_divergence("replay.jsonl", false),
        Divergence {
            height: 4,
            asset: 0,
            coin: "GOLD".to_owned(),
            side: Side::Bid,
            px: Px(990),
            field: Field::ExtraLevel {
                candidate: Lots(250_000),
                candidate_orders: 2,
            },
        }
    );
}

#[test]
fn the_binary_replays_with_exit_codes() {
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_book-check"))
            .args(args)
            .output()
            .unwrap()
            .status
            .code()
            .unwrap()
    };
    let clean = golden("replay.jsonl");
    let bad = golden("replay-corrupt-modify-queue.jsonl");
    let clean = clean.to_str().unwrap();
    assert_eq!(run(&[clean, "--replay"]), 0);
    assert_eq!(run(&[bad.to_str().unwrap(), "--replay"]), 1);
    assert_eq!(run(&[clean, "--replay", "--no-stp"]), 1);
    assert_eq!(run(&[clean, "--no-stp"]), 2);
}
