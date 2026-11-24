//! Replay driver: per-block comparison against native responses and book
//! diffs, resync on mismatch, report grouped by cause.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_block::decode_block;
use hl_node_convert::node::{parse_batch, BookDiff};
use p2_node::{
    engine::{ActionResult, BlockOutcome, Engine, UserAction},
    replay::{load_native_diffs, Cause, Replay, ReplayBlock, ReplayError},
};
use serde_json::Value;

const BLOCKS: &str = include_str!("data/three_blocks.jsonl");
const DIFFS: &str = include_str!("data/diffs_1000.jsonl");

fn input() -> Vec<ReplayBlock> {
    let diffs = load_native_diffs(DIFFS.as_bytes()).unwrap();
    BLOCKS
        .lines()
        .enumerate()
        .map(|(i, l)| {
            let h = 1000 + u64::try_from(i).unwrap();
            ReplayBlock::new(h, decode_block(l).unwrap(), diffs.get(&h).cloned()).unwrap()
        })
        .collect()
}

/// Answers from a script: per block, per action, either native's response
/// (cheating on purpose, to test the comparator) or a mutation.
struct Scripted {
    native: Vec<(Vec<Value>, Vec<BookDiff>)>,
    mutate: Option<(usize, Mutation)>,
    block: usize,
    resyncs: Vec<u64>,
}

#[derive(Clone, Copy)]
enum Mutation {
    Response,
    DropDiff,
    Unsupported,
    ExtraResponse,
    /// Three extra `Response` results (CI/CD #8: "actions executed 6/3").
    ExtraOk,
}

impl Scripted {
    fn new(input: &[ReplayBlock], mutate: Option<(usize, Mutation)>) -> Self {
        let native = input
            .iter()
            .map(|b| (b.native_responses().to_vec(), b.native_diffs().to_vec()))
            .collect();
        Self {
            native,
            mutate,
            block: 0,
            resyncs: Vec::new(),
        }
    }
}

