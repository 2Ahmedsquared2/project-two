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
