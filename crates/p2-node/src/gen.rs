//! Synthetic `replica_cmds` blocks and matching native diffs, for harness
//! tests and the cold-vs-warm benchmark. Deterministic from a seed. Not real
//! data: no number measured on it is a mainnet claim.

use std::fmt::Write as _;

/// xorshift64*.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn time(ms: u64) -> String {
    // 2025-09-08T00:00:00 = 1757289600 s; synthetic blocks start there.
    let s = 1_757_289_600 + ms / 1000;
    let sod = s % 86_400;
    format!(
        "2025-09-{:02}T{:02}:{:02}:{:02}.{:03}000000",
        8 + (s - 1_757_289_600) / 86_400,
        sod / 3600,
        sod % 3600 / 60,
        sod % 60,
        ms % 1000
    )
}

/// Returns (blocks jsonl, diffs jsonl) for `n` blocks of `actions` orders each,
/// heights from 1, one block per 70 ms.
#[must_use]
pub fn generate(n: u64, actions: u64, seed: u64) -> (String, String) {
    let mut r = Rng(seed.max(1));
    let mut blocks = String::new();
    let mut diffs = String::new();
    let sig = r#"{"r":"0x1111111111111111111111111111111111111111111111111111111111111111","s":"0x2222222222222222222222222222222222222222222222222222222222222222","v":27}"#;
    let mut oid = 1_000_000u64;
    for h in 1..=n {
        let t = time(h * 70);
        let mut acts = String::new();
        let mut resps = String::new();
        let mut evs = String::new();
        for a in 0..actions {
            let buy = r.next().is_multiple_of(2);
            // Bids below 100500, asks from 100500: the book never crosses.
            let px = if buy { 100_000 } else { 100_500 } + r.next() % 500;
            let sz = 1 + r.next() % 50;
            let user = format!("0x{:040x}", r.next() % 64);
            oid += 1;
            let sep = if a == 0 { "" } else { "," };
            let _ = write!(
                acts,
                r#"{sep}{{"signature":{sig},"action":{{"type":"order","orders":[{{"a":0,"b":{buy},"p":"{px}","s":"0.{sz:03}","r":false,"t":{{"limit":{{"tif":"Alo"}}}}}}],"grouping":"na"}},"nonce":{}}}"#,
                1_757_289_600_000 + h * 100 + a
            );
            let _ = write!(
                resps,
                r#"{sep}{{"user":"{user}","res":{{"status":"ok","response":{{"type":"order","data":{{"statuses":[{{"resting":{{"oid":{oid}}}}}]}}}}}}}}"#
            );
            let side = if buy { "B" } else { "A" };
            let _ = write!(
                evs,
                r#"{sep}{{"user":"{user}","oid":{oid},"coin":"BTC","side":"{side}","px":"{px}","raw_book_diff":{{"new":{{"sz":"0.{sz:03}"}}}}}}"#
            );
        }
        let hash = format!("0x{:064x}", r.next());
        let (bundles, full) = if actions == 0 {
            (String::new(), String::new())
        } else {
            (
                format!(
                    r#"["{hash}",{{"signed_actions":[{acts}],"broadcaster":"0x{:040x}","broadcaster_nonce":{h}}}]"#,
                    7
                ),
                format!(r#"["{hash}",[{resps}]]"#),
            )
        };
        let _ = writeln!(
            blocks,
            r#"{{"abci_block":{{"time":"{t}","round":{},"parent_round":{},"proposer":"0x{:040x}","hardfork":{{"version":57,"round":1}},"signed_action_bundles":[{bundles}]}},"resps":{{"Full":[{full}]}}}}"#,
            h + 1_000_000,
            h + 999_999,
            5
        );
        let _ = writeln!(
            diffs,
            r#"{{"local_time":"{t}","block_time":"{t}","block_number":{h},"events":[{evs}]}}"#
        );
    }
    (blocks, diffs)
}
