//! Replays a recorded dataset through a candidate order book and reports the
//! first place the candidate disagrees with the recorded book.
//!
//! The candidate is anything that implements [`Candidate`]: the reference
//! book from `hl-book-ref` today, every faster design later. The checker keeps
//! one candidate per asset declared in the dataset header and applies each
//! block's per-order effects to the asset they name. After each block, every
//! recorded book state is compared with that asset's candidate in integer
//! ticks and lots:
//!
//! - `l4_book`: level by level and order by order (size, order count, queue
//!   position, per-order size, missing or extra level);
//! - `l2_book`: level by level on (price, total size, order count), over the
//!   top `depth` levels when the record says the source was truncated.
//!
//! The first [`Divergence`] names the block, asset, side, price level and field.
//!
//! [`check`] replays per-order `ops` and only counts raw transactions
//! (`txs`). [`check_replay`] instead replays the raw signed actions through
//! the reference book with [`replay::Replayer`] (public data has no oids, so
//! `ops` are empty there) and compares the same recorded states.

use std::collections::BTreeMap;
use std::fmt;
use std::io::BufRead;

use hl_book_ref::{
    Book, BookOp, BookSnapshot, L2Level, L2Snapshot, Level, Lots, Oid, Px, Side, SizeOverflow,
};
use hl_dataset::{BookState, ReadError, Reader};

pub mod replay;

/// An order book under test.
pub trait Candidate {
    /// Why an op could not be applied.
    type Error: fmt::Display;

    /// Apply one per-order effect.
    fn apply(&mut self, op: &BookOp) -> Result<(), Self::Error>;

    /// The whole book as plain data, bids highest first and asks lowest first.
    fn snapshot(&self) -> BookSnapshot;

    /// The book aggregated to L2, at most `depth` levels per side, or the
    /// first level whose total size overflows `u64`. The default aggregates
    /// [`Candidate::snapshot`] with checked sums; a fast candidate should
    /// override it and must never wrap or saturate a total.
    fn l2(&self, depth: Option<usize>) -> Result<L2Snapshot, SizeOverflow> {
        self.snapshot().to_l2(depth)
    }
}

impl Candidate for Book {
    type Error = hl_book_ref::ApplyError;

    fn apply(&mut self, op: &BookOp) -> Result<(), Self::Error> {
        Book::apply(self, op)
    }

    fn snapshot(&self) -> BookSnapshot {
        Book::snapshot(self)
    }

    fn l2(&self, depth: Option<usize>) -> Result<L2Snapshot, SizeOverflow> {
        Book::l2(self, depth)
    }
}

/// Which field of a price level differs. `recorded` is the dataset's value,
/// `candidate` the book under test's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    /// Total resting size at the level.
    Size {
        /// Recorded total.
        recorded: Lots,
        /// Candidate total.
        candidate: Lots,
    },
    /// Number of resting orders at the level (totals agree).
    OrderCount {
        /// Recorded count.
        recorded: usize,
        /// Candidate count.
        candidate: usize,
    },
    /// A different order id at a queue position (totals and counts agree).
    QueuePosition {
        /// 0-based position from the front of the queue.
        position: usize,
        /// Recorded order id at that position.
        recorded: Oid,
        /// Candidate order id at that position.
        candidate: Oid,
    },
    /// The same order id at the same position with a different remaining size.
    OrderSize {
        /// 0-based position from the front of the queue.
        position: usize,
        /// The order.
        oid: Oid,
        /// Recorded remaining size.
        recorded: Lots,
        /// Candidate remaining size.
        candidate: Lots,
    },
    /// The recorded book has this level and the candidate does not.
    MissingLevel {
        /// Recorded total at the level.
        recorded: Lots,
        /// Recorded order count at the level.
        recorded_orders: usize,
    },
    /// The candidate has this level and the recorded book does not.
    ExtraLevel {
        /// Candidate total at the level.
        candidate: Lots,
        /// Candidate order count at the level.
        candidate_orders: usize,
    },
    /// A level's total size overflows `u64` lots, so it cannot be compared.
    /// Reported as a divergence, never as equal: a wrapped total could match
    /// a wrong recorded size.
    SizeOverflow {
        /// Which book holds the level.
        book: Which,
    },
}

