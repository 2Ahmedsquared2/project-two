//! Timed passes: only blocks reproduced in that pass are timed, warm-up
//! samples are excluded, every pass starts from a fresh engine, worst case is
//! a range over passes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use hl_node_convert::node::BookDiff;
use p2_node::engine::{ActionResult, BlockOutcome, Engine, UserAction};
use p2_node::replay::{load_native_diffs, ReplayBlock};
use p2_node::timing::{percentile, run_passes, TimingConfig};
use serde_json::Value;

const BLOCKS: &str = include_str!("data/three_blocks.jsonl");
const DIFFS: &str = include_str!("data/diffs_1000.jsonl");

fn input() -> (Vec<String>, Vec<ReplayBlock>) {
    let diffs = load_native_diffs(DIFFS.as_bytes()).unwrap();
    let lines: Vec<String> = BLOCKS.lines().map(str::to_owned).collect();
    let blocks = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let h = 1000 + u64::try_from(i).unwrap();
            ReplayBlock::new(
                h,
                hl_block::decode_block(l).unwrap(),
                diffs.get(&h).cloned(),
            )
            .unwrap()
        })
        .collect();
    (lines, blocks)
}

/// Answers with native's data (so it reproduces) and spins `spin` per block;
/// block index `wrong` answers wrong.
struct Spinner {
    native: Vec<(Vec<Value>, Vec<BookDiff>)>,
    i: usize,
    spin: Duration,
    wrong: Option<usize>,
}

impl Engine for Spinner {
    fn execute_block(&mut self, _t: u64, _a: &[UserAction<'_>]) -> BlockOutcome {
        let t = Instant::now();
        while t.elapsed() < self.spin {}
        let (r, d) = self.native[self.i % self.native.len()].clone();
        let mut results: Vec<ActionResult> = r.into_iter().map(ActionResult::Response).collect();
        if self.wrong == Some(self.i % self.native.len()) {
            results = results
                .into_iter()
                .map(|_| ActionResult::Unsupported)
                .collect();
        }
        self.i += 1;
        BlockOutcome {
            results,
            book_diffs: d,
        }
    }
    fn resync(&mut self, _h: u64, _d: &[BookDiff], _r: &[Value]) -> Result<(), String> {
        Ok(())
    }
}

fn factory(
    blocks: &[ReplayBlock],
    spin: Duration,
    wrong: Option<usize>,
) -> impl FnMut() -> Spinner + '_ {
    move || Spinner {
        native: blocks
            .iter()
            .map(|b| (b.native_responses().to_vec(), b.native_diffs().to_vec()))
            .collect(),
        i: 0,
        spin,
        wrong,
    }
}

#[test]
fn percentile_nearest_rank() {
    let v = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    assert_eq!(percentile(&v, 500), Some(5));
    assert_eq!(percentile(&v, 900), Some(9));
    assert_eq!(percentile(&v, 990), Some(10));
    assert_eq!(percentile(&v, 1000), Some(10));
    assert_eq!(percentile(&[], 500), None);
    assert_eq!(percentile(&[7], 1), Some(7));
}

#[test]
fn passes_time_only_reproduced_blocks_and_exclude_warmup() {
    let (lines, blocks) = input();
    let cfg = TimingConfig {
        passes: 5,
        prewarm_passes: 2,
        window_blocks: 1,
    };
    let r = run_passes(
        &mut factory(&blocks, Duration::from_micros(200), Some(2)),
        &lines,
        &blocks,
        999,
        &cfg,
    )
    .unwrap();
    assert_eq!(r.passes.len(), 5);
    for p in &r.passes {
        assert_eq!(p.reproduced, 2); // block index 2 answered wrong
                                     // window_blocks = 1: the first block of each pass is warm-up.
        assert_eq!(p.excluded_warmup, 1);
        assert_eq!(p.exec_ns.len(), 1); // 3 blocks - 1 warm-up - 1 not reproduced
        assert!(p.exec_ns[0] >= 200_000);
        assert!(p.parse_exec_ns[0] >= p.exec_ns[0]);
    }
    assert_eq!(r.outputs_matched, (2, 3));
    let s = r.summary();
    assert_eq!(s.samples, 5);
    assert!(s.exec.p50 >= 200_000);
    assert!(s.exec.worst_min <= s.exec.worst_max);
}

