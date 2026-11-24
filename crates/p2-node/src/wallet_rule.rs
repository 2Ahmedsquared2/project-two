//! `--signers FILE`: lane 1's unknown-wallet rule, taken as it is on PR #11
//! (head 4154560), fed by the same file. Off by default.
//!
//! The rule itself is lane 1's: `signers.rs` is lane 1's file, included by
//! path, and [`WalletRule::decide`] transcribes `decode_block`'s rule block
//! in `crates/hl-exec/examples/replay.rs` at 4154560. Each L1 action's
//! signer is recovered from its own signature (mainnet phantom agent); when
//! the signer is not in the set, the whole action is a noop answered
//! `User or API Wallet {signer} does not exist.`, whatever its type.
//! User-signed actions (`signatureChainId`), `multiSig`, `evmRawTx` and
//! expired actions are not ruled. An `approveAgent` native accepted adds its
//! agent to the set (acceptance read from native: input). Users created in
//! the window are not added. Not to be extended here: lane 1 owns the rule.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use hl_node_convert::node::BookDiff;
use p2_node::engine::{ActionResult, BlockOutcome, Engine, UserAction};
use serde_json::Value;

#[path = "../../hl-exec/examples/replay_support/signers.rs"]
// Lane 1's file, kept byte-for-byte; its own lints are lane 1's.
#[allow(
    dead_code,
    unreachable_pub,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction
)]
mod signers;

/// Per block, per action: the recovered signer the rule found absent.
pub(crate) type Decisions = Rc<RefCell<VecDeque<Vec<Option<[u8; 20]>>>>>;

/// The rule's state across blocks.
pub(crate) struct WalletRule {
    set: signers::Signers,
    ctx: secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    /// L1 actions whose signer could not be recovered (not ruled).
    pub(crate) not_recovered: usize,
    /// Accepted `approveAgent`s that added a new agent.
    pub(crate) agents_added: usize,
    /// Actions ruled absent.
    pub(crate) absent: usize,
    out: Decisions,
}

impl WalletRule {
    /// Reads the signer set (one address per line, `#` comments).
    pub(crate) fn new(r: impl std::io::BufRead, out: Decisions) -> Self {
        WalletRule {
            set: signers::Signers::from_reader(r),
            ctx: secp256k1::Secp256k1::verification_only(),
            not_recovered: 0,
            agents_added: 0,
            absent: 0,
            out,
        }
    }

    /// Addresses in the set now.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.set.len()
    }

    /// Decides one block from its replica line and decoded actions (with
    /// native's responses, for `approveAgent` acceptance), and queues the
    /// decisions for [`RuledEngine`].
    pub(crate) fn decide(
        &mut self,
        line: &str,
        acts: &[UserAction<'_>],
        native: &[Value],
        time_ns: u64,
    ) -> Result<(), String> {
        let l1 = signers::signed_actions(line);
        if l1.len() != acts.len() {
            return Err(format!(
                "--signers: {} signed actions, {} decoded",
                l1.len(),
                acts.len()
            ));
        }
        let now_ms = time_ns / 1_000_000;
        let mut d = Vec::with_capacity(acts.len());
        for (i, (a, s)) in acts.iter().zip(&l1).enumerate() {
            let t = a.action_type;
            let expired = a.expires_after.is_some_and(|e| e < now_ms);
            let user_signed =
                a.action.get("signatureChainId").is_some() || t == "multiSig" || t == "evmRawTx";
            let mut unknown = None;
            if !user_signed && !expired {
                match s.recover(&self.ctx, signers::MAINNET) {
                    Some(sg) => {
                        if !self.set.exists(&sg) {
                            unknown = Some(sg);
                        }
                    }
                    None => self.not_recovered += 1,
                }
            }
            if t == "approveAgent" && native.get(i).is_some_and(|x| x["status"] == "ok") {
                if let Some(ag) = a.action.get("agentAddress").and_then(Value::as_str) {
                    self.agents_added += usize::from(self.set.add_hex(ag));
                }
            }
            self.absent += usize::from(unknown.is_some());
            d.push(unknown);
        }
        self.out.borrow_mut().push_back(d);
        Ok(())
    }
}

/// Native's answer for an action the rule rules absent.
#[must_use]
pub(crate) fn absent_response(signer: &[u8; 20]) -> Value {
    serde_json::json!({"status":"err","response":format!(
        "User or API Wallet {} does not exist.",
        signers::addr_hex(signer)
    )})
}

