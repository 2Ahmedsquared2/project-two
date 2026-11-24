//! Action census over a block range: what the engine must cover.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::{json, Map, Value};

use crate::{normalize_message, Block, Outcome};

/// Action types listed in Dwellir's `replica_cmds` action reference (read
/// 2026-11-20). Anything else is reported as UNKNOWN so it is looked at.
pub const KNOWN_TYPES: &[&str] = &[
    "order",
    "cancel",
    "cancelByCloid",
    "batchModify",
    "modify",
    "scheduleCancel",
    "twapOrder",
    "twapCancel",
    "spotSend",
    "sendAsset",
    "usdClassTransfer",
    "withdraw3",
    "usdSend",
    "updateLeverage",
    "updateIsolatedMargin",
    "userPortfolioMargin",
    "approveAgent",
    "approveBuilderFee",
    "userDexAbstraction",
    "agentEnableDexAbstraction",
    "subAccountTransfer",
    "createSubAccount",
    "subAccountModify",
    "subAccountSpotTransfer",
    "createVault",
    "vaultTransfer",
    "NetChildVaultPositionsAction",
    "VoteEthDepositAction",
    "VoteEthFinalizedWithdrawalAction",
    "ValidatorSignWithdrawalAction",
    "voteAppHash",
    "evmRawTx",
    "evmUserModify",
    "multiSig",
    "convertToMultiSigUser",
    "setReferrer",
    "registerReferrer",
    "perpDeploy",
    "claimRewards",
    "cDeposit",
    "cWithdraw",
    "tokenDelegate",
    "borrowLend",
    "spotUser",
    "noop",
    "SetGlobalAction",
    "reserveRequestWeight",
];

/// Counts over a block range.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Blocks seen.
    pub blocks: u64,
    /// Blocks with no bundles.
    pub empty_blocks: u64,
    /// Bundles seen.
    pub bundles: u64,
    /// Actions seen.
    pub actions: u64,
    /// First round seen.
    pub first_round: Option<u64>,
    /// Last round seen.
    pub last_round: Option<u64>,
    /// Times a round was more than one past the previous one.
    pub round_gaps: u64,
    /// Times a round was not greater than the previous one.
    pub round_regressions: u64,
    /// Actions per `action.type`.
    pub by_type: BTreeMap<String, u64>,
    /// Per (`action.type`, response class): one count per status item for
    /// responses with `data.statuses`, else one per action.
    pub by_response: BTreeMap<(String, String), u64>,
    /// Lines that failed to decode, by normalised error.
    pub decode_errors: BTreeMap<String, u64>,
}

fn bump<K: Ord>(m: &mut BTreeMap<K, u64>, k: K, n: u64) {
    *m.entry(k).or_insert(0) += n;
}

/// Response classes of one outcome: `err:<msg>`, `ok:<type>`, or per status
/// `ok:<type>:<status key>` (`error` statuses carry their message).
#[must_use]
pub fn response_classes(o: &Outcome) -> Vec<String> {
    match o {
        Outcome::Err(m) => vec![format!("err:{}", normalize_message(m))],
        Outcome::Ok {
            response_type,
            data,
        } => {
            let statuses = data
                .as_ref()
                .and_then(|d| d.get("statuses"))
                .and_then(Value::as_array);
            match statuses {
                Some(st) if !st.is_empty() => st
                    .iter()
                    .map(|s| match s {
                        Value::Object(m) => match m.iter().next() {
                            Some((k, Value::String(msg))) if k == "error" => {
                                format!("ok:{response_type}:error:{}", normalize_message(msg))
                            }
                            Some((k, _)) => format!("ok:{response_type}:{k}"),
                            None => format!("ok:{response_type}:<empty>"),
                        },
                        Value::String(s) => format!("ok:{response_type}:{}", normalize_message(s)),
                        _ => format!("ok:{response_type}:<other>"),
                    })
                    .collect(),
                _ => vec![format!("ok:{response_type}")],
            }
        }
    }
}