/// The recorded book or the candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Which {
    /// The dataset's book.
    Recorded,
    /// The book under test.
    Candidate,
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Field::Size {
                recorded,
                candidate,
            } => write!(
                f,
                "size: recorded {} lots, candidate {} lots",
                recorded.0, candidate.0
            ),
            Field::OrderCount {
                recorded,
                candidate,
            } => write!(
                f,
                "order count: recorded {recorded}, candidate {candidate}"
            ),
            Field::QueuePosition {
                position,
                recorded,
                candidate,
            } => write!(
                f,
                "queue position {position}: recorded oid {}, candidate oid {}",
                recorded.0, candidate.0
            ),
            Field::OrderSize {
                position,
                oid,
                recorded,
                candidate,
            } => write!(
                f,
                "order size at queue position {position} (oid {}): recorded {} lots, candidate {} lots",
                oid.0, recorded.0, candidate.0
            ),
            Field::MissingLevel {
                recorded,
                recorded_orders,
            } => write!(
                f,
                "level missing from candidate: recorded {} lots in {recorded_orders} order(s)",
                recorded.0
            ),
            Field::ExtraLevel {
                candidate,
                candidate_orders,
            } => write!(
                f,
                "level not in recorded book: candidate has {} lots in {candidate_orders} order(s)",
                candidate.0
            ),
            Field::SizeOverflow { book } => write!(
                f,
                "{} level total size overflows u64 lots",
                match book {
                    Which::Recorded => "recorded",
                    Which::Candidate => "candidate",
                }
            ),
        }
    }
}

/// Where two books first differ within one asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LevelDiff {
    /// Side of the differing level.
    pub side: Side,
    /// Price of the differing level, in ticks.
    pub px: Px,
    /// What differs.
    pub field: Field,
}

/// The first disagreement between a recorded book and the candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    /// Block after which the books differ.
    pub height: u64,
    /// Asset index.
    pub asset: u32,
    /// Coin name from the dataset header.
    pub coin: String,
    /// Side of the differing level.
    pub side: Side,
    /// Price of the differing level, in ticks.
    pub px: Px,
    /// What differs.
    pub field: Field,
}

impl fmt::Display for Divergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let side = match self.side {
            Side::Bid => "bid",
            Side::Ask => "ask",
        };
        write!(
            f,
            "divergence at block {} in {} (asset {}) on the {side} side at price {} ticks: {}",
            self.height, self.coin, self.asset, self.px.0, self.field
        )
    }
}

/// Why a check did not finish cleanly.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    /// The dataset could not be read.
    #[error("dataset: {0}")]
    Read(#[from] ReadError),
    /// The candidate refused an op.
    #[error(
        "block {height}, op {op_index} (asset {asset}, {op:?}): candidate rejected it: {message}"
    )]
    Apply {
        /// Block height.
        height: u64,
        /// Asset the op applies to.
        asset: u32,
        /// 0-based index of the op within the block.
        op_index: usize,
        /// The op.
        op: BookOp,
        /// The candidate's error text.
        message: String,
    },
    /// The candidate's book differs from the recorded one.
    #[error("{0}")]
    Divergence(Divergence),
}

/// What a clean check covered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Blocks replayed.
    pub blocks: u64,
    /// Per-order effects applied.
    pub ops: u64,
    /// Raw transactions carried by the blocks (counted, not replayed).
    pub txs: u64,
    /// Recorded `l4_book` states compared.
    pub l4_books_compared: u64,
    /// Recorded `l2_book` states compared.
    pub l2_books_compared: u64,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ok: {} blocks, {} ops, {} L4 + {} L2 recorded books compared, no divergence",
            self.blocks, self.ops, self.l4_books_compared, self.l2_books_compared
        )?;
        if self.txs > 0 {
            write!(f, " ({} txs not replayed)", self.txs)?;
        }
        Ok(())
    }
}

