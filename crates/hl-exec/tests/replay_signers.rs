//! The replay's L1 signer recovery against the SDK golden vectors
//! (hyperliquid-python-sdk 0.24.0, testnet source "b"): the action's msgpack
//! must equal the SDK's byte for byte (key order kept from the JSON text) and
//! ecrecover must return the vector's signer.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

#[path = "../examples/replay_support/signers.rs"]
#[allow(
    dead_code,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::expect_used
)]
mod signers;

use serde_json::Value;

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

#[test]
fn recovers_sdk_vectors() {
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../golden/hl-order/vectors.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let want = v["signer_address"].as_str().unwrap().to_ascii_lowercase();
    let ctx = secp256k1::Secp256k1::verification_only();
    let mut n = 0;
    for t in v["signing"].as_array().unwrap() {
        let sa = signers::signed_action_body(t["body_json"].as_str().unwrap());
        assert_eq!(
            hex(sa.action_msgpack()),
            t["action_msgpack_hex"].as_str().unwrap(),
            "{}",
            t["name"]
        );
        assert_eq!(
            hex(&sa.connection_id().unwrap()),
            t["connection_id_hex"].as_str().unwrap(),
            "{}",
            t["name"]
        );
        let got = sa.recover(&ctx, signers::TESTNET).unwrap();
        assert_eq!(signers::addr_hex(&got), want, "{}", t["name"]);
        // The wrong network's phantom agent recovers someone else.
        assert_ne!(
            signers::addr_hex(&sa.recover(&ctx, signers::MAINNET).unwrap()),
            want
        );
        n += 1;
    }
    assert!(n >= 3, "only {n} vectors");
}

#[test]
fn signer_set_lines_and_add() {
    let txt = "# signers\n0xABCDEFabcdefABCDEFabcdefABCDEFabcdefABCD\n\n0x0000000000000000000000000000000000000001\n";
    let mut s = signers::Signers::from_reader(txt.as_bytes());
    assert_eq!(s.len(), 2);
    let a = [
        0xabu8, 0xcd, 0xef, 0xab, 0xcd, 0xef, 0xab, 0xcd, 0xef, 0xab, 0xcd, 0xef, 0xab, 0xcd, 0xef,
        0xab, 0xcd, 0xef, 0xab, 0xcd,
    ];
    assert!(s.exists(&a));
    let mut b = [0u8; 20];
    b[19] = 2;
    assert!(!s.exists(&b));
    assert!(s.add_hex("0x0000000000000000000000000000000000000002"));
    assert!(s.exists(&b));
}
