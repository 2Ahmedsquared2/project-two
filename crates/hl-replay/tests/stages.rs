//! The pluggable stages and the pipeline that chains them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_replay::{
    active_pipeline, node_line, stage_table, ActiveSigner, Book, Clock, Decision, Handle,
    NodeLineParser, OrderIntent, Parser, Pipeline, PrototypeSigner, Signer, Stage, StageLabel,
    StubBook, StubDecision, TopOfBook,
};
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use secp256k1::{Message, Secp256k1};

const TEST_KEY: [u8; 32] = [0x42; 32]; // throwaway, test only, never funded

fn line(seq: u64) -> String {
    node_line(seq, 20).trim_end().to_owned()
}

fn intent(nonce: u64) -> OrderIntent {
    OrderIntent {
        asset: 0,
        is_buy: true,
        px_ticks: 671_235,
        sz_lots: 100,
        nonce,
    }
}

#[test]
fn every_stage_carries_its_honest_label() {
    let t = stage_table();
    let got: Vec<(&str, StageLabel)> = t.iter().map(|s| (s.stage, s.label)).collect();
    assert_eq!(
        got,
        vec![
            ("reader", StageLabel::Real),
            ("parser", StageLabel::Real),
            ("book", StageLabel::Stub),
            ("decision", StageLabel::Stub),
            ("signer", StageLabel::Prototype),
            ("send", StageLabel::Prototype),
        ]
    );
    assert_eq!(t[1].name, "hl_latency::parse_node_line");
    assert_eq!(StageLabel::Real.to_string(), "real");
    assert_eq!(StageLabel::Stub.to_string(), "stub");
    assert_eq!(StageLabel::Prototype.to_string(), "prototype");
    assert_eq!(<ActiveSigner as Stage>::LABEL, StageLabel::Prototype);
    assert_eq!(<NodeLineParser as Stage>::LABEL, StageLabel::Real);
}

#[test]
fn the_real_parser_is_hl_latency_parse_node_line() {
    let l = line(676_607_012);
    let want = hl_latency::parse_node_line(&l, 9).unwrap();
    assert_eq!(NodeLineParser.parse(&l, 9).unwrap(), want);
    assert_eq!(want.block_number, Some(676_607_012));
    assert!(NodeLineParser.parse("{not json", 9).is_err());
}

#[test]
fn the_stub_book_and_decision_do_deterministic_work() {
    let obs = hl_latency::parse_node_line(&line(7), 1).unwrap();
    let (mut a, mut b) = (StubBook::default(), StubBook::default());
    let top = a.apply(&obs);
    assert_eq!(top, b.apply(&obs));
    assert!(top.bid_ticks > 0 && top.bid_ticks < top.ask_ticks, "{top:?}");
    assert_eq!(top.seq, 7);
    let order = StubDecision::default().decide(&top).unwrap();
    assert!(order.px_ticks > 0 && order.sz_lots > 0);
    assert!(order.nonce >= obs.block_time_ms);
    // A crossed or empty book is not quoted.
    assert_eq!(StubDecision::default().decide(&TopOfBook::default()), None);
}

#[test]
fn nonces_never_repeat_within_a_run() {
    let mut book = StubBook::default();
    let mut d = StubDecision::default();
    let mut seen = std::collections::BTreeSet::new();
    for seq in 0..500 {
        // Same block time on every line: the nonce must still move.
        let obs = hl_latency::parse_node_line(&line(seq), 1).unwrap();
        let o = d.decide(&book.apply(&obs)).unwrap();
        assert!(seen.insert(o.nonce), "nonce {} repeated", o.nonce);
    }
}

#[test]
fn the_prototype_signature_recovers_to_the_key_that_signed() {
    let mut s = PrototypeSigner::new(&TEST_KEY).unwrap();
    let mut out = Vec::new();
    s.sign(&intent(1_753_606_210_051), &mut out).unwrap();
    assert_eq!(out.last(), Some(&b'\n'), "one newline-terminated body per order");
    let body: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(body["nonce"], 1_753_606_210_051_u64);
    assert_eq!(body["action"]["type"], "order");
    assert_eq!(body["action"]["orders"][0]["p"], "67123.5");
    assert_eq!(body["action"]["orders"][0]["s"], "0.001");
    assert_eq!(body["action"]["orders"][0]["b"], true);
    let hex = |k: &str| {
        let s = body["signature"][k].as_str().unwrap().strip_prefix("0x").unwrap().to_owned();
        assert_eq!(s.len(), 64);
        (0..32)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect::<Vec<u8>>()
    };
    let mut compact = hex("r");
    compact.extend(hex("s"));
    let v = i32::try_from(body["signature"]["v"].as_u64().unwrap()).unwrap();
    assert!(v == 27 || v == 28);
    let sig = RecoverableSignature::from_compact(&compact, RecoveryId::from_i32(v - 27).unwrap())
        .unwrap();
    let digest = s.digest(&intent(1_753_606_210_051));
    let pk = Secp256k1::verification_only()
        .recover_ecdsa(&Message::from_digest(digest), &sig)
        .unwrap();
    assert_eq!(pk.serialize(), s.public_key());
}