/// Replay every block of `reader`, one candidate per declared asset (built
/// by `new_candidate` from the asset index), comparing after each block with
/// every recorded book state. Stops at the first problem.
pub fn check<R: BufRead, C: Candidate>(
    reader: Reader<R>,
    mut new_candidate: impl FnMut(u32) -> C,
) -> Result<Report, CheckError> {
    let coins: BTreeMap<u32, String> = reader
        .header()
        .assets
        .iter()
        .map(|a| (a.asset_index, a.name.clone()))
        .collect();
    let mut books: BTreeMap<u32, C> = coins.keys().map(|&a| (a, new_candidate(a))).collect();
    let mut report = Report::default();
    for step in reader.steps() {
        let step = step?;
        let height = step.block.height;
        for (op_index, aop) in step.block.ops.iter().enumerate() {
            // The reader guarantees every op's asset is declared.
            let Some(book) = books.get_mut(&aop.asset) else {
                unreachable!("reader admitted undeclared asset {}", aop.asset);
            };
            book.apply(&aop.op).map_err(|e| CheckError::Apply {
                height,
                asset: aop.asset,
                op_index,
                op: aop.op,
                message: e.to_string(),
            })?;
            report.ops += 1;
        }
        report.txs += u64::try_from(step.block.txs.len()).unwrap_or(u64::MAX);
        report.blocks += 1;
        for recorded in &step.books {
            let asset = recorded.asset();
            let Some(book) = books.get(&asset) else {
                unreachable!("reader admitted undeclared asset {asset}");
            };
            compare_recorded(height, recorded, book, &coins, &mut report)?;
        }
    }
    Ok(report)
}

/// Compare one recorded state with the candidate's book, counting it in `report`.
fn compare_recorded<C: Candidate + ?Sized>(
    height: u64,
    recorded: &BookState,
    book: &C,
    coins: &BTreeMap<u32, String>,
    report: &mut Report,
) -> Result<(), CheckError> {
    let asset = recorded.asset();
    let diff = match recorded {
        BookState::L4(r) => {
            report.l4_books_compared += 1;
            compare_l4(&r.to_snapshot(), &book.snapshot())
        }
        BookState::L2(r) => {
            report.l2_books_compared += 1;
            let depth = r.depth.map(|d| usize::try_from(d).unwrap_or(usize::MAX));
            let cand = match book.l2(depth) {
                Ok(c) => c,
                Err(o) => {
                    return Err(divergence(
                        height,
                        asset,
                        coins,
                        LevelDiff {
                            side: o.side,
                            px: o.px,
                            field: Field::SizeOverflow {
                                book: Which::Candidate,
                            },
                        },
                    ))
                }
            };
            let conv = |ls: &[hl_dataset::L2Level]| -> Vec<L2Level> {
                ls.iter()
                    .map(|l| L2Level {
                        px: Px(l.px_ticks),
                        sz: Lots(l.sz_lots),
                        n: l.n,
                    })
                    .collect()
            };
            compare_l2(&conv(&r.bids), &conv(&r.asks), &cand.bids, &cand.asks)
        }
    };
    match diff {
        Some(d) => Err(divergence(height, asset, coins, d)),
        None => Ok(()),
    }
}

fn divergence(height: u64, asset: u32, coins: &BTreeMap<u32, String>, d: LevelDiff) -> CheckError {
    CheckError::Divergence(Divergence {
        height,
        asset,
        coin: coins.get(&asset).cloned().unwrap_or_default(),
        side: d.side,
        px: d.px,
        field: d.field,
    })
}

/// What a raw-action replay covered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplayReport {
    /// Recorded states compared (blocks, L4 and L2 counts; `ops` and `txs` unused).
    pub compared: Report,
    /// What the replayer did.
    pub stats: replay::ReplayStats,
}

