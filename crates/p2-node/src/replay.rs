//! Per-block differential replay against native output.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::BufRead;

use hl_block::census::response_classes;
use hl_block::{Block, Outcome};
use hl_node_convert::node::{BlockBatches, BookDiff, NodeError};
use serde_json::Value;

use crate::engine::{ActionResult, BlockOutcome, Engine, UserAction};

/// Native `node_raw_book_diffs` (one batch per block) keyed by height.
pub fn load_native_diffs<R: BufRead>(
    r: R,
) -> Result<HashMap<u64, (u64, Vec<BookDiff>)>, NodeError> {
    let mut m = HashMap::new();
    for b in BlockBatches::<R, BookDiff>::new(r) {
        let b = b?;
        m.insert(b.block_number, (b.block_time_ms, b.events));
    }
    Ok(m)
}

/// A block id that does not line up with the native diffs offered for it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "height {height}: replica block time {block_ms} ms but native diffs block time {diffs_ms} ms"
)]
pub struct Misaligned {
    /// Height.
    pub height: u64,
    /// Block time from `replica_cmds`, ms.
    pub block_ms: u64,
    /// Block time from the diffs line, ms.
    pub diffs_ms: u64,
}

/// One block ready to replay: the block, its height, native's responses and
/// native's book diffs.
#[derive(Clone, Debug)]
pub struct ReplayBlock {
    height: u64,
    block: Block,
    native_responses: Vec<Value>,
    native_diffs: Option<Vec<BookDiff>>,
}

impl ReplayBlock {
    /// Pairs a block with its height and native diffs. The diffs line's block
    /// time must equal the block's time truncated to ms (the replica record
    /// has no height field, so this is the alignment check).
    pub fn new(
        height: u64,
        block: Block,
        diffs: Option<(u64, Vec<BookDiff>)>,
    ) -> Result<Self, Misaligned> {
        let block_ms = block.time_ns / 1_000_000;
        let native_diffs = match diffs {
            Some((diffs_ms, _)) if diffs_ms != block_ms => {
                return Err(Misaligned {
                    height,
                    block_ms,
                    diffs_ms,
                })
            }
            Some((_, d)) => Some(d),
            None => None,
        };
        let native_responses = block.actions().map(|a| a.outcome.to_json()).collect();
        Ok(Self {
            height,
            block,
            native_responses,
            native_diffs,
        })
    }

    /// Height.
    #[must_use]
    pub fn height(&self) -> u64 {
        self.height
    }

    /// The block.
    #[must_use]
    pub fn block(&self) -> &Block {
        &self.block
    }

    /// Native responses, one per action.
    #[must_use]
    pub fn native_responses(&self) -> &[Value] {
        &self.native_responses
    }

    /// Native book diffs (empty when absent).
    #[must_use]
    pub fn native_diffs(&self) -> &[BookDiff] {
        self.native_diffs.as_deref().unwrap_or(&[])
    }

    /// The engine's input: what the block carries.
    #[must_use]
    pub fn user_actions(&self) -> Vec<UserAction<'_>> {
        self.block
            .actions()
            .map(|a| UserAction {
                action_type: &a.action_type,
                action: &a.action,
                nonce: a.nonce,
                user: &a.user,
                vault_address: a.vault_address.as_deref(),
                expires_after: a.expires_after,
            })
            .collect()
    }
}

/// Input the driver refuses before running anything (lead planner's release
/// gates F1 and F2, from Proof's TLA+ model of this driver).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    /// F1: blocks must be exactly `snapshot+1, snapshot+2, ...` in order.
    #[error("height: block {index} is height {got}, expected {expected} (snapshot {snapshot}); gap, repeat, swap or at/below snapshot")]
    Height {
        /// Index in the input.
        index: usize,
        /// Height found.
        got: u64,
        /// Height required.
        expected: u64,
        /// Snapshot height.
        snapshot: u64,
    },
    /// F2: no native diffs for this height. Absent is not empty: the block can
    /// be neither compared nor resynced, so the run stops before it. Replay a
    /// range that ends at the last height with native diffs.
    #[error(
        "no native book diffs for height {height}: refusing to run past the last block with diffs"
    )]
    NoNativeDiffs {
        /// Height.
        height: u64,
    },
}