#[test]
fn the_digest_covers_every_order_field() {
    let mut s = PrototypeSigner::new(&TEST_KEY).unwrap();
    let base = s.digest(&intent(5));
    assert_eq!(base, s.digest(&intent(5)));
    assert_ne!(base, s.digest(&intent(6)));
    for other in [
        OrderIntent { asset: 1, ..intent(5) },
        OrderIntent { is_buy: false, ..intent(5) },
        OrderIntent { px_ticks: 671_236, ..intent(5) },
        OrderIntent { sz_lots: 101, ..intent(5) },
    ] {
        assert_ne!(base, s.digest(&other), "{other:?}");
    }
}

#[test]
fn signing_appends_and_reuses_the_callers_buffer() {
    let mut s = PrototypeSigner::throwaway();
    let mut out = Vec::new();
    s.sign(&intent(1), &mut out).unwrap();
    let first = out.len();
    s.sign(&intent(2), &mut out).unwrap();
    assert_eq!(out.iter().filter(|b| **b == b'\n').count(), 2);
    assert!(out.len() >= 2 * first - 2);
}

#[test]
fn an_invalid_key_is_refused() {
    assert!(PrototypeSigner::new(&[0; 32]).is_err());
    assert!(PrototypeSigner::new(&[0xff; 32]).is_err());
}

#[test]
fn the_pipeline_turns_a_line_into_one_signed_order_and_stamps_t1() {
    let clock = Clock::new();
    let mut p = active_pipeline();
    let mut out = Vec::new();
    let t0 = clock.now_ns();
    let r = p.on_line(line(42).as_bytes(), t0, &clock, &mut out).unwrap();
    assert!(r.order);
    assert!(r.t1_ns >= t0);
    assert!(clock.now_ns() >= r.t1_ns);
    let body: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(body["signature"]["r"].is_string());
    // The buffer is cleared per line: one order, not an ever-growing batch.
    p.on_line(line(43).as_bytes(), t0, &clock, &mut out).unwrap();
    assert_eq!(out.iter().filter(|b| **b == b'\n').count(), 1);
}

#[test]
fn bad_input_is_an_error_not_a_skipped_sample() {
    let clock = Clock::new();
    let mut p = active_pipeline();
    let mut out = Vec::new();
    assert!(p.on_line(b"{\"block_number\":1}", 0, &clock, &mut out).is_err());
    assert!(p.on_line(&[0xff, 0xfe, b'{'], 0, &clock, &mut out).is_err());
}

/// Swapping a stage is a type change, nothing else: this is what the real
/// signer will do.
#[test]
fn any_stage_can_be_swapped_by_type() {
    #[derive(Default)]
    struct FixedSigner;
    impl Stage for FixedSigner {
        const NAME: &'static str = "fixed";
        const LABEL: StageLabel = StageLabel::Stub;
    }
    impl Signer for FixedSigner {
        fn sign(&mut self, i: &OrderIntent, out: &mut Vec<u8>) -> Result<(), hl_replay::ReplayError> {
            out.extend_from_slice(&i.nonce.to_be_bytes());
            out.push(b'\n');
            Ok(())
        }
    }
    let clock = Clock::new();
    let mut p = Pipeline::new(NodeLineParser, StubBook::default(), StubDecision::default(), FixedSigner);
    let mut out = Vec::new();
    assert!(p.on_line(line(1).as_bytes(), 0, &clock, &mut out).unwrap().order);
    assert_eq!(out.len(), 9);
}

#[test]
fn the_clock_is_monotonic_nanoseconds() {
    let c = Clock::new();
    let a = c.now_ns();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let b = c.now_ns();
    assert!(b - a >= 2_000_000, "{a} {b}");
    let copy = c;
    assert!(copy.now_ns() >= b, "copies share the epoch");
}
