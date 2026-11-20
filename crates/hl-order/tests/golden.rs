//! Golden vectors from the official Python SDK (`scripts/gen_hl_order_golden.py`,
//! hyperliquid-python-sdk 0.24.0, testnet). Every byte must match: action
//! msgpack, connectionId, r/s/v and the POST /exchange JSON body.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use hl_order::{
    connection_id, request_body, Action, Cloid, Decimal8, LimitOrder, Network, Signer, Tif,
};
use serde_json::Value;

const TEST_KEY_HEX: &str = "0123456789012345678901234567890123456789012345678901234567890123";

fn vectors() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../golden/hl-order/vectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("read vectors")).expect("json")
}

fn hex_bytes(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hex20(s: &str) -> [u8; 20] {
    hex_bytes(s).try_into().expect("20 bytes")
}

fn signer() -> Signer {
    let key: [u8; 32] = hex_bytes(TEST_KEY_HEX).try_into().expect("32 bytes");
    Signer::from_bytes(&key).expect("valid key")
}

fn action_from(inputs: &Value) -> Action {
    match inputs["kind"].as_str().expect("kind") {
        "order" => Action::Order(LimitOrder {
            asset: u32::try_from(inputs["asset"].as_u64().expect("asset")).expect("u32"),
            is_buy: inputs["is_buy"].as_bool().expect("is_buy"),
            px: Decimal8::parse(inputs["px"].as_str().expect("px")).expect("px"),
            sz: Decimal8::parse(inputs["sz"].as_str().expect("sz")).expect("sz"),
            reduce_only: inputs["reduce_only"].as_bool().expect("ro"),
            tif: match inputs["tif"].as_str().expect("tif") {
                "Alo" => Tif::Alo,
                "Gtc" => Tif::Gtc,
                "Ioc" => Tif::Ioc,
                other => panic!("tif {other}"),
            },
            cloid: inputs["cloid"]
                .as_str()
                .map(|c| Cloid(hex_bytes(c).try_into().expect("16 bytes"))),
        }),
        "cancel" => Action::Cancel {
            asset: u32::try_from(inputs["asset"].as_u64().expect("asset")).expect("u32"),
            oid: inputs["oid"].as_u64().expect("oid"),
        },
        "scheduleCancel" => Action::ScheduleCancel {
            time: inputs["time"].as_u64(),
        },
        other => panic!("kind {other}"),
    }
}

#[test]
fn signer_address_matches_sdk() {
    let v = vectors();
    assert_eq!(v["is_mainnet"], Value::Bool(false));
    let want = v["signer_address"].as_str().expect("addr").to_lowercase();
    assert_eq!(signer().address_hex(), want);
}

#[test]
fn signing_vectors_match_sdk_byte_for_byte() {
    let v = vectors();
    let cases = v["signing"].as_array().expect("signing");
    assert!(cases.len() >= 12, "expected the full case list");
    let signer = signer();
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let action = action_from(&case["inputs"]);
        let nonce = case["nonce"].as_u64().expect("nonce");
        let vault = case["vault"].as_str().map(hex20);
        let expires_after = case["expires_after"].as_u64();

        let mut packed = Vec::new();
        action.write_msgpack(&mut packed).expect("encode");
        assert_eq!(
            packed,
            hex_bytes(case["action_msgpack_hex"].as_str().expect("mp")),
            "{name}: msgpack"
        );

        let cid = connection_id(&action, nonce, vault.as_ref(), expires_after).expect("hash");
        assert_eq!(
            cid.to_vec(),
            hex_bytes(case["connection_id_hex"].as_str().expect("cid")),
            "{name}: connectionId"
        );

        let signed = signer
            .sign_l1(Network::Testnet, &action, nonce, vault.as_ref(), expires_after)
            .expect("sign");
        assert_eq!(signed.connection_id, cid, "{name}: signed connectionId");
        assert_eq!(signed.signature.r_hex(), case["r"].as_str().expect("r"), "{name}: r");
        assert_eq!(signed.signature.s_hex(), case["s"].as_str().expect("s"), "{name}: s");
        assert_eq!(
            u64::from(signed.signature.v),
            case["v"].as_u64().expect("v"),
            "{name}: v"
        );

        let mut body = String::new();
        request_body(&mut body, &action, nonce, &signed.signature, vault.as_ref(), expires_after);
        assert_eq!(body, case["body_json"].as_str().expect("body"), "{name}: body");
    }
}

#[test]
fn required_cases_are_covered() {
    let v = vectors();
    let names: Vec<&str> = v["signing"]
        .as_array()
        .expect("signing")
        .iter()
        .map(|c| c["name"].as_str().expect("name"))
        .collect();
    for want in [
        "alo_buy",
        "gtc_sell",
        "ioc_buy_reduce_only",
        "alo_buy_vault",
        "alo_buy_expires_after",
        "gtc_sell_vault_and_expires",
        "cancel",
        "schedule_cancel_with_time",
        "schedule_cancel_without_time",
    ] {
        assert!(names.contains(&want), "missing golden case {want}");
    }
}

#[test]
fn float_to_wire_matches_sdk() {
    let v = vectors();
    for case in v["float_to_wire"].as_array().expect("f2w") {
        let input = case["input"].as_str().expect("input");
        let d = Decimal8::parse(input).expect("parse");
        assert_eq!(d.to_wire(), case["wire"].as_str().expect("wire"), "input {input}");
    }
}

#[test]
fn round_px_perp_matches_sdk_example_rounding() {
    let v = vectors();
    for case in v["round_px_perp"].as_array().expect("round") {
        let input = case["input"].as_str().expect("input");
        let szd = u8::try_from(case["sz_decimals"].as_u64().expect("szd")).expect("u8");
        let d = Decimal8::parse(input).expect("parse");
        let got = d.round_px_perp(szd).expect("round");
        assert_eq!(got.to_wire(), case["wire"].as_str().expect("wire"), "{input} szd {szd}");
    }
}
