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
    /// Signer address (lowercase hex); `None` when native resolved none.
    pub user: Option<&'a str>,
    /// `vaultAddress`, when present.
    pub vault_address: Option<&'a str>,
    /// `expiresAfter`, ms, when present.
    pub expires_after: Option<u64>,
    /// Index of the signed-action bundle the action came in, within its
    /// block. Native runs each bundle's actions in its own order (cancels
    /// before GTC/IOC orders), so an engine needs the bundle boundaries.
    pub bundle: u32,
}

impl<'a> UserAction<'a> {
    /// The engine inputs for one decoded block, in input order, each tagged
    /// with its bundle index.
    #[must_use]
    pub fn of_block(b: &'a hl_block::Block) -> Vec<Self> {
        b.bundles
            .iter()
            .enumerate()
            .flat_map(|(j, bd)| {
                let j = u32::try_from(j).unwrap_or(u32::MAX);
                bd.actions.iter().map(move |a| UserAction {
                    bundle: j,
                    ..UserAction::from_signed(a)
                })
            })
            .collect()
    }

    /// The engine input for one decoded action (bundle 0).
    #[must_use]
    pub fn from_signed(a: &'a hl_block::SignedAction) -> Self {
        UserAction {
            action_type: &a.action_type,
            action: &a.action,
            nonce: a.nonce,
            user: a.user.as_deref(),
            vault_address: a.vault_address.as_deref(),
            expires_after: a.expires_after,
            bundle: 0,
        }
    }
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
    /// Called after a mismatch at `height` (the block is already counted as
    /// NOT reproduced): adopt native's book changes for that block, and the
    /// oid counter and cloids its responses show, so later blocks are
    /// compared from native's state. An error means our book is no longer
    /// known to equal native's: the run stops.
    fn resync(
        &mut self,
        height: u64,
        native_diffs: &[BookDiff],
        native_responses: &[serde_json::Value],
    ) -> Result<(), String>;

    /// Nanoseconds of the engine core alone in the last `execute_block`
    /// (input conversion and response rendering excluded), when the engine
    /// measures it.
    fn core_exec_ns(&self) -> Option<u64> {
        None
    }

    /// The final book in canonical text form (per asset and side, resting
    /// orders in queue order, then the next oid), for the end-state check.
    fn final_book(&self) -> Option<String> {
        None
    }
}
