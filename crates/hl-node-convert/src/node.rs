//! The node's per-block output lines and the events inside them.
//!
//! Schemas, read from primary sources (nothing copied):
//!
//! - batch line `{local_time, block_time, block_number, events}`: the
//!   `hyperliquid-dex/node` README, flags `--batch-by-block` and
//!   `--stream-with-block-info`, and `Batch` in `hyperliquid-dex/order_book_server`
//!   `server/src/types/node_data.rs` (times are chrono `NaiveDateTime`, UTC);
//! - raw book diff `{user, oid, coin, px, side?, raw_book_diff}`: `NodeDataOrderDiff`
//!   and `OrderDiff` in `order_book_server` (`server/src/types/node_data.rs`,
//!   `server/src/types/mod.rs`) and the Hyperliquid docs page "L1 data schemas",
//!   which also shows a `side` of `"Bid"`/`"Ask"` that `order_book_server` does not read;
//! - order status `{time, user, status, order}`: `NodeDataOrderStatus` and
//!   `L4Order` in `order_book_server` and the same docs page.

use std::io::BufRead;

use serde::de::DeserializeOwned;
use serde::Deserialize;

/// A side as the node writes it: `"B"`/`"A"` in order statuses and snapshots,
/// `"Bid"`/`"Ask"` in the docs' raw book diff example. All four are accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub enum NodeSide {
    /// Buy side.
    #[serde(rename = "B", alias = "Bid")]
    Bid,
    /// Sell side.
    #[serde(rename = "A", alias = "Ask")]
    Ask,
}

impl From<NodeSide> for hl_book_ref::Side {
    fn from(s: NodeSide) -> Self {
        match s {
            NodeSide::Bid => hl_book_ref::Side::Bid,
            NodeSide::Ask => hl_book_ref::Side::Ask,
        }
    }
}

/// What a raw book diff did to one resting order.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffKind {
    /// The order came to rest with `sz`, at the back of its level or directly
    /// in front of `insert_before` (priority ALO).
    #[serde(rename_all = "camelCase")]
    New {
        /// Resting size, decimal string.
        sz: String,
        /// Oid of the resting order this one goes in front of.
        #[serde(default)]
        insert_before: Option<u64>,
    },
    /// The resting size changed in place.
    #[serde(rename_all = "camelCase")]
    Update {
        /// Size before, decimal string.
        orig_sz: String,
        /// Size after, decimal string.
        new_sz: String,
    },
    /// The order left the book.
    Remove,
}

/// One event of `node_raw_book_diffs`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct BookDiff {
    /// Owner address.
    pub user: String,
    /// Order id.
    pub oid: u64,
    /// Coin: perp name (`BTC`), spot `@index` or `PURR/USDC`, builder perp `dex:NAME`.
    pub coin: String,
    /// Limit price, decimal string.
    pub px: String,
    /// Side, when the line carries it (docs example yes, `order_book_server` does not read it).
    #[serde(default)]
    pub side: Option<NodeSide>,
    /// The change.
    pub raw_book_diff: DiffKind,
}

/// The order inside an order status event (only the fields the converter uses).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusOrder {
    /// Coin.
    pub coin: String,
    /// Side.
    pub side: NodeSide,
    /// Limit price, decimal string.
    pub limit_px: String,
    /// Size, decimal string.
    pub sz: String,
    /// Order id.
    pub oid: u64,
    /// Trigger (stop / take-profit) order.
    #[serde(default)]
    pub is_trigger: bool,
    /// Time in force (`Gtc`, `Alo`, `Ioc`), `null` for some trigger orders.
    #[serde(default)]
    pub tif: Option<String>,
}

/// One event of `node_order_statuses`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct OrderStatus {
    /// Status time, naive UTC.
    pub time: String,
    /// Owner address.
    pub user: String,
    /// `open`, `filled`, `canceled`, `triggered`, `rejected`, ...
    pub status: String,
    /// The order.
    pub order: StatusOrder,
}

impl OrderStatus {
    /// Whether this status puts the order on the book: `open` for a
    /// non-trigger order that is not IOC, or `triggered` for a trigger order.
    /// Same rule as `NodeDataOrderStatus::is_inserted_into_book` in
    /// `order_book_server`.
    #[must_use]
    pub fn is_inserted_into_book(&self) -> bool {
        if self.order.is_trigger {
            self.status == "triggered"
        } else {
            self.status == "open" && self.order.tif.as_deref() != Some("Ioc")
        }
    }
}