impl Engine for Scripted {
    fn execute_block(&mut self, _block_time_ns: u64, actions: &[UserAction<'_>]) -> BlockOutcome {
        let (resps, diffs) = self.native[self.block].clone();
        assert_eq!(actions.len(), resps.len());
        let mut out = BlockOutcome {
            results: resps.into_iter().map(ActionResult::Response).collect(),
            book_diffs: diffs,
        };
        if let Some((b, m)) = self.mutate {
            if b == self.block {
                match m {
                    Mutation::Response => {
                        out.results[0] = ActionResult::Response(
                            serde_json::json!({"status":"err","response":"nope"}),
                        );
                    }
                    Mutation::DropDiff => {
                        out.book_diffs.pop();
                    }
                    Mutation::Unsupported => out.results[0] = ActionResult::Unsupported,
                    Mutation::ExtraResponse => out.results.push(ActionResult::Unsupported),
                    Mutation::ExtraOk => {
                        for _ in 0..3 {
                            out.results
                                .push(ActionResult::Response(serde_json::json!({"status":"ok"})));
                        }
                    }
                }
            }
        }
        self.block += 1;
        out
    }

    fn resync(&mut self, height: u64, _native_diffs: &[BookDiff]) {
        self.resyncs.push(height);
    }
}

#[test]
fn engine_never_sees_native_responses() {
    // UserAction carries what a block carries, not what native answered.
    let blocks = input();
    let acts = blocks[0].user_actions();
    assert_eq!(acts.len(), 2);
    assert_eq!(acts[0].action_type, "order");
    assert_eq!(
        acts[0].user,
        Some("0xecececececececececececececececececececec")
    );
    assert_eq!(
        acts[1].vault_address,
        Some("0x0101010101010101010101010101010101010101")
    );
}

#[test]
fn exact_engine_reproduces_every_block() {
    let blocks = input();
    let mut e = Scripted::new(&blocks, None);
    let r = Replay::run(&mut e, &blocks, 999).unwrap();
    assert_eq!((r.reproduced, r.blocks), (3, 3));
    assert!(r.causes.is_empty());
    assert_eq!(e.resyncs.len(), 0);
    assert_eq!(r.reproduced_heights, vec![1000, 1001, 1002]);
}

#[test]
fn response_mismatch_counts_and_resyncs() {
    let blocks = input();
    let mut e = Scripted::new(&blocks, Some((0, Mutation::Response)));
    let r = Replay::run(&mut e, &blocks, 999).unwrap();
    assert_eq!((r.reproduced, r.blocks), (2, 3));
    assert_eq!(
        r.causes.get(&Cause::Response {
            action_type: "order".into(),
            native_class: "ok:order:filled".into()
        }),
        Some(&1)
    );
    assert_eq!(e.resyncs, vec![1000]);
    assert_eq!(r.first_mismatch.as_ref().unwrap().height, 1000);
}

#[test]
fn book_diff_mismatch() {
    let blocks = input();
    let mut e = Scripted::new(&blocks, Some((0, Mutation::DropDiff)));
    let r = Replay::run(&mut e, &blocks, 999).unwrap();
    assert_eq!(r.reproduced, 2);
    assert_eq!(r.causes.get(&Cause::BookDiffs), Some(&1));
}

#[test]
fn unsupported_action_is_its_own_cause_and_coverage() {
    let blocks = input();
    let mut e = Scripted::new(&blocks, Some((2, Mutation::Unsupported)));
    let r = Replay::run(&mut e, &blocks, 999).unwrap();
    assert_eq!(r.reproduced, 2);
    assert_eq!(
        r.causes.get(&Cause::Unsupported {
            action_type: "brandNewAction".into()
        }),
        Some(&1)
    );
    assert_eq!((r.actions_executed, r.actions), (2, 3));
}

#[test]
fn result_count_mismatch() {
    let blocks = input();
    let mut e = Scripted::new(&blocks, Some((1, Mutation::ExtraResponse)));
    let r = Replay::run(&mut e, &blocks, 999).unwrap();
    assert_eq!(r.causes.get(&Cause::ResultCount), Some(&1));
}

#[test]
fn f2_missing_native_diffs_stops_the_run() {
    // Absent is not empty: with no native diffs the block cannot be compared
    // or resynced, so the run stops there and nothing after it counts.
    let mut blocks = input();
    let b = decode_block(BLOCKS.lines().nth(1).unwrap()).unwrap();
    blocks[1] = ReplayBlock::new(1001, b, None).unwrap();
    let mut e = Scripted::new(&blocks, None);
    let err = Replay::run(&mut e, &blocks, 999).unwrap_err();
    assert!(
        matches!(err, ReplayError::NoNativeDiffs { height: 1001 }),
        "{err}"
    );
    assert_eq!(e.block, 0, "engine must not run when the input is refused");
}

#[test]
fn f1_gap_repeat_swap_or_at_snapshot_is_refused() {
    let base = input();
    let mk = |order: &[usize], heights: &[u64]| -> Vec<ReplayBlock> {
        order
            .iter()
            .zip(heights)
            .map(|(&i, &h)| {
                let mut v: serde_json::Value =
                    serde_json::from_str(BLOCKS.lines().nth(i).unwrap()).unwrap();
                let _ = &mut v;
                let diffs = load_native_diffs(DIFFS.as_bytes()).unwrap();
                let d = diffs
                    .get(&(1000 + u64::try_from(i).unwrap()))
                    .cloned()
                    .map(|(_, d)| (base[i].block().time_ns / 1_000_000, d));
                ReplayBlock::new(h, decode_block(BLOCKS.lines().nth(i).unwrap()).unwrap(), d)
                    .unwrap()
            })
            .collect()
    };
    let cases: [(&[usize], &[u64], u64); 4] = [
        (&[0, 2], &[1000, 1002], 999),  // gap
        (&[0, 0], &[1000, 1000], 999),  // repeat
        (&[1, 0], &[1001, 1000], 999),  // swap
        (&[0, 1], &[1000, 1001], 1000), // first block at the snapshot height
    ];
    for (order, heights, snap) in cases {
        let blocks = mk(order, heights);
        let mut e = Scripted::new(&blocks, None);
        let err = Replay::run(&mut e, &blocks, snap).unwrap_err();
        assert!(
            matches!(err, ReplayError::Height { .. }),
            "{order:?} {heights:?}: {err}"
        );
        assert_eq!(e.block, 0, "engine must not run when the input is refused");
    }
}

#[test]
fn report_carries_audit_fields() {
    let blocks = input();
    let mut e = Scripted::new(&blocks, Some((2, Mutation::Unsupported)));
    let r = Replay::run(&mut e, &blocks, 999).unwrap();
    assert_eq!(
        (r.snapshot_height, r.first_height, r.last_height),
        (999, Some(1000), Some(1002))
    );
    assert_eq!(r.blocks_with_unsupported, 1);
    assert_eq!(r.resynced, 1);
    let t = r.to_text();
    assert!(t.contains("snapshot 999, heights 1000..=1002"), "{t}");
    assert!(t.contains("end-state check: not run"), "{t}");
    assert!(t.contains("per-block outputs matched 2/3"), "{t}");
}

#[test]
fn alignment_checked_by_block_time() {
    // Height 1001's diffs carry block time ...58.070; block 0 is ...57.997.
    let diffs = load_native_diffs(DIFFS.as_bytes()).unwrap();
    let b0 = decode_block(BLOCKS.lines().next().unwrap()).unwrap();
    let e = ReplayBlock::new(1001, b0, diffs.get(&1001).cloned()).unwrap_err();
    assert!(e.to_string().contains("block time"), "{e}");
}

#[test]
fn report_text() {
    let blocks = input();
    let mut e = Scripted::new(&blocks, Some((0, Mutation::Response)));
    let r = Replay::run(&mut e, &blocks, 999).unwrap();
    let t = r.to_text();
    assert!(t.contains("per-block outputs matched 2/3"), "{t}");
    assert!(t.contains("response order / ok:order:filled"), "{t}");
}

#[test]
fn native_diff_fixture_parses_with_converter_schema() {
    let b = parse_batch::<BookDiff>(DIFFS.lines().next().unwrap()).unwrap();
    assert_eq!(b.block_number, 1000);
}

/// CI/CD #2: a block whose decoded height differs from the height it is
/// replayed as is refused, even when its block time lines up.
#[test]
fn decoded_height_must_equal_replayed_height() {
    let diffs = load_native_diffs(DIFFS.as_bytes()).unwrap();
    let blocks: Vec<ReplayBlock> = BLOCKS
        .lines()
        .enumerate()
        .map(|(i, l)| {
            let h = 1000 + u64::try_from(i).unwrap();
            let mut b = decode_block(l).unwrap();
            b.height = Some(77);
            ReplayBlock::new(h, b, diffs.get(&h).cloned()).unwrap()
        })
        .collect();
    let mut e = Scripted::new(&blocks, None);
    let err = Replay::run(&mut e, &blocks, 999).unwrap_err();
    assert!(
        matches!(
            err,
            ReplayError::DecodedHeight {
                height: 1000,
                decoded: 77
            }
        ),
        "{err}"
    );
}

/// CI/CD #8: extra engine results never count as executed actions.
#[test]
fn extra_results_never_exceed_actions() {
    let blocks = input();
    let mut e = Scripted::new(&blocks, Some((1, Mutation::ExtraOk)));
    let r = Replay::run(&mut e, &blocks, 999).unwrap();
    assert!(r.actions_executed <= r.actions, "{}", r.to_text());
}
