//! Proof (`ReplayCli.tla`, CLI1): the CLI labels replica line i as height
//! `snapshot + 1 + i`, so a dropped, repeated or swapped line must be refused by
//! `ReplayBlock::new`'s block-time check against native's diffs (exit 1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

use hl_block::decode_block;
use p2_node::replay::{load_native_diffs, validate, ReplayBlock, ReplayError};

const BLOCKS: &str = include_str!("data/three_blocks.jsonl");
const DIFFS: &str = include_str!("data/diffs_1000.jsonl");

fn blocks_1000_to_1002() -> Vec<ReplayBlock> {
    let diffs = load_native_diffs(DIFFS.as_bytes()).unwrap();
    BLOCKS
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            let h = 1000 + u64::try_from(i).unwrap();
            ReplayBlock::new(h, decode_block(l).unwrap(), diffs.get(&h).cloned()).unwrap()
        })
        .collect()
}

fn data(f: &str) -> String {
    format!("{}/tests/data/{f}", env!("CARGO_MANIFEST_DIR"))
}

fn replay_with_lines(name: &str, order: &[usize]) -> std::process::Output {
    replay_with(
        name,
        order,
        &std::fs::read_to_string(data("diffs_1000.jsonl")).unwrap(),
    )
}

fn replay_with(name: &str, order: &[usize], diffs: &str) -> std::process::Output {
    let src = std::fs::read_to_string(data("three_blocks.jsonl")).unwrap();
    let lines: Vec<&str> = src.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut body = order
        .iter()
        .map(|&i| lines[i])
        .collect::<Vec<_>>()
        .join("\n");
    body.push('\n');
    let f = std::env::temp_dir().join(format!("p2n-proof-{name}-{}.jsonl", std::process::id()));
    std::fs::write(&f, body).unwrap();
    let d = std::env::temp_dir().join(format!("p2n-proof-{name}-{}.diffs", std::process::id()));
    std::fs::write(&d, diffs).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_p2-node"))
        .args([
            "replay",
            "--blocks",
            f.to_str().unwrap(),
            "--diffs",
            d.to_str().unwrap(),
        ])
        .args([
            "--snapshot-height",
            "999",
            "--passes",
            "5",
            "--prewarm-passes",
            "0",
            "--window-blocks",
            "0",
        ])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&f);
    let _ = std::fs::remove_file(&d);
    o
}

#[test]
fn cli_refuses_dropped_repeated_or_swapped_replica_line() {
    // Heights by position: 1000, 1001, 1002 (snapshot 999).
    for (name, order) in [
        ("drop", &[0usize, 2][..]), // block 1001 dropped: line 1 (block 1002) labelled 1001
        ("drop-first", &[1, 2][..]), // block 1000 dropped
        ("repeat", &[0, 0, 1][..]), // block 1000 twice
        ("swap", &[1, 0, 2][..]),   // 1000 and 1001 swapped
    ] {
        let o = replay_with_lines(name, order);
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{name}: {err}");
        assert!(err.contains("block time"), "{name}: {err}");
    }
}

#[test]
fn cli_accepts_the_well_formed_file() {
    let o = replay_with_lines("ok", &[0, 1, 2]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn f1_first_block_above_snapshot_plus_one_is_refused() {
    // TLC trace F1/P1: engine at the snapshot, first block two above it (one block skipped).
    let err = validate(&blocks_1000_to_1002(), 998).unwrap_err();
    assert!(
        matches!(
            err,
            ReplayError::Height {
                index: 0,
                got: 1000,
                expected: 999,
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn f1_block_below_snapshot_is_refused() {
    // F1/P2: snapshot already past the first block.
    let err = validate(&blocks_1000_to_1002(), 1001).unwrap_err();
    assert!(
        matches!(
            err,
            ReplayError::Height {
                index: 0,
                got: 1000,
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn f2_missing_diffs_refused_on_the_timed_cli_path() {
    // F2 through run_passes (the CLI's path): native diffs for 1001 removed.
    let all = std::fs::read_to_string(data("diffs_1000.jsonl")).unwrap();
    let mut diffs = all
        .lines()
        .filter(|l| !l.contains("\"block_number\":1001"))
        .collect::<Vec<_>>()
        .join("\n");
    diffs.push('\n');
    assert_eq!(diffs.lines().count(), 2, "fixture shape changed");
    let o = replay_with("f2-timed", &[0, 1, 2], &diffs);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.contains("no native book diffs for height 1001"),
        "{err}"
    );
}