/// One block's events from one stream, with its block metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch<E> {
    /// Block height.
    pub block_number: u64,
    /// Block time, Unix ms.
    pub block_time_ms: u64,
    /// The events, in file order.
    pub events: Vec<E>,
}

/// Why a single line could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LineError {
    /// Not the batch schema, or an event that is not.
    #[error("bad json: {0}")]
    Json(String),
    /// `block_time` is not a naive UTC timestamp.
    #[error("bad block_time: {0}")]
    Time(String),
}

/// Why a node output stream could not be read. Lines are 1-based.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// Underlying I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// A line that does not parse.
    #[error("line {line}: {error}")]
    Line {
        /// Line number.
        line: usize,
        /// What is wrong.
        error: LineError,
    },
    /// A block number lower than one already read.
    #[error("line {line}: block {found} after block {previous}")]
    Regression {
        /// Line number.
        line: usize,
        /// Highest block read before.
        previous: u64,
        /// Block on this line.
        found: u64,
    },
    /// Two lines of the same block disagree on its time.
    #[error("line {line}: block {block} has two different block_time values")]
    TimeMismatch {
        /// Line number.
        line: usize,
        /// The block.
        block: u64,
    },
}

/// Parses one line `{local_time, block_time, block_number, events}`.
pub fn parse_batch<E: DeserializeOwned>(line: &str) -> Result<Batch<E>, LineError> {
    #[derive(Deserialize)]
    struct Raw<E> {
        block_time: String,
        block_number: u64,
        events: Vec<E>,
    }
    let raw: Raw<E> = serde_json::from_str(line).map_err(|e| LineError::Json(e.to_string()))?;
    let block_time_ms = hl_latency::parse_naive_utc_ms(&raw.block_time)
        .map_err(|e| LineError::Time(e.to_string()))?;
    Ok(Batch {
        block_number: raw.block_number,
        block_time_ms,
        events: raw.events,
    })
}

/// Reads a stream of batch lines and yields one [`Batch`] per block: lines
/// of the same block that follow each other (`--stream-with-block-info` may
/// write several) are merged in file order. Blank lines are skipped. A block
/// number lower than one already seen, or a block that reappears after
/// another, is a [`NodeError::Regression`].
pub struct BlockBatches<R, E> {
    lines: std::io::Lines<R>,
    line_no: usize,
    /// A block read but not yet yielded (more lines of it may follow).
    pending: Option<Batch<E>>,
    /// An error to yield after `pending`.
    failed: Option<NodeError>,
    /// Highest block number read so far.
    last: Option<u64>,
    done: bool,
}

impl<R: BufRead, E> BlockBatches<R, E> {
    /// Wraps a reader.
    pub fn new(reader: R) -> Self {
        Self {
            lines: reader.lines(),
            line_no: 0,
            pending: None,
            failed: None,
            last: None,
            done: false,
        }
    }

    /// Yields the pending block first, then the error; nothing after it.
    fn fail(&mut self, error: NodeError) -> Result<Batch<E>, NodeError> {
        self.done = true;
        match self.pending.take() {
            Some(b) => {
                self.failed = Some(error);
                Ok(b)
            }
            None => Err(error),
        }
    }
}

impl<R: BufRead, E: DeserializeOwned> Iterator for BlockBatches<R, E> {
    type Item = Result<Batch<E>, NodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return self.failed.take().map(Err);
        }
        loop {
            let Some(line) = self.lines.next() else {
                self.done = true;
                return self.pending.take().map(Ok);
            };
            self.line_no += 1;
            let line_no = self.line_no;
            let line = match line {
                Ok(l) => l,
                Err(e) => return Some(self.fail(NodeError::Io(e))),
            };
            if line.trim().is_empty() {
                continue;
            }
            let batch = match parse_batch::<E>(&line) {
                Ok(b) => b,
                Err(error) => {
                    return Some(self.fail(NodeError::Line {
                        line: line_no,
                        error,
                    }))
                }
            };
            if let Some(p) = self.pending.as_mut() {
                if p.block_number == batch.block_number {
                    if p.block_time_ms != batch.block_time_ms {
                        self.pending = None;
                        return Some(self.fail(NodeError::TimeMismatch {
                            line: line_no,
                            block: batch.block_number,
                        }));
                    }
                    p.events.extend(batch.events);
                    continue;
                }
            }
            if let Some(previous) = self.last {
                if batch.block_number <= previous {
                    return Some(self.fail(NodeError::Regression {
                        line: line_no,
                        previous,
                        found: batch.block_number,
                    }));
                }
            }
            self.last = Some(batch.block_number);
            if let Some(ready) = self.pending.replace(batch) {
                return Some(Ok(ready));
            }
        }
    }
}
