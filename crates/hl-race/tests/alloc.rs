//! Zero allocation on the hot path after construction.
#![allow(missing_docs, clippy::as_conversions)]

use hl_race::{Item, Racer};

#[test]
fn offer_and_pop_do_not_allocate() {
    let mut r = Racer::new(4, 1024, 64, 1_000_000);
    let payload = [7u8; 48];
    hl_alloc_gate::assert_no_alloc_after_warmup("racer offer+pop", 16, 50_000, |i| {
        let seq = 1_000_000 + i;
        for src in 0..4 {
            let _ = r.offer(
                src,
                Item {
                    seq,
                    payload: &payload,
                    t_recv_ns: i * 10 + src as u64,
                },
            );
        }
        let _ = r.offer(
            0,
            Item {
                seq: seq + 5000,
                payload: &payload,
                t_recv_ns: 0,
            },
        ); // TooFar
        while r.pop().is_some() {}
    });
}
