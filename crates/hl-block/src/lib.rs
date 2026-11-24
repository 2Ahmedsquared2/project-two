//! Strict decoder for the block records `hl-node` writes to
//! `~/hl/data/replica_cmds/{start_time}/{date}/{height}` when run with
//! `--replica-cmds-style actions-and-responses`, plus an action [`census`].
//!
//! Schema source: the official `hyperliquid-dex/node` README (paths, flag) and
//! Dwellir's `StreamBlocks` "Full Block Spec", which documents the same
//! `replica_cmds` `BlockData`. PROVISIONAL until checked against a real capture:
//! that is why every envelope here rejects unknown fields, and an action
//! without a string `type` is an error. Action payloads and response data are
//! kept as raw JSON: the engine decodes the ones it executes, the census counts
//! all of them, so nothing is skipped silently.
//!
//! One line = one block:
//! `{"abci_block": {time, round, parent_round, proposer, hardfork,
//! signed_action_bundles: [[hash, {signed_actions, broadcaster,
//! broadcaster_nonce}]]}, "resps": {"Full": [[hash, [{user, res}]]]}}`.
//! Checked invariants: one response bundle per action bundle, same hashes in
//! the same order, one response per signed action.

use std::io::BufRead;

use serde::Deserialize;
use serde_json::Value;

pub mod census;
mod raw;
mod text;

pub use text::{normalize_message, parse_time_ns, TimeError};

/// One decoded block with every action paired to its native response.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// `HyperBFT` round of this block. Rounds can skip.
    pub round: u64,
    /// Round of the parent block (not always `round - 1`).
    pub parent_round: u64,
    /// Block time as written (naive UTC, up to nanoseconds).
    pub time: String,
    /// Block time, ns since the Unix epoch.
    pub time_ns: u64,
    /// Proposer address.
    pub proposer: String,
    /// Protocol version in force.
    pub hardfork_version: u64,
    /// Round at which that version activated.
    pub hardfork_round: u64,
    /// Bundles in block order.
    pub bundles: Vec<Bundle>,
}

impl Block {
    /// All actions of the block in execution order.
    pub fn actions(&self) -> impl Iterator<Item = &SignedAction> {
        self.bundles.iter().flat_map(|b| b.actions.iter())
    }
}

/// One signed action bundle.
#[derive(Clone, Debug, PartialEq)]
pub struct Bundle {
    /// Bundle hash (join key between actions and responses).
    pub hash: String,
    /// Who broadcast the bundle.
    pub broadcaster: String,
    /// Bundle-level nonce.
    pub broadcaster_nonce: u64,
    /// The actions, each with its native response.
    pub actions: Vec<SignedAction>,
}

/// ECDSA signature as written.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    /// `r`, hex.
    pub r: String,
    /// `s`, hex.
    pub s: String,
    /// Recovery id (27 or 28).
    pub v: u8,
}

/// One signed action and what native execution answered.
#[derive(Clone, Debug, PartialEq)]
pub struct SignedAction {
    /// `action.type`.
    pub action_type: String,
    /// The whole action object, raw.
    pub action: Value,
    /// Action nonce.
    pub nonce: u64,
    /// `vaultAddress`, when acting for a vault or sub-account.
    pub vault_address: Option<String>,
    /// `expiresAfter`, ms.
    pub expires_after: Option<u64>,
    /// Signature.
    pub signature: Signature,
    /// The signer native resolved (from the response).
    pub user: String,
    /// Native's response.
    pub outcome: Outcome,
}

/// Native execution response of one action.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// `{"status":"ok","response":{"type":..,"data":..}}`.
    Ok {
        /// `response.type`.
        response_type: String,
        /// `response.data`, raw, when present.
        data: Option<Value>,
    },
    /// `{"status":"err","response":"<message>"}`.
    Err(String),
}

/// Why a block line was rejected.
#[derive(Debug, thiserror::Error)]
pub enum BlockError {
    /// Not the expected JSON shape (includes unknown fields).
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Different number of response bundles and action bundles.
    #[error("{bundles} action bundles but {responses} response bundles")]
    BundleCount {
        /// Action bundles.
        bundles: usize,
        /// Response bundles.
        responses: usize,
    },
    /// Response bundle hash differs from the action bundle's.
    #[error("bundle {bundle}: action hash {action_hash} but response hash {response_hash}")]
    BundleHash {
        /// Bundle index.
        bundle: usize,
        /// Hash on the action side.
        action_hash: String,
        /// Hash on the response side.
        response_hash: String,
    },
    /// Different number of responses and actions in a bundle.
    #[error("bundle {bundle}: {actions} actions but {responses} responses")]
    ResponseCount {
        /// Bundle index.
        bundle: usize,
        /// Actions.
        actions: usize,
        /// Responses.
        responses: usize,
    },
    /// An action without a string `type`.
    #[error("bundle {bundle} action {action}: no string `type`")]
    ActionType {
        /// Bundle index.
        bundle: usize,
        /// Action index in the bundle.
        action: usize,
    },
    /// A response with a status other than ok/err, or a malformed body.
    #[error("bundle {bundle} action {action}: response: {why}")]
    Response {
        /// Bundle index.
        bundle: usize,
        /// Action index in the bundle.
        action: usize,
        /// What was wrong.
        why: String,
    },
    /// Unparseable block time.
    #[error("time {0:?}: not YYYY-MM-DDTHH:MM:SS[.fffffffff]")]
    Time(String),
}

/// Decodes one `replica_cmds` line.
pub fn decode_block(line: &str) -> Result<Block, BlockError> {
    let raw: raw::Line = serde_json::from_str(line)?;
    raw::into_block(raw)
}

/// A line that failed, with its 1-based line number.
#[derive(Debug, thiserror::Error)]
#[error("line {line}: {error}")]
pub struct LineError {
    /// 1-based line number in the input.
    pub line: u64,
    /// What failed.
    pub error: BlockError,
}

/// Iterates the blocks of a `replica_cmds` file. Blank lines are skipped; an
/// I/O error ends the iteration after it is reported once.
pub struct Reader<R> {
    inner: R,
    buf: String,
    line: u64,
    done: bool,
}

impl<R: BufRead> Reader<R> {
    /// Wraps a buffered reader.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buf: String::new(),
            line: 0,
            done: false,
        }
    }
}

impl<R: BufRead> Iterator for Reader<R> {
    type Item = Result<Block, LineError>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
            self.buf.clear();
            self.line += 1;
            match self.inner.read_line(&mut self.buf) {
                Ok(0) => self.done = true,
                Ok(_) => {
                    let l = self.buf.trim_end_matches(['\n', '\r']);
                    if l.trim().is_empty() {
                        continue;
                    }
                    return Some(decode_block(l).map_err(|error| LineError {
                        line: self.line,
                        error,
                    }));
                }
                Err(e) => {
                    self.done = true;
                    return Some(Err(LineError {
                        line: self.line,
                        error: BlockError::Json(serde_json::Error::io(e)),
                    }));
                }
            }
        }
        None
    }
}
