//! Census tests: counts per action type and per (action type, response class).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_block::{census::Census, decode_block, normalize_message};

const FIXTURE: &str = include_str!("data/three_blocks.jsonl");

fn census() -> Census {
    let mut c = Census::default();
    for l in FIXTURE.lines() {
        c.add(&decode_block(l).unwrap());
    }
    c
}

#[test]
fn counts_blocks_bundles_actions() {
    let c = census();
    assert_eq!(c.blocks, 3);
    assert_eq!(c.empty_blocks, 1);
    assert_eq!(c.bundles, 2);
    assert_eq!(c.actions, 3);
    assert_eq!(c.first_round, Some(992_814_678));
    assert_eq!(c.last_round, Some(992_814_681));
    assert_eq!(c.round_gaps, 1); // 678 -> 680 skips one round
}

#[test]
fn per_action_type() {
    let c = census();
    assert_eq!(c.by_type.get("order").copied(), Some(1));
    assert_eq!(c.by_type.get("cancel").copied(), Some(1));
    assert_eq!(c.by_type.get("brandNewAction").copied(), Some(1));
}

#[test]
fn unknown_types_are_flagged() {
    let c = census();
    assert_eq!(
        c.unknown_types().collect::<Vec<_>>(),
        vec!["brandNewAction"]
    );
}

#[test]
fn per_response_class() {
    let c = census();
    let k = |t: &str, r: &str| c.by_response.get(&(t.to_owned(), r.to_owned())).copied();
    assert_eq!(k("order", "ok:order:filled"), Some(1));
    assert_eq!(
        k(
            "cancel",
            "ok:cancel:error:Order was never placed, already canceled, or filled. asset=<n>"
        ),
        Some(1)
    );
    assert_eq!(
        k(
            "brandNewAction",
            "err:User or API Wallet <hex> does not exist."
        ),
        Some(1)
    );
}

#[test]
fn normalizes_numbers_and_hex() {
    assert_eq!(
        normalize_message("asset=3 oid 156190414943"),
        "asset=<n> oid <n>"
    );
    assert_eq!(
        normalize_message("wallet 0xAbC123 missing"),
        "wallet <hex> missing"
    );
    assert_eq!(normalize_message("px 0.038385 bad"), "px <n> bad");
    assert_eq!(normalize_message("BTC-USD v2"), "BTC-USD v2");
}

#[test]
fn json_and_text_render() {
    let c = census();
    let j: serde_json::Value = serde_json::from_str(&c.to_json()).unwrap();
    assert_eq!(j["actions"], 3);
    assert_eq!(j["by_type"]["order"], 1);
    let t = c.to_text();
    assert!(t.contains("order"));
    assert!(t.contains("UNKNOWN brandNewAction"));
}

#[test]
fn merge_is_additive() {
    let mut a = census();
    let b = census();
    a.merge(&b);
    assert_eq!(a.actions, 6);
    assert_eq!(a.by_type["order"], 2);
}
