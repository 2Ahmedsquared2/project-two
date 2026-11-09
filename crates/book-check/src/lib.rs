//! Replays a recorded dataset through a candidate order book and reports the
//! first place the candidate disagrees with the recorded book.
//!
//! The candidate is anything that implements [`Candidate`]: the reference
//! book from `hl-book-ref` today, every faster design later. After each block
//! that has a recorded book the two snapshots are compared level by level in
//! integer ticks and lots; the first [`Divergence`] names the block, side,
//! price level and the field that differs (size, order count, queue position,
//! a per-order size, or a level present on one side only).

use std::fmt;
use std::io::BufRead;

use hl_book_ref::{Book, BookOp, BookSnapshot, Level, Lots, Oid, Px, Side};
use hl_dataset::{ReadError, Reader};

/// An order book under test.
pub trait Candidate {
    /// Why an op could not be applied.
    type Error: fmt::Display;

    /// Apply one per-order effect.
    fn apply(&mut self, op: &BookOp) -> Result<(), Self::Error>;

    /// The whole book as plain data, bids highest first and asks lowest first.
    fn snapshot(&self) -> BookSnapshot;
}

impl Candidate for Book {
    type Error = hl_book_ref::ApplyError;

    fn apply(&mut self, op: &BookOp) -> Result<(), Self::Error> {
        Book::apply(self, op)
    }

    fn snapshot(&self) -> BookSnapshot {
        Book::snapshot(self)
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
        }
    }
}

/// The first disagreement between the recorded book and the candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    /// Block after which the books differ.
    pub height: u64,
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
            "divergence at block {} on the {side} side at price {} ticks: {}",
            self.height, self.px.0, self.field
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
    #[error("block {height}, op {op_index} ({op:?}): candidate rejected it: {message}")]
    Apply {
        /// Block height.
        height: u64,
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
    /// Ops applied.
    pub ops: u64,
    /// Recorded books compared (blocks with a book record).
    pub books_compared: u64,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ok: {} blocks, {} ops, {} recorded books compared, no divergence",
            self.blocks, self.ops, self.books_compared
        )
    }
}

/// Replay every block of `reader` through `candidate`, comparing after each
/// block that has a recorded book. Stops at the first problem.
pub fn check<R: BufRead, C: Candidate>(
    reader: Reader<R>,
    candidate: &mut C,
) -> Result<Report, CheckError> {
    let mut report = Report::default();
    for step in reader.steps() {
        let step = step?;
        let height = step.block.height;
        for (op_index, op) in step.block.ops.iter().enumerate() {
            candidate.apply(op).map_err(|e| CheckError::Apply {
                height,
                op_index,
                op: *op,
                message: e.to_string(),
            })?;
            report.ops += 1;
        }
        report.blocks += 1;
        if let Some(recorded) = &step.book {
            if let Some(d) = compare_books(height, recorded, &candidate.snapshot()) {
                return Err(CheckError::Divergence(d));
            }
            report.books_compared += 1;
        }
    }
    Ok(report)
}

/// Compare two books level by level and return the first difference. Bids are
/// checked before asks; within a side, levels are walked from best to worst;
/// within a level, total size, then order count, then each queue position.
#[must_use]
pub fn compare_books(
    height: u64,
    recorded: &BookSnapshot,
    candidate: &BookSnapshot,
) -> Option<Divergence> {
    compare_side(height, Side::Bid, &recorded.bids, &candidate.bids)
        .or_else(|| compare_side(height, Side::Ask, &recorded.asks, &candidate.asks))
}

/// Does `a` come before `b` when walking `side` from best to worst?
fn better(side: Side, a: Px, b: Px) -> bool {
    match side {
        Side::Bid => a > b,
        Side::Ask => a < b,
    }
}

fn compare_side(
    height: u64,
    side: Side,
    recorded: &[Level],
    candidate: &[Level],
) -> Option<Divergence> {
    let mut r = recorded.iter().peekable();
    let mut c = candidate.iter().peekable();
    loop {
        match (r.peek(), c.peek()) {
            (None, None) => return None,
            (Some(rl), None) => return Some(missing(height, side, rl)),
            (None, Some(cl)) => return Some(extra(height, side, cl)),
            (Some(rl), Some(cl)) if rl.px == cl.px => {
                if let Some(field) = compare_level(rl, cl) {
                    return Some(Divergence {
                        height,
                        side,
                        px: rl.px,
                        field,
                    });
                }
                r.next();
                c.next();
            }
            (Some(rl), Some(cl)) => {
                // Prices differ: whichever is better comes first in the walk, so it is
                // the one the other book lacks.
                return Some(if better(side, rl.px, cl.px) {
                    missing(height, side, rl)
                } else {
                    extra(height, side, cl)
                });
            }
        }
    }
}

fn missing(height: u64, side: Side, level: &Level) -> Divergence {
    Divergence {
        height,
        side,
        px: level.px,
        field: Field::MissingLevel {
            recorded: level.total(),
            recorded_orders: level.orders.len(),
        },
    }
}

fn extra(height: u64, side: Side, level: &Level) -> Divergence {
    Divergence {
        height,
        side,
        px: level.px,
        field: Field::ExtraLevel {
            candidate: level.total(),
            candidate_orders: level.orders.len(),
        },
    }
}

fn compare_level(recorded: &Level, candidate: &Level) -> Option<Field> {
    let (rt, ct) = (recorded.total(), candidate.total());
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
