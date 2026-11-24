//! Per-call latency percentiles of signing one testnet limit order (the same
//! work as the criterion `sign_order` benchmark). Criterion reports a mean per
//! batch of iterations; this times every call on its own so p50 and p99 are
//! real per-call percentiles. No network.
//!
//! `cargo bench -p hl-order --bench sign_percentiles` (`HL_SIGN_N` sets the
//! number of timed calls, default 100000).
#![allow(missing_docs, clippy::expect_used, clippy::print_stdout)]

use std::hint::black_box;
use std::time::Instant;

use hl_order::{Action, Decimal8, LimitOrder, Network, Signer, Tif};

fn main() {
    let n: usize = std::env::var("HL_SIGN_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
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
    let mut sign = |nonce: u64| {
        black_box(
            signer
                .sign_l1(Network::Testnet, black_box(&action), nonce, None, None)
                .expect("sign"),
        );
    };
    for _ in 0..n / 10 {
        nonce += 1;
        sign(nonce);
    }
    let mut ns: Vec<u64> = Vec::with_capacity(n);
    for _ in 0..n {
        nonce += 1;
        let t = Instant::now();
        sign(nonce);
        ns.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    ns.sort_unstable();
    let at = |num: usize, den: usize| ns[((n - 1) * num) / den];
    println!(
        "sign_order n={n} min={} p50={} p90={} p99={} p99.9={} max={} (ns per call)",
        ns[0],
        at(50, 100),
        at(90, 100),
        at(99, 100),
        at(999, 1000),
        ns[n - 1],
    );
}