#[test]
fn fewer_than_five_passes_rejected() {
    let (lines, blocks) = input();
    let cfg = TimingConfig {
        passes: 4,
        prewarm_passes: 1,
        window_blocks: 0,
    };
    assert!(run_passes(
        &mut factory(&blocks, Duration::ZERO, None),
        &lines,
        &blocks,
        999,
        &cfg
    )
    .is_err());
}

#[test]
fn line_count_must_match_blocks() {
    let (mut lines, blocks) = input();
    lines.pop();
    let cfg = TimingConfig {
        passes: 5,
        prewarm_passes: 0,
        window_blocks: 0,
    };
    assert!(run_passes(
        &mut factory(&blocks, Duration::ZERO, None),
        &lines,
        &blocks,
        999,
        &cfg
    )
    .is_err());
}

#[test]
fn state_identical_reports_worst_pass() {
    // Reproduction is deterministic here, so every pass agrees; the summary
    // reports the minimum over passes.
    let (lines, blocks) = input();
    let cfg = TimingConfig {
        passes: 5,
        prewarm_passes: 0,
        window_blocks: 0,
    };
    let r = run_passes(
        &mut factory(&blocks, Duration::ZERO, None),
        &lines,
        &blocks,
        999,
        &cfg,
    )
    .unwrap();
    assert_eq!(r.outputs_matched, (3, 3));
    let t = r.summary().to_text();
    // CI/CD #4 / planner A: no end-state evidence here, so never "state identical".
    assert!(t.contains("per-block outputs matched 3/3"), "{t}");
    assert!(!t.contains("state identical"), "{t}");
    assert!(t.contains("median"), "{t}");
    assert!(t.contains("worst"), "{t}");
}

#[test]
fn csv_has_one_row_per_timed_block() {
    let (lines, blocks) = input();
    let cfg = TimingConfig {
        passes: 5,
        prewarm_passes: 0,
        window_blocks: 0,
    };
    let r = run_passes(
        &mut factory(&blocks, Duration::ZERO, None),
        &lines,
        &blocks,
        999,
        &cfg,
    )
    .unwrap();
    let csv = r.to_csv();
    let mut it = csv.lines();
    assert_eq!(
        it.next(),
        Some("pass,height,phase,reproduced,actions,exec_ns,parse_exec_ns")
    );
    assert_eq!(it.count(), 15);
}

#[test]
fn f1_timed_line_must_be_the_compared_block() {
    // Proof's side note: the line executed must be the block compared, not
    // just the same count. Swap two raw lines: same length, wrong block.
    let (mut lines, blocks) = input();
    lines.swap(0, 1);
    let cfg = TimingConfig {
        passes: 5,
        prewarm_passes: 0,
        window_blocks: 0,
    };
    let e = run_passes(
        &mut factory(&blocks, Duration::ZERO, None),
        &lines,
        &blocks,
        999,
        &cfg,
    )
    .unwrap_err();
    assert!(e.to_string().contains("not the block compared"), "{e}");
}

#[test]
fn f1_f2_apply_to_timed_passes() {
    let (lines, blocks) = input();
    let cfg = TimingConfig {
        passes: 5,
        prewarm_passes: 0,
        window_blocks: 0,
    };
    // snapshot at 1000: first block 1000 is at the snapshot height
    assert!(run_passes(
        &mut factory(&blocks, Duration::ZERO, None),
        &lines,
        &blocks,
        1000,
        &cfg
    )
    .is_err());
}

/// CI/CD #3: the timed line must be the compared block in full, not only its
/// time and round: a changed order price is refused.
#[test]
fn timed_line_with_changed_action_is_refused() {
    let (mut lines, blocks) = input();
    let i = lines
        .iter()
        .position(|l| l.contains("\"p\":\"0.038385\""))
        .unwrap();
    lines[i] = lines[i].replacen("\"p\":\"0.038385\"", "\"p\":\"0.038386\"", 1);
    let cfg = TimingConfig {
        passes: 5,
        prewarm_passes: 0,
        window_blocks: 0,
    };
    let e = run_passes(
        &mut factory(&blocks, Duration::ZERO, None),
        &lines,
        &blocks,
        999,
        &cfg,
    )
    .unwrap_err();
    assert!(e.to_string().contains("not the block compared"), "{e}");
}
