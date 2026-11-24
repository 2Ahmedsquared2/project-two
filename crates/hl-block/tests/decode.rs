//! Decoder tests against a hand-written fixture in the `replica_cmds` shape
//! (Dwellir `StreamBlocks` "Full Block Spec"). Synthetic values; the real
//! capture test lives in `real_capture.rs` and runs when data is present.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_block::{decode_block, parse_time_ns, BlockError, Outcome, Reader};

const FIXTURE: &str = include_str!("data/three_blocks.jsonl");

fn lines() -> Vec<&'static str> {
    FIXTURE.lines().collect()
}

#[test]
fn decodes_header_fields() {
    let b = decode_block(lines()[0]).unwrap();
    assert_eq!(b.round, 992_814_678);
    assert_eq!(b.parent_round, 992_814_677);
    assert_eq!(b.hardfork_version, 57);
    assert_eq!(b.hardfork_round, 990_500_929);
    assert_eq!(b.proposer, "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a");
    // 2025-09-08T06:41:57.997372546 = 1757313717 s (date -u -d ... +%s)
    assert_eq!(b.time_ns, 1_757_313_717_997_372_546);
}

#[test]
fn pairs_actions_with_responses() {
    let b = decode_block(lines()[0]).unwrap();
    assert_eq!(b.bundles.len(), 1);
    let bundle = &b.bundles[0];
    assert_eq!(bundle.actions.len(), 2);
    assert_eq!(bundle.actions[0].action_type, "order");
    assert_eq!(bundle.actions[0].nonce, 1_757_313_597_362);
    assert_eq!(bundle.actions[1].action_type, "cancel");
    assert!(bundle.actions[1].vault_address.is_some());
    assert_eq!(
        bundle.actions[0].user,
        "0xecececececececececececececececececececec"
    );
    assert!(
        matches!(bundle.actions[0].outcome, Outcome::Ok { ref response_type, .. } if response_type == "order")
    );
}

#[test]
fn empty_block_and_err_response() {
    let b = decode_block(lines()[1]).unwrap();
    assert_eq!(b.bundles.len(), 0);
    let b = decode_block(lines()[2]).unwrap();
    let a = &b.bundles[0].actions[0];
    assert_eq!(a.action_type, "brandNewAction");
    assert_eq!(a.expires_after, Some(1_757_313_718_705));
    assert!(matches!(a.outcome, Outcome::Err(ref s) if s.starts_with("User or API Wallet")));
}

#[test]
fn unknown_envelope_field_is_an_error() {
    let bad = lines()[0].replacen("\"proposer\"", "\"surprise\":1,\"proposer\"", 1);
    assert!(matches!(decode_block(&bad), Err(BlockError::Json(_))));
    let bad = lines()[0].replacen("\"broadcaster\"", "\"extra\":true,\"broadcaster\"", 1);
    assert!(decode_block(&bad).is_err());
}

#[test]
fn response_count_mismatch_is_an_error() {
    // Drop the second response of the first bundle.
    let mut v: serde_json::Value = serde_json::from_str(lines()[0]).unwrap();
    v["resps"]["Full"][0][1].as_array_mut().unwrap().pop();
    let e = decode_block(&v.to_string()).unwrap_err();
    assert!(
        matches!(
            e,
            BlockError::ResponseCount {
                bundle: 0,
                actions: 2,
                responses: 1
            }
        ),
        "{e:?}"
    );
}

#[test]
fn bundle_hash_mismatch_is_an_error() {
    let mut v: serde_json::Value = serde_json::from_str(lines()[0]).unwrap();
    v["resps"]["Full"][0][0] = serde_json::json!("0xdead");
    assert!(matches!(
        decode_block(&v.to_string()),
        Err(BlockError::BundleHash { bundle: 0, .. })
    ));
}

#[test]
fn bundle_count_mismatch_is_an_error() {
    let mut v: serde_json::Value = serde_json::from_str(lines()[0]).unwrap();
    v["resps"]["Full"] = serde_json::json!([]);
    assert!(matches!(
        decode_block(&v.to_string()),
        Err(BlockError::BundleCount {
            bundles: 1,
            responses: 0
        })
    ));
}

#[test]
fn action_without_type_is_an_error() {
    let bad = lines()[0].replacen("\"type\":\"order\",\"orders\"", "\"orders\"", 1);
    assert!(matches!(
        decode_block(&bad),
        Err(BlockError::ActionType { .. })
    ));
}

#[test]
fn time_parsing() {
    assert_eq!(parse_time_ns("1970-01-01T00:00:00").unwrap(), 0);
    assert_eq!(
        parse_time_ns("1970-01-01T00:00:01.5").unwrap(),
        1_500_000_000
    );
    assert_eq!(
        parse_time_ns("2000-03-01T00:00:00.000000001").unwrap(),
        951_868_800_000_000_001
    );
    assert!(parse_time_ns("2025-13-01T00:00:00").is_err());
    assert!(parse_time_ns("2025-02-30T00:00:00").is_err());
    assert!(parse_time_ns("2025-01-01T00:00:00.1234567891").is_err());
    assert!(parse_time_ns("2025-01-01 00:00:00").is_err());
}

#[test]
fn reader_numbers_lines_and_reports_errors_with_line() {
    let mut input = String::from(FIXTURE);
    input.push_str("{not json}\n");
    let r = Reader::new(input.as_bytes());
    let got: Vec<_> = r.collect();
    assert_eq!(got.len(), 4);
    assert!(got[..3].iter().all(Result::is_ok));
    let e = got[3].as_ref().unwrap_err();
    assert_eq!(e.line, 4);
}

#[test]
fn reader_skips_blank_lines() {
    let input = format!("\n{}\n\n", lines()[1]);
    let got: Vec<_> = Reader::new(input.as_bytes()).collect();
    assert_eq!(got.len(), 1);
    assert!(got[0].is_ok());
}

#[test]
fn outcome_round_trips_to_the_native_json() {
    for l in lines() {
        let raw: serde_json::Value = serde_json::from_str(l).unwrap();
        let b = decode_block(l).unwrap();
        for (bi, bundle) in b.bundles.iter().enumerate() {
            for (ai, a) in bundle.actions.iter().enumerate() {
                assert_eq!(a.outcome.to_json(), raw["resps"]["Full"][bi][1][ai]["res"]);
            }
        }
    }
}
