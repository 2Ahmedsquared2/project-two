//! Client behaviour with a fake transport only: nothing here touches a network.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use hl_order::{
    Action, Decimal8, ExchangeClient, LimitOrder, Network, Signer, Tif, Transport,
    TransportError, EXCHANGE_PATH, TESTNET_API_URL,
};
use serde_json::Value;

fn key() -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        // 0x0123...0123: the throwaway public test key used by the goldens.
        let pair = [0x01, 0x23, 0x45, 0x67, 0x89];
        *b = pair[i % 5];
    }
    k
}

#[derive(Default)]
struct FakeTransport {
    sent: Vec<(String, String)>,
    reply: Vec<u8>,
    fail: bool,
}

impl Transport for FakeTransport {
    fn post(&mut self, url: &str, body: &[u8]) -> Result<Vec<u8>, TransportError> {
        if self.fail {
            return Err(TransportError("connection refused".into()));
        }
        self.sent.push((url.to_owned(), String::from_utf8(body.to_vec()).expect("utf8")));
        Ok(self.reply.clone())
    }
}

fn alo_buy() -> Action {
    Action::Order(LimitOrder {
        asset: 0,
        is_buy: true,
        px: Decimal8::parse("30000").expect("px"),
        sz: Decimal8::parse("0.001").expect("sz"),
        reduce_only: false,
        tif: Tif::Alo,
        cloid: None,
    })
}

fn golden_body(name: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../golden/hl-order/vectors.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json");
    v["signing"]
        .as_array()
        .expect("signing")
        .iter()
        .find(|c| c["name"] == name)
        .expect("case")["body_json"]
        .as_str()
        .expect("body")
        .to_owned()
}

#[test]
fn testnet_is_the_only_network() {
    assert_eq!(Network::Testnet.source(), "b");
    assert_eq!(Network::Testnet.base_url(), TESTNET_API_URL);
    assert_eq!(TESTNET_API_URL, "https://api.hyperliquid-testnet.xyz");
    assert_eq!(EXCHANGE_PATH, "/exchange");
    assert_eq!(Network::Testnet.exchange_url(), "https://api.hyperliquid-testnet.xyz/exchange");
}

#[test]
fn send_posts_the_golden_body_to_testnet_and_stamps_timing() {
    let signer = Signer::from_bytes(&key()).expect("key");
    let transport = FakeTransport { reply: br#"{"status":"ok"}"#.to_vec(), ..Default::default() };
    let mut client = ExchangeClient::new(transport, signer, Network::Testnet);
    let resp = client.send(&alo_buy(), 1_700_000_000_001).expect("send");
    assert_eq!(resp.body, br#"{"status":"ok"}"#);
    assert!(resp.timing.after_read >= resp.timing.before_write);
    assert_eq!(resp.timing.rtt(), resp.timing.after_read - resp.timing.before_write);
    let sent = &client.transport().sent;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "https://api.hyperliquid-testnet.xyz/exchange");
    assert_eq!(sent[0].1, golden_body("alo_buy"));
}

#[test]
fn vault_and_expires_after_flow_into_the_body() {
    let signer = Signer::from_bytes(&key()).expect("key");
    let mut client = ExchangeClient::new(FakeTransport::default(), signer, Network::Testnet)
        .with_vault([0x11; 20])
        .with_expires_after(1_700_000_060_000);
    let action = Action::Order(LimitOrder {
        asset: 1,
        is_buy: false,
        px: Decimal8::parse("2501.5").expect("px"),
        sz: Decimal8::parse("0.0123").expect("sz"),
        reduce_only: false,
        tif: Tif::Gtc,
        cloid: None,
    });
    client.send(&action, 1_700_000_000_007).expect("send");
    assert_eq!(client.transport().sent[0].1, golden_body("gtc_sell_vault_and_expires"));
}

#[test]
fn transport_errors_propagate() {
    let signer = Signer::from_bytes(&key()).expect("key");
    let transport = FakeTransport { fail: true, ..Default::default() };
    let mut client = ExchangeClient::new(transport, signer, Network::Testnet);
    let err = client.send(&Action::ScheduleCancel { time: None }, 1).expect_err("must fail");
    assert!(err.to_string().contains("connection refused"), "{err}");
}

#[test]
fn zero_key_is_rejected() {
    assert!(Signer::from_bytes(&[0u8; 32]).is_err());
}
