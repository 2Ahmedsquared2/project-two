//! Serde shapes of the line, every envelope strict.

use serde::Deserialize;
use serde_json::Value;

use crate::{text::parse_time_ns, Block, BlockError, Bundle, Outcome, Signature, SignedAction};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Line {
    abci_block: AbciBlock,
    resps: Resps,
    #[serde(default)]
    height: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AbciBlock {
    time: String,
    round: u64,
    parent_round: u64,
    proposer: String,
    hardfork: Hardfork,
    signed_action_bundles: Vec<(String, RawBundle)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hardfork {
    version: u64,
    round: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundle {
    signed_actions: Vec<RawSigned>,
    broadcaster: String,
    broadcaster_nonce: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSigned {
    signature: Signature,
    action: Value,
    nonce: u64,
    #[serde(rename = "vaultAddress", default)]
    vault_address: Option<String>,
    #[serde(rename = "expiresAfter", default)]
    expires_after: Option<u64>,
    #[serde(rename = "isFrontend", default)]
    is_frontend: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resps {
    #[serde(rename = "Full")]
    full: Vec<(String, Vec<RawResp>)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResp {
    user: Option<String>,
    res: RawRes,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRes {
    status: String,
    response: Value,
}

fn outcome(res: RawRes, bundle: usize, action: usize) -> Result<Outcome, BlockError> {
    let bad = |why: &str| BlockError::Response {
        bundle,
        action,
        why: why.to_owned(),
    };
    match res.status.as_str() {
        "err" => match res.response {
            Value::String(s) => Ok(Outcome::Err(s)),
            _ => Err(bad("err response is not a string")),
        },
        "ok" => {
            let Value::Object(mut m) = res.response else {
                return Err(bad("ok response is not an object"));
            };
            let Some(Value::String(t)) = m.remove("type") else {
                return Err(bad("ok response without string type"));
            };
            let data = m.remove("data");
            if let Some(k) = m.keys().next() {
                return Err(bad(&format!("ok response has unknown field {k:?}")));
            }
            Ok(Outcome::Ok {
                response_type: t,
                data,
            })
        }
        other => Err(bad(&format!("unknown status {other:?}"))),
    }
}

pub(crate) fn into_block(raw: Line) -> Result<Block, BlockError> {
    let a = raw.abci_block;
    let time_ns = parse_time_ns(&a.time).map_err(|_| BlockError::Time(a.time.clone()))?;
    let resps = raw.resps.full;
    let height = raw.height;
    if resps.len() != a.signed_action_bundles.len() {
        return Err(BlockError::BundleCount {
            bundles: a.signed_action_bundles.len(),
            responses: resps.len(),
        });
    }
    let mut bundles = Vec::with_capacity(resps.len());
    for (bi, ((hash, rb), (rhash, rr))) in
        a.signed_action_bundles.into_iter().zip(resps).enumerate()
    {
        if hash != rhash {
            return Err(BlockError::BundleHash {
                bundle: bi,
                action_hash: hash,
                response_hash: rhash,
            });
        }
        if rb.signed_actions.len() != rr.len() {
            return Err(BlockError::ResponseCount {
                bundle: bi,
                actions: rb.signed_actions.len(),
                responses: rr.len(),
            });
        }
        let mut actions = Vec::with_capacity(rr.len());
        for (ai, (s, r)) in rb.signed_actions.into_iter().zip(rr).enumerate() {
            let Some(Value::String(action_type)) = s.action.get("type").cloned() else {
                return Err(BlockError::ActionType {
                    bundle: bi,
                    action: ai,
                });
            };
            actions.push(SignedAction {
                action_type,
                action: s.action,
                nonce: s.nonce,
                vault_address: s.vault_address,
                expires_after: s.expires_after,
                is_frontend: s.is_frontend,
                signature: s.signature,
                user: r.user,
                outcome: outcome(r.res, bi, ai)?,
            });
        }
        bundles.push(Bundle {
            hash,
            broadcaster: rb.broadcaster,
            broadcaster_nonce: rb.broadcaster_nonce,
            actions,
        });
    }
    Ok(Block {
        height,
        round: a.round,
        parent_round: a.parent_round,
        time: a.time,
        time_ns,
        proposer: a.proposer,
        hardfork_version: a.hardfork.version,
        hardfork_round: a.hardfork.round,
        bundles,
    })
}