/// True for native's unknown-wallet error.
#[must_use]
pub(crate) fn is_unknown_wallet(v: &Value) -> bool {
    v["status"] == "err"
        && v["response"].as_str().is_some_and(|r| {
            r.starts_with("User or API Wallet ") && r.ends_with(" does not exist.")
        })
}

/// Runs the inner engine on the actions the rule leaves, and answers the
/// others with [`absent_response`]; they are noops (no oid, no book change),
/// as lane 1's kind 5.
pub(crate) struct RuledEngine<'e, E: Engine> {
    /// The engine under test.
    pub(crate) inner: &'e mut E,
    /// Decisions queued by [`WalletRule::decide`], one entry per block.
    pub(crate) decisions: Decisions,
}

impl<E: Engine> Engine for RuledEngine<'_, E> {
    fn execute_block(&mut self, block_time_ns: u64, actions: &[UserAction<'_>]) -> BlockOutcome {
        let d = self.decisions.borrow_mut().pop_front().unwrap_or_default();
        if d.iter().all(Option::is_none) {
            return self.inner.execute_block(block_time_ns, actions);
        }
        let kept: Vec<UserAction<'_>> = actions
            .iter()
            .zip(d.iter().chain(std::iter::repeat(&None)))
            .filter(|(_, u)| u.is_none())
            .map(|(a, _)| *a)
            .collect();
        let out = self.inner.execute_block(block_time_ns, &kept);
        let mut inner = out.results.into_iter();
        let results = (0..actions.len())
            .map(|i| match d.get(i).copied().flatten() {
                Some(s) => ActionResult::Response(absent_response(&s)),
                None => inner.next().unwrap_or(ActionResult::Unsupported),
            })
            .collect();
        BlockOutcome {
            results,
            book_diffs: out.book_diffs,
        }
    }

    fn resync(
        &mut self,
        height: u64,
        native_diffs: &[BookDiff],
        native_responses: &[Value],
    ) -> Result<(), String> {
        self.inner.resync(height, native_diffs, native_responses)
    }

    fn core_exec_ns(&self) -> Option<u64> {
        self.inner.core_exec_ns()
    }

    fn final_book(&self) -> Option<String> {
        self.inner.final_book()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Answers each action it receives with its own type; counts calls.
    struct Stub(Vec<usize>);
    impl Engine for Stub {
        fn execute_block(&mut self, _: u64, actions: &[UserAction<'_>]) -> BlockOutcome {
            self.0.push(actions.len());
            BlockOutcome {
                results: actions
                    .iter()
                    .map(|a| ActionResult::Response(serde_json::json!(a.action_type)))
                    .collect(),
                book_diffs: Vec::new(),
            }
        }
        fn resync(&mut self, _: u64, _: &[BookDiff], _: &[Value]) -> Result<(), String> {
            Ok(())
        }
    }

    fn ua<'a>(t: &'a str, v: &'a Value) -> UserAction<'a> {
        UserAction {
            action_type: t,
            action: v,
            nonce: 1,
            user: None,
            vault_address: None,
            expires_after: None,
            bundle: 0,
        }
    }

    #[test]
    fn absent_actions_are_answered_and_not_run_the_rest_keep_their_places() {
        let v = serde_json::json!({});
        let acts = [ua("order", &v), ua("cancel", &v), ua("noop", &v)];
        let d: Decisions = Rc::default();
        d.borrow_mut().push_back(vec![None, Some([0xab; 20]), None]);
        d.borrow_mut().push_back(vec![None, None, None]);
        let mut stub = Stub(Vec::new());
        let mut e = RuledEngine {
            inner: &mut stub,
            decisions: d,
        };
        let out = e.execute_block(0, &acts);
        let r: Vec<Value> = out
            .results
            .iter()
            .map(|x| match x {
                ActionResult::Response(v) => v.clone(),
                ActionResult::Unsupported => Value::Null,
            })
            .collect();
        assert_eq!(r[0], serde_json::json!("order"));
        assert_eq!(
            r[1],
            serde_json::json!({"status":"err","response":"User or API Wallet 0xabababababababababababababababababababab does not exist."})
        );
        assert!(is_unknown_wallet(&r[1]));
        assert_eq!(r[2], serde_json::json!("noop"));
        let _ = e.execute_block(0, &acts);
        assert_eq!(stub.0, vec![2, 3]);
    }
}