impl fmt::Display for ReplayReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "ok: replayed {} blocks of raw actions, {} L4 + {} L2 recorded books compared, no divergence",
            self.stats.blocks, self.compared.l4_books_compared, self.compared.l2_books_compared
        )?;
        write!(f, "{}", self.stats)
    }
}

/// A divergence (or read error) together with the replay counts up to it.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct ReplayFailure {
    /// What stopped the replay.
    pub error: CheckError,
    /// Counts up to and including the failing block.
    pub report: ReplayReport,
}

/// Replay the raw actions of every block through [`replay::Replayer`] and,
/// when `compare` is true, compare each recorded book state after its block.
/// With `compare` false the whole dataset is read and replayed and only the
/// counts are returned (used for real public data, which has no seed book).
pub fn check_replay<R: BufRead>(
    reader: Reader<R>,
    config: replay::ReplayConfig,
    compare: bool,
) -> Result<ReplayReport, Box<ReplayFailure>> {
    let coins: BTreeMap<u32, String> = reader
        .header()
        .assets
        .iter()
        .map(|a| (a.asset_index, a.name.clone()))
        .collect();
    let mut r = replay::Replayer::new(&reader.header().assets, config);
    let mut compared = Report::default();
    let fail = |error: CheckError, compared: Report, r: &replay::Replayer| {
        Box::new(ReplayFailure {
            error,
            report: ReplayReport {
                compared,
                stats: r.stats().clone(),
            },
        })
    };
    for step in reader.steps() {
        let step = match step {
            Ok(s) => s,
            Err(e) => return Err(fail(e.into(), compared, &r)),
        };
        r.apply_block(&step.block);
        compared.blocks += 1;
        if !compare {
            continue;
        }
        for recorded in &step.books {
            let Some(book) = r.book(recorded.asset()) else {
                unreachable!("reader admitted undeclared asset {}", recorded.asset());
            };
            if let Err(e) =
                compare_recorded(step.block.height, recorded, book, &coins, &mut compared)
            {
                return Err(fail(e, compared, &r));
            }
        }
    }
    Ok(ReplayReport {
        compared,
        stats: r.stats().clone(),
    })
}

/// Compare two per-order books and return the first difference. A level
/// whose total size overflows `u64` (recorded book first, then candidate;
/// bids before asks) is reported as [`Field::SizeOverflow`] before anything
/// else. Then bids are checked before asks; within a side, levels are walked
/// from best to worst; within a level, total size, then order count, then
/// each queue position.
#[must_use]
pub fn compare_l4(recorded: &BookSnapshot, candidate: &BookSnapshot) -> Option<LevelDiff> {
    for (book, snap) in [(Which::Recorded, recorded), (Which::Candidate, candidate)] {
        for (side, levels) in [(Side::Bid, &snap.bids), (Side::Ask, &snap.asks)] {
            if let Some(l) = levels.iter().find(|l| l.total().is_none()) {
                return Some(LevelDiff {
                    side,
                    px: l.px,
                    field: Field::SizeOverflow { book },
                });
            }
        }
    }
    // Every total is representable from here on.
    compare_side(
        Side::Bid,
        &recorded.bids,
        &candidate.bids,
        l4_view,
        compare_level,
    )
    .or_else(|| {
        compare_side(
            Side::Ask,
            &recorded.asks,
            &candidate.asks,
            l4_view,
            compare_level,
        )
    })
}

/// Compare two aggregated books (bids, asks per side) and return the first
/// difference: missing or extra level, then total size, then order count.
#[must_use]
pub fn compare_l2(
    recorded_bids: &[L2Level],
    recorded_asks: &[L2Level],
    candidate_bids: &[L2Level],
    candidate_asks: &[L2Level],
) -> Option<LevelDiff> {
    compare_side(
        Side::Bid,
        recorded_bids,
        candidate_bids,
        l2_view,
        compare_l2_level,
    )
    .or_else(|| {
        compare_side(
            Side::Ask,
            recorded_asks,
            candidate_asks,
            l2_view,
            compare_l2_level,
        )
    })
}