impl Census {
    /// Adds one block.
    pub fn add(&mut self, b: &Block) {
        if let Some(prev) = self.last_round {
            if b.round <= prev {
                self.round_regressions += 1;
            } else if b.round > prev + 1 {
                self.round_gaps += 1;
            }
        }
        self.first_round.get_or_insert(b.round);
        self.last_round = Some(b.round);
        self.blocks += 1;
        if b.bundles.is_empty() {
            self.empty_blocks += 1;
        }
        self.bundles += u64::try_from(b.bundles.len()).unwrap_or(u64::MAX);
        for a in b.actions() {
            self.actions += 1;
            bump(&mut self.by_type, a.action_type.clone(), 1);
            for c in response_classes(&a.outcome) {
                bump(&mut self.by_response, (a.action_type.clone(), c), 1);
            }
        }
    }

    /// Records a line that did not decode.
    pub fn add_error(&mut self, msg: &str) {
        bump(&mut self.decode_errors, normalize_message(msg), 1);
    }

    /// Adds another census (e.g. from another file). Round continuity across
    /// the two is not checked.
    pub fn merge(&mut self, o: &Census) {
        self.blocks += o.blocks;
        self.empty_blocks += o.empty_blocks;
        self.bundles += o.bundles;
        self.actions += o.actions;
        self.round_gaps += o.round_gaps;
        self.round_regressions += o.round_regressions;
        self.first_round = match (self.first_round, o.first_round) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.last_round = match (self.last_round, o.last_round) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        for (k, v) in &o.by_type {
            bump(&mut self.by_type, k.clone(), *v);
        }
        for (k, v) in &o.by_response {
            bump(&mut self.by_response, k.clone(), *v);
        }
        for (k, v) in &o.decode_errors {
            bump(&mut self.decode_errors, k.clone(), *v);
        }
    }

    /// Action types not in [`KNOWN_TYPES`].
    pub fn unknown_types(&self) -> impl Iterator<Item = &str> {
        self.by_type
            .keys()
            .map(String::as_str)
            .filter(|t| !KNOWN_TYPES.contains(t))
    }

    /// Machine-readable form.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut resp: Map<String, Value> = Map::new();
        for ((t, c), n) in &self.by_response {
            let e = resp
                .entry(t.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(m) = e {
                m.insert(c.clone(), json!(n));
            }
        }
        json!({
            "blocks": self.blocks,
            "empty_blocks": self.empty_blocks,
            "bundles": self.bundles,
            "actions": self.actions,
            "first_round": self.first_round,
            "last_round": self.last_round,
            "round_gaps": self.round_gaps,
            "round_regressions": self.round_regressions,
            "by_type": self.by_type,
            "by_response": resp,
            "unknown_types": self.unknown_types().collect::<Vec<_>>(),
            "decode_errors": self.decode_errors,
        })
        .to_string()
    }

    /// Human-readable table, action types by count, responses under each.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "blocks {} (empty {}), bundles {}, actions {}, rounds {:?}..{:?}, round gaps {}, regressions {}, decode errors {}",
            self.blocks,
            self.empty_blocks,
            self.bundles,
            self.actions,
            self.first_round,
            self.last_round,
            self.round_gaps,
            self.round_regressions,
            self.decode_errors.values().sum::<u64>()
        );
        let mut types: Vec<_> = self.by_type.iter().collect();
        types.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (t, n) in types {
            let tag = if KNOWN_TYPES.contains(&t.as_str()) {
                ""
            } else {
                "UNKNOWN "
            };
            let _ = writeln!(s, "{n:>12}  {tag}{t}");
            let mut rs: Vec<_> = self
                .by_response
                .iter()
                .filter(|((tt, _), _)| tt == t)
                .collect();
            rs.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            for ((_, c), n) in rs {
                let _ = writeln!(s, "{n:>16}    {c}");
            }
        }
        for (e, n) in &self.decode_errors {
            let _ = writeln!(s, "{n:>12}  DECODE ERROR {e}");
        }
        s
    }
}