/// Checks F1 and F2 for the whole input before anything runs.
pub fn validate(blocks: &[ReplayBlock], snapshot: u64) -> Result<(), ReplayError> {
    for (index, b) in blocks.iter().enumerate() {
        let expected = snapshot + 1 + u64::try_from(index).unwrap_or(u64::MAX);
        if b.height != expected {
            return Err(ReplayError::Height {
                index,
                got: b.height,
                expected,
                snapshot,
            });
        }
        if b.native_diffs.is_none() {
            return Err(ReplayError::NoNativeDiffs { height: b.height });
        }
    }
    Ok(())
}

/// Why a block was not reproduced. A block counts once, under its first cause.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cause {
    /// The engine returned a different number of results than actions.
    ResultCount,
    /// The engine does not execute this action type.
    Unsupported {
        /// `action.type`.
        action_type: String,
    },
    /// A response differs; grouped by action type and native's response class.
    Response {
        /// `action.type`.
        action_type: String,
        /// Native's response class (first class for multi-status responses).
        native_class: String,
    },
    /// Book diffs differ.
    BookDiffs,
}

/// The first mismatching block, for debugging.
#[derive(Clone, Debug)]
pub struct FirstMismatch {
    /// Height.
    pub height: u64,
    /// Cause.
    pub cause: Cause,
    /// Native's value (response or diffs) at the first difference.
    pub native: String,
    /// The engine's value there.
    pub ours: String,
}

/// Result of one replay pass.
#[derive(Clone, Debug, Default)]
pub struct Replay {
    /// Snapshot height the engine started from.
    pub snapshot_height: u64,
    /// First and last height replayed.
    pub first_height: Option<u64>,
    /// Last height replayed.
    pub last_height: Option<u64>,
    /// Blocks containing an action the engine returned `Unsupported` for.
    pub blocks_with_unsupported: u64,
    /// Blocks mismatched and resynced from native diffs.
    pub resynced: u64,
    /// Whether the end-state check against native's translated snapshot ran
    /// (set by the caller; without it the verdict is "per-block outputs
    /// matched", never "state identical").
    pub end_state: Option<bool>,
    /// N: blocks attempted.
    pub blocks: u64,
    /// Blocks reproduced exactly.
    pub reproduced: u64,
    /// Heights reproduced exactly (timing is reported over these only).
    pub reproduced_heights: Vec<u64>,
    /// Actions seen.
    pub actions: u64,
    /// Actions the engine executed (did not return `Unsupported`).
    pub actions_executed: u64,
    /// Non-reproduced blocks per first cause.
    pub causes: BTreeMap<Cause, u64>,
    /// The first mismatch.
    pub first_mismatch: Option<FirstMismatch>,
}

fn native_class(o: &Outcome) -> String {
    response_classes(o).into_iter().next().unwrap_or_default()
}

/// Compares one block's outcome with native's. `None` = reproduced.
#[must_use]
pub fn compare(b: &ReplayBlock, out: &BlockOutcome) -> Option<FirstMismatch> {
    let mk = |cause, native: String, ours: String| {
        Some(FirstMismatch {
            height: b.height,
            cause,
            native,
            ours,
        })
    };
    if out.results.len() != b.native_responses.len() {
        return mk(
            Cause::ResultCount,
            b.native_responses.len().to_string(),
            out.results.len().to_string(),
        );
    }
    for ((a, native), ours) in b.block.actions().zip(&b.native_responses).zip(&out.results) {
        match ours {
            ActionResult::Unsupported => {
                return mk(
                    Cause::Unsupported {
                        action_type: a.action_type.clone(),
                    },
                    native.to_string(),
                    "unsupported".into(),
                );
            }
            ActionResult::Response(v) if v != native => {
                let cause = Cause::Response {
                    action_type: a.action_type.clone(),
                    native_class: native_class(&a.outcome),
                };
                return mk(cause, native.to_string(), v.to_string());
            }
            ActionResult::Response(_) => {}
        }
    }
    let nd = b.native_diffs();
    if nd != out.book_diffs.as_slice() {
        let i = nd
            .iter()
            .zip(&out.book_diffs)
            .position(|(x, y)| x != y)
            .unwrap_or(nd.len().min(out.book_diffs.len()));
        return mk(
            Cause::BookDiffs,
            format!("{:?}", nd.get(i)),
            format!("{:?}", out.book_diffs.get(i)),
        );
    }
    None
}

