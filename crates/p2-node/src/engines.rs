//! Stand-in engines until lane 1's `hl-exec` lands. Neither is an execution
//! engine; their numbers are never a speed claim.

use hl_node_convert::node::BookDiff;
use serde_json::Value;

use crate::engine::{ActionResult, BlockOutcome, Engine, UserAction};

/// Executes nothing: every action is `Unsupported`, no book diffs. Measures
/// the driver's own overhead and gives honest coverage (0 actions).
#[derive(Debug, Default)]
pub struct NullEngine;

impl Engine for NullEngine {
    fn execute_block(&mut self, _t: u64, actions: &[UserAction<'_>]) -> BlockOutcome {
        BlockOutcome {
            results: vec![ActionResult::Unsupported; actions.len()],
            book_diffs: Vec::new(),
        }
    }
    fn resync(&mut self, _h: u64, _d: &[BookDiff]) {}
}

/// Test oracle: answers with native's own responses and diffs, handed to it
/// up front. Checks the comparator and the timing harness end to end; it
/// "reproduces" everything by construction.
#[derive(Debug)]
pub struct EchoEngine {
    native: Vec<(Vec<Value>, Vec<BookDiff>)>,
    i: usize,
}

impl EchoEngine {
    /// From native responses and diffs, in block order.
    #[must_use]
    pub fn new(native: Vec<(Vec<Value>, Vec<BookDiff>)>) -> Self {
        Self { native, i: 0 }
    }
}

impl Engine for EchoEngine {
    fn execute_block(&mut self, _t: u64, _a: &[UserAction<'_>]) -> BlockOutcome {
        let (r, d) = self.native.get(self.i).cloned().unwrap_or_default();
        self.i += 1;
        BlockOutcome {
            results: r.into_iter().map(ActionResult::Response).collect(),
            book_diffs: d,
        }
    }
    fn resync(&mut self, _h: u64, _d: &[BookDiff]) {}
}
