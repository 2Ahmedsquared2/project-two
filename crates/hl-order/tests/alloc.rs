//! Zero allocation per signature after warmup, with the repository's gate.
//!
//! The ECDSA context is built when the `Signer` is (once per process), so the
//! first signature does no setup either.
#![allow(clippy::expect_used)]

use std::hint::black_box;

use hl_alloc_gate::assert_no_alloc_after_warmup;
use hl_order::{request_body, Action, Cloid, Decimal8, LimitOrder, Network, Signer, Tif};

const WARMUP: u64 = 64;
const TICKS: u64 = 2000;

/// The throwaway public test key the goldens use. Never a real key.
fn signer() -> Signer {
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate() {
        *b = [0x01, 0x23, 0x45, 0x67, 0x89][i % 5];
    }
    Signer::from_bytes(&key).expect("key")
}

fn order(i: u64) -> Action {
    Action::Order(LimitOrder {
        asset: 0,
        is_buy: i.is_multiple_of(2),
        px: Decimal8::from_units(3_000_000_000_000 + (i % 5000) * 100_000_000),
        sz: Decimal8::from_units(100_000 + (i % 7) * 100_000),
        reduce_only: false,
        tif: Tif::Alo,
        cloid: Some(Cloid([0xab; 16])),
    })
}

#[test]
fn gate_sign_l1_order() {
    let signer = signer();
    let vault = [0x11u8; 20];
    assert_no_alloc_after_warmup("hl-order: Signer::sign_l1 (order)", WARMUP, TICKS, |i| {
        let nonce = 1_700_000_000_000 + i;
        let vault = (i % 3 == 0).then_some(&vault);
        let expires = (i % 5 == 0).then_some(nonce + 1000);
        black_box(
            signer
                .sign_l1(Network::Testnet, &order(i), nonce, vault, expires)
                .expect("sign"),
        );
    });
}

#[test]
fn gate_sign_l1_cancel_and_schedule_cancel() {
    let signer = signer();
    assert_no_alloc_after_warmup("hl-order: Signer::sign_l1 (cancels)", WARMUP, TICKS, |i| {
        let nonce = 1_700_000_000_000 + i;
        let action = if i.is_multiple_of(2) {
            Action::Cancel { asset: 0, oid: i }
        } else {
            Action::ScheduleCancel { time: Some(nonce) }
        };
        black_box(
            signer
                .sign_l1(Network::Testnet, &action, nonce, None, None)
                .expect("sign"),
        );
    });
}

/// The first signature of a fresh signer does not allocate either: no lazy
/// context creation hides behind the first call.
#[test]
fn first_signature_does_not_allocate() {
    let signer = signer();
    let action = order(0);
    let ((), allocs) = hl_alloc_gate::measure(|| {
        black_box(
            signer
                .sign_l1(Network::Testnet, &action, 1_700_000_000_000, None, None)
                .expect("sign"),
        );
    });
    assert_eq!(allocs.count, 0, "first sign allocated: {allocs:?}");
}

#[test]
fn gate_sign_and_request_body() {
    let signer = signer();
    let mut body = String::with_capacity(1024);
    assert_no_alloc_after_warmup("hl-order: sign_l1 + request_body", WARMUP, TICKS, |i| {
        let nonce = 1_700_000_000_000 + i;
        let action = order(i);
        let signed = signer
            .sign_l1(Network::Testnet, &action, nonce, None, None)
            .expect("sign");
        body.clear();
        request_body(&mut body, &action, nonce, &signed.signature, None, None);
        black_box(body.len());
    });
}
