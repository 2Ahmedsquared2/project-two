//! The real signer stage: `hl-order`'s signer behind the harness's `Signer`
//! trait, checked against the Python SDK golden vectors
//! (`golden/hl-order/vectors.json`). This test is what the stage's `real`
//! label rests on.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use hl_order::{Action, Cloid, Decimal8, LimitOrder, Tif};
use hl_replay::{HlOrderSigner, OrderIntent, Signer, Stage, StageLabel};
use serde_json::Value;

/// The public test key of the golden vectors. Never a real key.
const TEST_KEY_HEX: &str = "0123456789012345678901234567890123456789012345678901234567890123";

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../golden/hl-order/vectors.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("read vectors")).expect("json")
}

fn hex_bytes(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn golden_signer() -> HlOrderSigner {
    let key: [u8; 32] = hex_bytes(TEST_KEY_HEX).try_into().expect("32 bytes");
    HlOrderSigner::new(&key).expect("valid key")
}

fn action_from(inputs: &Value) -> Action {
    let asset = || u32::try_from(inputs["asset"].as_u64().expect("asset")).expect("u32");
    match inputs["kind"].as_str().expect("kind") {
        "order" => Action::Order(LimitOrder {
            asset: asset(),
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
            asset: asset(),
            oid: inputs["oid"].as_u64().expect("oid"),
        },
        "scheduleCancel" => Action::ScheduleCancel {
            time: inputs["time"].as_u64(),
        },
        other => panic!("kind {other}"),
    }
}

/// The harness intent for a golden case, when the case is one the harness
/// can express: an ALO order, not reduce-only, no cloid, no vault, no
/// expiry, price a whole number of 0.1 ticks, size a whole number of 0.00001
/// lots.
fn intent_from(case: &Value) -> Option<OrderIntent> {
    let i = &case["inputs"];
    if i["kind"] != "order"
        || i["tif"] != "Alo"
        || i["reduce_only"] != false
        || !i["cloid"].is_null()
        || !case["vault"].is_null()
        || !case["expires_after"].is_null()
    {
        return None;
    }
    let px = Decimal8::parse(i["px"].as_str()?).ok()?.units();
    let sz = Decimal8::parse(i["sz"].as_str()?).ok()?.units();
    if px % 10_000_000 != 0 || sz % 1_000 != 0 {
        return None;
    }
    Some(OrderIntent {
        asset: u32::try_from(i["asset"].as_u64()?).ok()?,
        is_buy: i["is_buy"].as_bool()?,
        px_ticks: px / 10_000_000,
        sz_lots: sz / 1_000,
        nonce: case["nonce"].as_u64()?,
    })
}

#[test]
fn the_adapter_uses_the_golden_key_address() {
    let v = vectors();
    assert_eq!(v["is_mainnet"], Value::Bool(false));
    let want = hex_bytes(v["signer_address"].as_str().expect("addr"));
    assert_eq!(golden_signer().address().to_vec(), want);
}

/// Every golden case (orders with each tif, cloid, vault, expiry; cancels;
/// scheduleCancel) through the adapter's signing path: the bytes handed to
/// the send stage are the SDK's `POST /exchange` body, so r, s, v and the
/// action JSON all match byte for byte.
#[test]
fn every_golden_vector_signs_byte_for_byte_through_the_adapter() {
    let v = vectors();
    let cases = v["signing"].as_array().expect("signing");
    assert!(cases.len() >= 12, "expected the full case list");
    let mut s = golden_signer();
    let mut out = Vec::new();
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let action = action_from(&case["inputs"]);
        let vault: Option<[u8; 20]> = case["vault"]
            .as_str()
            .map(|a| hex_bytes(a).try_into().expect("20 bytes"));
        out.clear();
        s.sign_action(
            &action,
            case["nonce"].as_u64().expect("nonce"),
            vault.as_ref(),
            case["expires_after"].as_u64(),
            &mut out,
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        let want = format!("{}\n", case["body_json"].as_str().expect("body"));
        assert_eq!(String::from_utf8_lossy(&out), want, "{name}");
        let body: Value = serde_json::from_slice(&out).expect("json");
        assert_eq!(body["signature"]["r"], case["r"], "{name}: r");
        assert_eq!(body["signature"]["s"], case["s"], "{name}: s");
        assert_eq!(body["signature"]["v"], case["v"], "{name}: v");
    }
}

/// The path the harness times, `Signer::sign(&OrderIntent)`, on the golden
/// cases an intent can express. `alo_buy` is one, so the intent mapping
/// (ticks and lots to wire strings, ALO, nonce) is covered by a vector too.
#[test]
fn an_order_intent_signs_to_the_golden_body() {
    let v = vectors();
    let mut s = golden_signer();
    let mut covered = Vec::new();
    for case in v["signing"].as_array().expect("signing") {
        let Some(intent) = intent_from(case) else {
            continue;
        };
        let name = case["name"].as_str().expect("name");
        let mut out = Vec::new();
        s.sign(&intent, &mut out)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let want = format!("{}\n", case["body_json"].as_str().expect("body"));
        assert_eq!(String::from_utf8_lossy(&out), want, "{name}");
        covered.push(name);
    }
    assert_eq!(covered, ["alo_buy"], "golden cases an intent can express");
}

#[test]
fn the_label_is_real_and_names_the_code_path() {
    assert_eq!(<HlOrderSigner as Stage>::LABEL, StageLabel::Real);
    let name = <HlOrderSigner as Stage>::NAME;
    assert!(name.contains("hl_order::Signer::sign_l1"), "{name}");
    assert!(name.contains("testnet"), "{name}");
    // hl-order signs with libsecp256k1 since PR #20; k256 is test-only there.
    assert!(name.contains("libsecp256k1 RFC 6979"), "{name}");
    assert!(!name.contains("k256 RFC 6979"), "{name}");
}

#[test]
fn an_intent_maps_to_one_alo_order_in_ticks_and_lots() {
    let intent = OrderIntent {
        asset: 3,
        is_buy: false,
        px_ticks: 671_235,
        sz_lots: 100,
        nonce: 9,
    };
    assert_eq!(
        HlOrderSigner::action_for(&intent).unwrap(),
        Action::Order(LimitOrder {
            asset: 3,
            is_buy: false,
            px: Decimal8::parse("67123.5").unwrap(),
            sz: Decimal8::parse("0.001").unwrap(),
            reduce_only: false,
            tif: Tif::Alo,
            cloid: None,
        })
    );
}

#[test]
fn a_price_or_size_that_does_not_fit_is_an_error_not_a_wrapped_number() {
    let ok = OrderIntent {
        asset: 0,
        is_buy: true,
        px_ticks: 1,
        sz_lots: 1,
        nonce: 1,
    };
    let mut s = HlOrderSigner::throwaway();
    let mut out = Vec::new();
    for bad in [
        OrderIntent {
            px_ticks: u64::MAX / 10_000_000 + 1,
            ..ok
        },
        OrderIntent {
            sz_lots: u64::MAX / 1_000 + 1,
            ..ok
        },
    ] {
        assert!(s.sign(&bad, &mut out).is_err(), "{bad:?}");
        assert!(out.is_empty(), "nothing is appended for a refused order");
    }
}

#[test]
fn a_throwaway_signer_appends_one_body_per_order_and_keys_differ_from_the_golden_key() {
    let mut s = HlOrderSigner::throwaway();
    assert_ne!(s.address(), golden_signer().address());
    let mut out = Vec::new();
    for nonce in [1, 2] {
        let intent = OrderIntent {
            asset: 0,
            is_buy: true,
            px_ticks: 671_235,
            sz_lots: 100,
            nonce,
        };
        s.sign(&intent, &mut out).unwrap();
    }
    let text = String::from_utf8(out).unwrap();
    assert!(text.ends_with('\n'));
    let bodies: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["action"]["orders"][0]["p"], "67123.5");
    assert_eq!(bodies[0]["action"]["orders"][0]["s"], "0.001");
    assert_eq!(bodies[1]["nonce"], 2);
    assert_ne!(bodies[0]["signature"], bodies[1]["signature"]);
}

#[test]
fn an_invalid_key_is_refused() {
    assert!(HlOrderSigner::new(&[0; 32]).is_err());
    assert!(HlOrderSigner::new(&[0xff; 32]).is_err());
}