impl Replay {
    /// Replays `blocks` (heights `snapshot+1..`) in order through `engine`,
    /// resyncing after a mismatch. Refuses the input up front on F1/F2.
    pub fn run<E: Engine>(
        engine: &mut E,
        blocks: &[ReplayBlock],
        snapshot: u64,
    ) -> Result<Self, ReplayError> {
        validate(blocks, snapshot)?;
        let mut r = Replay::starting_at(snapshot);
        for b in blocks {
            let acts = b.user_actions();
            let out = engine.execute_block(b.block.time_ns, &acts);
            r.record(b, &out, engine);
        }
        Ok(r)
    }

    /// An empty report for a run from `snapshot`.
    #[must_use]
    pub fn starting_at(snapshot: u64) -> Self {
        Replay {
            snapshot_height: snapshot,
            ..Replay::default()
        }
    }

    /// Records one block's outcome (used by the timed runner too).
    pub fn record<E: Engine>(&mut self, b: &ReplayBlock, out: &BlockOutcome, engine: &mut E) {
        self.blocks += 1;
        self.first_height.get_or_insert(b.height);
        self.last_height = Some(b.height);
        if out
            .results
            .iter()
            .any(|x| matches!(x, ActionResult::Unsupported))
        {
            self.blocks_with_unsupported += 1;
        }
        self.actions += u64::try_from(b.native_responses.len()).unwrap_or(u64::MAX);
        self.actions_executed += u64::try_from(
            out.results
                .iter()
                .filter(|x| matches!(x, ActionResult::Response(_)))
                .count(),
        )
        .unwrap_or(u64::MAX);
        match compare(b, out) {
            None => {
                self.reproduced += 1;
                self.reproduced_heights.push(b.height);
            }
            Some(m) => {
                *self.causes.entry(m.cause.clone()).or_insert(0) += 1;
                if self.first_mismatch.is_none() {
                    self.first_mismatch = Some(m);
                }
                self.resynced += 1;
                engine.resync(b.height, b.native_diffs());
            }
        }
    }

    /// Human-readable report.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let verdict = match self.end_state {
            Some(true) if self.reproduced == self.blocks => "state identical",
            _ => "per-block outputs matched",
        };
        let _ = writeln!(s, "{verdict} {}/{}", self.reproduced, self.blocks);
        let _ = writeln!(
            s,
            "snapshot {}, heights {}..={}",
            self.snapshot_height,
            self.first_height.map_or("-".into(), |h| h.to_string()),
            self.last_height.map_or("-".into(), |h| h.to_string())
        );
        let _ = writeln!(
            s,
            "blocks with an unsupported action {}, mismatched and resynced {}",
            self.blocks_with_unsupported, self.resynced
        );
        let _ = writeln!(
            s,
            "end-state check: {}",
            match self.end_state {
                None => "not run",
                Some(true) => "final book equals native's",
                Some(false) => "final book DIFFERS from native's",
            }
        );
        let _ = writeln!(
            s,
            "actions executed {}/{}",
            self.actions_executed, self.actions
        );
        let mut causes: Vec<_> = self.causes.iter().collect();
        causes.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (c, n) in causes {
            let label = match c {
                Cause::ResultCount => "result count".to_owned(),
                Cause::Unsupported { action_type } => format!("unsupported {action_type}"),
                Cause::Response {
                    action_type,
                    native_class,
                } => format!("response {action_type} / {native_class}"),
                Cause::BookDiffs => "book diffs".to_owned(),
            };
            let _ = writeln!(s, "{n:>10}  {label}");
        }
        if let Some(m) = &self.first_mismatch {
            let _ = writeln!(
                s,
                "first mismatch at {}: {:?}\n  native: {}\n  ours:   {}",
                m.height, m.cause, m.native, m.ours
            );
        }
        s
    }
}