/// (price, total size, order count) of a level.
fn l4_view(l: &Level) -> (Px, Lots, usize) {
    (l.px, l4_total(l), l.orders.len())
}

/// A level total already checked by [`compare_l4`]'s overflow scan.
fn l4_total(l: &Level) -> Lots {
    let Some(total) = l.total() else {
        unreachable!("compare_l4 reports overflowing levels before walking them");
    };
    total
}

fn l2_view(l: &L2Level) -> (Px, Lots, usize) {
    (l.px, l.sz, usize::try_from(l.n).unwrap_or(usize::MAX))
}

fn compare_l2_level(recorded: &L2Level, candidate: &L2Level) -> Option<Field> {
    if recorded.sz != candidate.sz {
        return Some(Field::Size {
            recorded: recorded.sz,
            candidate: candidate.sz,
        });
    }
    if recorded.n != candidate.n {
        return Some(Field::OrderCount {
            recorded: l2_view(recorded).2,
            candidate: l2_view(candidate).2,
        });
    }
    None
}

/// Does `a` come before `b` when walking `side` from best to worst?
fn better(side: Side, a: Px, b: Px) -> bool {
    match side {
        Side::Bid => a > b,
        Side::Ask => a < b,
    }
}

fn compare_side<L>(
    side: Side,
    recorded: &[L],
    candidate: &[L],
    view: fn(&L) -> (Px, Lots, usize),
    compare: fn(&L, &L) -> Option<Field>,
) -> Option<LevelDiff> {
    let missing = |l: &L| {
        let (px, total, orders) = view(l);
        LevelDiff {
            side,
            px,
            field: Field::MissingLevel {
                recorded: total,
                recorded_orders: orders,
            },
        }
    };
    let extra = |l: &L| {
        let (px, total, orders) = view(l);
        LevelDiff {
            side,
            px,
            field: Field::ExtraLevel {
                candidate: total,
                candidate_orders: orders,
            },
        }
    };
    let mut r = recorded.iter().peekable();
    let mut c = candidate.iter().peekable();
    loop {
        match (r.peek(), c.peek()) {
            (None, None) => return None,
            (Some(rl), None) => return Some(missing(rl)),
            (None, Some(cl)) => return Some(extra(cl)),
            (Some(rl), Some(cl)) => {
                let (rpx, cpx) = (view(rl).0, view(cl).0);
                if rpx == cpx {
                    if let Some(field) = compare(rl, cl) {
                        return Some(LevelDiff {
                            side,
                            px: rpx,
                            field,
                        });
                    }
                    r.next();
                    c.next();
                } else {
                    // Prices differ: whichever is better comes first in the walk, so it
                    // is the one the other book lacks.
                    return Some(if better(side, rpx, cpx) {
                        missing(rl)
                    } else {
                        extra(cl)
                    });
                }
            }
        }
    }
}

fn compare_level(recorded: &Level, candidate: &Level) -> Option<Field> {
    let (rt, ct) = (l4_total(recorded), l4_total(candidate));
    if rt != ct {
        return Some(Field::Size {
            recorded: rt,
            candidate: ct,
        });
    }
    if recorded.orders.len() != candidate.orders.len() {
        return Some(Field::OrderCount {
            recorded: recorded.orders.len(),
            candidate: candidate.orders.len(),
        });
    }
    for (position, (&(ro, rs), &(co, cs))) in recorded
        .orders
        .iter()
        .zip(candidate.orders.iter())
        .enumerate()
    {
        if ro != co {
            return Some(Field::QueuePosition {
                position,
                recorded: ro,
                candidate: co,
            });
        }
        if rs != cs {
            return Some(Field::OrderSize {
                position,
                oid: ro,
                recorded: rs,
                candidate: cs,
            });
        }
    }
    None
}
