//! Latency of signing one testnet limit order (msgpack -> keccak -> EIP-712 -> ECDSA),
//! and of building the POST /exchange body. No network.
#![allow(missing_docs, clippy::expect_used)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use hl_order::{request_body, Action, Decimal8, LimitOrder, Network, Signer, Tif};

fn bench(c: &mut Criterion) {
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate() {
        *b = [0x01, 0x23, 0x45, 0x67, 0x89][i % 5];
    }
    let signer = Signer::from_bytes(&key).expect("key");
    let action = Action::Order(LimitOrder {
        asset: 0,
        is_buy: true,
        px: Decimal8::parse("30000").expect("px"),
        sz: Decimal8::parse("0.001").expect("sz"),
        reduce_only: false,
        tif: Tif::Alo,
        cloid: None,
    });
    let mut nonce = 1_700_000_000_000u64;
    c.bench_function("sign_order", |b| {
        b.iter(|| {
            nonce += 1;
            black_box(
                signer
                    .sign_l1(Network::Testnet, black_box(&action), nonce, None, None)
                    .expect("sign"),
            )
        });
    });
    let got = signer
        .sign_l1(Network::Testnet, &action, nonce, None, None)
        .expect("sign");
    let mut body = String::with_capacity(512);
    c.bench_function("request_body", |b| {
        b.iter(|| {
            body.clear();
            request_body(
                &mut body,
                black_box(&action),
                nonce,
                &got.signature,
                None,
                None,
            );
            black_box(body.len())
        });
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
