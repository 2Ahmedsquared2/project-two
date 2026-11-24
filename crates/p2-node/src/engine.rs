//! The engine contract the replay driver needs.
//!
//! Defined here, in lane 3, until lane 1's `hl-exec` publishes its own types;
//! the adapter is then one `impl Engine for hl_exec::...`. The engine sees
//! what a block carries (signed action, nonce, signer) and never native's
//! answer.

use hl_node_convert::node::BookDiff;
use serde_json::Value;

/// One action as the block carries it, plus the signer native resolved
/// (recovering it from the signature is engine work this contract does not
/// require yet).
#[derive(Clone, Copy, Debug)]
pub struct UserAction<'a> {
    /// `action.type`.
    pub action_type: &'a str,
    /// The whole action object.
    pub action: &'a Value,
    /// Action nonce.
    pub nonce: u64,
    /// Signer address (lowercase hex).
    pub user: &'a str,
    /// `vaultAddress`, when present.
    pub vault_address: Option<&'a str>,
    /// `expiresAfter`, ms, when present.
    pub expires_after: Option<u64>,
}

/// What the engine produced for one action.
#[derive(Clone, Debug, PartialEq)]
pub enum ActionResult {
    /// The `res` object, in native's JSON shape:
    /// `{"status":"ok","response":{..}}` or `{"status":"err","response":".."}`.
    Response(Value),
    /// The engine does not execute this action type. Never counts as reproduced.
    Unsupported,
}

/// What the engine produced for one block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockOutcome {
    /// One per input action, in order.
    pub results: Vec<ActionResult>,
    /// Book changes in application order, in native's raw book diff schema.
    pub book_diffs: Vec<BookDiff>,
}

/// An execution engine under test.
pub trait Engine {
    /// Executes one block's actions at `block_time_ns`.
    fn execute_block(&mut self, block_time_ns: u64, actions: &[UserAction<'_>]) -> BlockOutcome;
    /// Called after a mismatch at `height`: bring the book in line with
    /// native's diffs for that block so later blocks are compared from
    /// native's state.
    fn resync(&mut self, height: u64, native_diffs: &[BookDiff]);
}
