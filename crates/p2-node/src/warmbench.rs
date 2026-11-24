//! Cold vs warm first-N-blocks latency, one fresh process per trial.
//!
//! Hot path timed per block (monotonic clock): decode the `replica_cmds` line,
//! build the engine input, apply the block's book changes to the Phase 1
//! reference book (`hl-book-ref`). This is a stand-in for `execute_block`
//! until `hl-exec` lands: it shows what warm-up buys on this code, not engine
//! speed.
//!
//! Configurations (every warm-up step switched off once, to show what each
//! one buys):
//! - `cold`: nothing; the first measured block is the process's first.
//! - `warm`: pin + pre-touched arena + hot-path prewarm (a different synthetic
//!   block set pushed through the same path `prewarm` times, then the book
//!   is reset).
//! - `warm-no-pin`, `warm-no-arena`, `warm-no-prewarm`: `warm` minus one step.
//!
//! `mlockall` is not in the matrix: it needs an unlimited `RLIMIT_MEMLOCK`,
//! which this box does not grant (reported, not hidden).

use std::fmt::Write as _;
use std::time::Instant;

use hl_book_ref::{Book, BookOp, Lots, Oid, Order, Px, Side};
use hl_node_convert::node::{BookDiff, DiffKind, NodeSide};

use crate::engine::UserAction;

/// Trial configurations, in matrix order.
pub const CONFIGS: &[&str] = &[
    "cold",
    "warm",
    "warm-no-pin",
    "warm-no-arena",
    "warm-no-prewarm",
];

/// Steps a configuration enables: (pin, arena, prewarm).
#[must_use]
pub fn steps(config: &str) -> Option<(bool, bool, bool)> {
    match config {
        "cold" => Some((false, false, false)),
        "warm" => Some((true, true, true)),
        "warm-no-pin" => Some((false, true, true)),
        "warm-no-arena" => Some((true, false, true)),
        "warm-no-prewarm" => Some((true, true, false)),
        _ => None,
    }
}

/// Decimal string to integer units with `scale` decimals (`"0.012"`, 3 -> 12).
#[must_use]
pub fn fixed(s: &str, scale: u32) -> Option<u64> {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if frac.len() > usize::try_from(scale).ok()? || int.is_empty() {
        return None;
    }
    let mut v: u64 = int.parse().ok()?;
    let mut f = frac.to_owned();
    while f.len() < usize::try_from(scale).ok()? {
        f.push('0');
    }
    v = v.checked_mul(10u64.checked_pow(scale)?)?;
    if !f.is_empty() {
        v = v.checked_add(f.parse::<u64>().ok()?)?;
    }
    Some(v)
}

/// Applies native-shaped book diffs to the reference book (stand-in for the
/// engine's book step). Returns how many applied.
pub fn apply_diffs(book: &mut Book, diffs: &[BookDiff]) -> usize {
    let mut n = 0;
    for d in diffs {
        let (Some(px), side) = (fixed(&d.px, 0), d.side) else {
            continue;
        };
        let op = match &d.raw_book_diff {
            DiffKind::New { sz, .. } => {
                let (Some(sz), Some(side)) = (fixed(sz, 3), side) else {
                    continue;
                };
                let side = match side {
                    NodeSide::Bid => Side::Bid,
                    NodeSide::Ask => Side::Ask,
                };
                BookOp::Insert(Order {
                    oid: Oid(d.oid),
                    side,
                    px: Px(px),
                    sz: Lots(sz),
                })
            }
            DiffKind::Update { new_sz, .. } => {
                let Some(sz) = fixed(new_sz, 3) else { continue };
                BookOp::Resize {
                    oid: Oid(d.oid),
                    sz: Lots(sz),
                }
            }
            DiffKind::Remove => BookOp::Cancel { oid: Oid(d.oid) },
        };
        if book.apply(&op).is_ok() {
            n += 1;
        }
    }
    n
}

/// One block through the hot path; returns (`ns`, checksum).
pub fn hot(
    line: &str,
    diffs: &[BookDiff],
    book: &mut Book,
    arena: Option<&mut [u8]>,
) -> (u64, u64) {
    let t = Instant::now();
    let Ok(block) = hl_block::decode_block(line) else {
        return (0, 0);
    };
    let acts: Vec<UserAction<'_>> = block.actions().map(UserAction::from_signed).collect();
    let applied = apply_diffs(book, diffs);
    if let Some(a) = arena {
        // Scratch writes into the hot-path arena (what a preallocated engine
        // buffer would take): one byte per action.
        for (i, act) in acts.iter().enumerate() {
            let len = a.len();
            let k = usize::try_from(act.nonce).unwrap_or(0) ^ i;
            a[k % len] ^= 1;
        }
    }
    let ns = u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX);
    (ns, u64::try_from(acts.len() + applied).unwrap_or(0))
}

/// Per-event summary across trials: event index -> sorted samples.
#[must_use]
pub fn summarize(config: &str, trials: &[Vec<u64>], n: usize) -> String {
    let mut s = String::new();
    let pick = |idx: std::ops::Range<usize>| {
        let mut v: Vec<u64> = trials
            .iter()
            .flat_map(|t| t.get(idx.clone()).unwrap_or(&[]).iter().copied())
            .collect();
        v.sort_unstable();
        v
    };
    let row = |s: &mut String, label: &str, v: &[u64]| {
        let p = |m| crate::timing::percentile(v, m).unwrap_or(0);
        let _ = writeln!(
            s,
            "{config:<16}{label:<14}{:>8}{:>12}{:>12}{:>12}{:>12}",
            v.len(),
            p(500),
            p(900),
            p(990),
            v.last().copied().unwrap_or(0)
        );
    };
    row(&mut s, "event 0", &pick(0..1));
    row(&mut s, "events 1-9", &pick(1..10.min(n)));
    row(&mut s, &format!("events 0-{}", n - 1), &pick(0..n));
    s
}
