//! Parse cost: the old `serde_json::Value` parsers (the oracle) against the
//! strict zero-allocation parsers and the node header scan.
//!
//! ```text
//! cargo bench -p hl-latency --bench parse
//! ```
#![allow(missing_docs, clippy::unwrap_used)]

#[path = "../tests/oracle/mod.rs"]
mod oracle;
#[path = "../tests/support/mod.rs"]
mod support;

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use hl_latency::{parse_node_line, parse_ws_message_into, scan_node_header, Observation};

fn node(c: &mut Criterion) {
    for (name, events) in [("small_2_events", 2), ("busy_600_events", 600)] {
        let line = support::node_line(676_607_012, events);
        let mut g = c.benchmark_group(format!("node/{name}"));
        g.bench_function("oracle_value", |b| {
            b.iter(|| oracle::parse_node_line(black_box(&line), 1).unwrap());
        });
        g.bench_function("strict", |b| {
            b.iter(|| parse_node_line(black_box(&line), 1).unwrap());
        });
        g.bench_function("header_scan", |b| {
            b.iter(|| scan_node_header(black_box(line.as_bytes())).unwrap());
        });
        g.finish();
    }
}

fn ws(c: &mut Criterion) {
    let frames = [
        (
            "l2book_20_levels",
            support::l2book_frame(1_795_128_590_474, 20),
        ),
        ("trades_12", support::trades_frame(1_795_128_575_393, 12, 3)),
    ];
    for (name, frame) in &frames {
        let mut g = c.benchmark_group(format!("ws/{name}"));
        g.bench_function("oracle_value", |b| {
            b.iter(|| oracle::parse_ws_message(black_box(frame), 1).unwrap());
        });
        let mut out: Vec<Observation> = Vec::with_capacity(16);
        g.bench_function("into", |b| {
            b.iter(|| {
                parse_ws_message_into(black_box(frame), 1, &mut out).unwrap();
                black_box(out.len())
            });
        });
        g.finish();
    }
}

criterion_group!(benches, node, ws);
criterion_main!(benches);
