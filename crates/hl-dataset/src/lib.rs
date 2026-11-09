//! Versioned on-disk format for a recorded `HyperCore` dataset (format v2).
//!
//! A dataset is a contiguous run of blocks, each with the transactions it
//! carried (raw signed actions, in the file order of the block) and, when the
//! capture knows them, the per-order effects those transactions had on each
//! asset's resting book. After any block, zero or more recorded book states
//! follow: full per-order `l4_book` states or aggregated `l2_book` states
//! (price, total size, order count), one per asset at most. It is the ground
//! truth `book-check` replays a candidate book against.
//!
//! The format is specified in `docs/dataset-format.md` at the repository root;
//! this crate is its reader and writer and enforces its structural rules on
//! both paths. Every price and size is an integer (ticks and lots, scaled by
//! the header's per-asset [`AssetMeta`]); the original decimal strings may sit
//! beside them for audit and are checked against the integers on read.
//!
//! The encoding is JSON Lines. That is not a hot-path format and is not meant
//! to be: it is written by a capture tool, read by a checker, and inspected by
//! people with `head` and `jq`.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};

use hl_book_ref::{BookSnapshot, Level, Lots, Oid, Px};
use serde::{Deserialize, Serialize};

pub mod decimal;

pub use records::{AssetOp, Header};

/// The format version this crate writes and the only one it reads.
pub const FORMAT_VERSION: u32 = 2;

mod records {
    //! `#[serde(flatten)]` expands to a visitor over arbitrary JSON values whose
    //! generated code names `f32`/`f64`, which the workspace `clippy.toml` bans.
    //! No float reaches any field of ours; the ban is lifted for this module only.
    #![allow(clippy::disallowed_types)]

    use std::collections::BTreeMap;

    use hl_book_ref::BookOp;
    use serde::{Deserialize, Serialize};

    use super::AssetMeta;

    /// The first record of a dataset: what was captured and how to read the integers.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Header {
        /// Format version; see [`FORMAT_VERSION`](super::FORMAT_VERSION).
        pub format_version: u32,
        /// Chain the blocks come from, e.g. `mainnet` or `testnet`.
        pub chain: String,
        /// Height of the first block in the file.
        pub first_height: u64,
        /// Free-text provenance: capture tool, endpoints, dates. Never parsed.
        pub source: String,
        /// Every asset whose ops or books appear in the file.
        pub assets: Vec<AssetMeta>,
        /// Any other header fields. Preserved verbatim so a capture tool can add
        /// provenance before this crate knows the field.
        #[serde(flatten)]
        pub extra: BTreeMap<String, serde_json::Value>,
    }

    /// One per-order book effect, tagged with the asset whose book it changes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct AssetOp {
        /// Asset index (perp: position in `meta.universe`; spot: 10000 + index).
        pub asset: u32,
        /// The effect.
        #[serde(flatten)]
        pub op: BookOp,
    }
}

/// Per-asset metadata needed to read integer prices and sizes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetMeta {
    /// Hyperliquid asset index: perp = position in `meta.universe`, spot = 10000 + spot index.
    pub asset_index: u32,
    /// Coin name, e.g. `BTC` or `PURR/USDC`.
    pub name: String,
    /// `szDecimals`: `size = sz_lots / 10^sz_decimals`.
    pub sz_decimals: u32,
    /// Spot (true) or perp (false).
    pub is_spot: bool,
    /// Price grid: `price = px_ticks / 10^max_price_decimals`. Must equal
    /// `6 - sz_decimals` for perps and `8 - sz_decimals` for spot.
    pub max_price_decimals: u32,
}

/// Why an [`AssetMeta`] is not usable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AssetMetaError {
    /// `sz_decimals` exceeds the decimal budget (6 perp, 8 spot).
    #[error("sz_decimals {sz_decimals} exceeds the {budget}-decimal budget")]
    SzDecimalsTooLarge {
        /// The value given.
        sz_decimals: u32,
        /// 6 for perps, 8 for spot.
        budget: u32,
    },
    /// `max_price_decimals` disagrees with the rule.
    #[error("max_price_decimals {found}, expected {expected}")]
    PriceDecimalsMismatch {
        /// Value the rule gives.
        expected: u32,
        /// Value found.
        found: u32,
    },
}

impl AssetMeta {
    /// Build the metadata, deriving `max_price_decimals` from Hyperliquid's
    /// rule: at most `6 - szDecimals` price decimals for perps and
    /// `8 - szDecimals` for spot (Hyperliquid docs, "Tick and lot size").
    pub fn new(
        asset_index: u32,
        name: &str,
        sz_decimals: u32,
        is_spot: bool,
    ) -> Result<Self, AssetMetaError> {
        Ok(Self {
            asset_index,
            name: name.to_owned(),
            sz_decimals,
            is_spot,
            max_price_decimals: Self::price_decimals(sz_decimals, is_spot)?,
        })
    }

    fn price_decimals(sz_decimals: u32, is_spot: bool) -> Result<u32, AssetMetaError> {
        let budget: u32 = if is_spot { 8 } else { 6 };
        budget
            .checked_sub(sz_decimals)
            .ok_or(AssetMetaError::SzDecimalsTooLarge {
                sz_decimals,
                budget,
            })
    }

    /// Check a deserialised value against the rule.
    pub fn validate(&self) -> Result<(), AssetMetaError> {
        let expected = Self::price_decimals(self.sz_decimals, self.is_spot)?;
        if expected != self.max_price_decimals {
            return Err(AssetMetaError::PriceDecimalsMismatch {
                expected,
                found: self.max_price_decimals,
            });
        }
        Ok(())
    }

    /// A decimal price string in integer ticks.
    pub fn px_ticks(&self, px: &str) -> Result<u64, decimal::DecimalError> {
        decimal::parse_units(px, self.max_price_decimals)
    }

    /// A decimal size string in integer lots.
    pub fn sz_lots(&self, sz: &str) -> Result<u64, decimal::DecimalError> {
        decimal::parse_units(sz, self.sz_decimals)
    }
}

/// A raw JSON value kept byte for byte (key order, spacing, number strings):
/// the signed action exactly as the source returned it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawJson(Box<serde_json::value::RawValue>);

impl RawJson {
    /// Wrap a JSON text. Fails if it is not valid JSON.
    pub fn from_string(json: String) -> Result<Self, serde_json::Error> {
        serde_json::value::RawValue::from_string(json).map(Self)
    }

    /// The JSON text, exactly as stored.
    #[must_use]
    pub fn get(&self) -> &str {
        self.0.get()
    }
}

impl PartialEq for RawJson {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl Eq for RawJson {}

/// One transaction as listed in the block (explorer `blockDetails` order,
/// which is `HyperCore`'s execution order).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tx {
    /// Transaction time in ms since the Unix epoch, when the source gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
    /// Signer address.
    pub user: String,
    /// Transaction hash.
    pub hash: String,
    /// `null` if the action succeeded, else the error string.
    #[serde(default)]
    pub error: Option<String>,
    /// The signed action as submitted, raw.
    pub action: RawJson,
}

/// One block: its transactions and, when known, their per-order book effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    /// Block height.
    pub height: u64,
    /// Block time in ms since the Unix epoch, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
    /// Block hash, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// Proposer address, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposer: Option<String>,
    /// Transactions in file order. May be empty.
    #[serde(default)]
    pub txs: Vec<Tx>,
    /// Per-order book effects in application order, across assets. Empty when
    /// the capture only has raw actions.
    #[serde(default)]
    pub ops: Vec<AssetOp>,
}

/// One resting order in an L4 level.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct L4Order {
    /// Order id.
    pub oid: u64,
    /// Remaining size in lots.
    pub sz_lots: u64,
    /// Owner address, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// One L4 price level: its orders in queue order, front first.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct L4Level {
    /// Price in ticks.
    pub px_ticks: u64,
    /// Original decimal price string, for audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub px: Option<String>,
    /// Orders, front of queue first.
    pub orders: Vec<L4Order>,
}

/// Full per-order book of one asset after a block.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct L4Book {
    /// Height of the block this state follows.
    pub height: u64,
    /// Asset index.
    pub asset: u32,
    /// Source timestamp, ms, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
    /// Bid levels, highest first.
    pub bids: Vec<L4Level>,
    /// Ask levels, lowest first.
    pub asks: Vec<L4Level>,
}

impl L4Book {
    /// As a reference-book snapshot (drops users and decimal strings).
    #[must_use]
    pub fn to_snapshot(&self) -> BookSnapshot {
        let conv = |levels: &[L4Level]| -> Vec<Level> {
            levels
                .iter()
                .map(|l| Level {
                    px: Px(l.px_ticks),
                    orders: l
                        .orders
                        .iter()
                        .map(|o| (Oid(o.oid), Lots(o.sz_lots)))
                        .collect(),
                })
                .collect()
        };
        BookSnapshot {
            bids: conv(&self.bids),
            asks: conv(&self.asks),
        }
    }

    /// From a reference-book snapshot.
    #[must_use]
    pub fn from_snapshot(height: u64, asset: u32, snap: &BookSnapshot) -> Self {
        let conv = |levels: &[Level]| -> Vec<L4Level> {
            levels
                .iter()
                .map(|l| L4Level {
                    px_ticks: l.px.0,
                    px: None,
                    orders: l
                        .orders
                        .iter()
                        .map(|&(oid, sz)| L4Order {
                            oid: oid.0,
                            sz_lots: sz.0,
                            user: None,
                        })
                        .collect(),
                })
                .collect()
        };
        Self {
            height,
            asset,
            time_ms: None,
            bids: conv(&snap.bids),
            asks: conv(&snap.asks),
        }
    }
}

/// One aggregated L2 level, as in the public `l2Book` (`px`, `sz`, `n`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct L2Level {
    /// Price in ticks.
    pub px_ticks: u64,
    /// Total size in lots.
    pub sz_lots: u64,
    /// Number of resting orders.
    pub n: u32,
    /// Original decimal price string, for audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub px: Option<String>,
    /// Original decimal size string, for audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sz: Option<String>,
}

/// Aggregated book of one asset after a block.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct L2Book {
    /// Height of the block this state is anchored to.
    pub height: u64,
    /// Asset index.
    pub asset: u32,
    /// Source timestamp, ms (public `l2Book.time`), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
    /// Levels per side the source returns at most (public `l2Book`: 20). When a
    /// side has exactly this many levels the book may continue beyond them, so
    /// the checker compares only the top `depth`. `None`: the whole book.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// Bid levels, highest first.
    pub bids: Vec<L2Level>,
    /// Ask levels, lowest first.
    pub asks: Vec<L2Level>,
}

/// A recorded book state: aggregated (`l2_book`) or per-order (`l4_book`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BookState {
    /// Aggregated levels.
    L2(L2Book),
    /// Per-order queues.
    L4(L4Book),
}

impl BookState {
    /// Height of the block this state follows.
    #[must_use]
    pub fn height(&self) -> u64 {
        match self {
            BookState::L2(b) => b.height,
            BookState::L4(b) => b.height,
        }
    }

    /// Asset index.
    #[must_use]
    pub fn asset(&self) -> u32 {
        match self {
            BookState::L2(b) => b.asset,
            BookState::L4(b) => b.asset,
        }
    }
}

/// One block and the book states recorded after it (possibly none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The block.
    pub block: Block,
    /// Recorded states after the block, in file order, at most one per asset.
    pub books: Vec<BookState>,
}

/// Why a dataset could not be read. `line` is 1-based.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// Underlying I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// A line is not a valid record.
    #[error("line {line}: invalid record: {source}")]
    Json {
        /// 1-based line number.
        line: usize,
        /// The JSON error.
        source: serde_json::Error,
    },
    /// A line's `kind` is not one this version knows.
    #[error("line {line}: unknown record kind {kind:?}")]
    UnknownKind {
        /// 1-based line number.
        line: usize,
        /// The kind found.
        kind: String,
    },
    /// The file is empty or its first record is not a header.
    #[error("the first line must be a header record")]
    MissingHeader,
    /// A second header appeared after the first.
    #[error("line {line}: a second header record")]
    DuplicateHeader {
        /// 1-based line number.
        line: usize,
    },
    /// The header's version is not one this crate reads.
    #[error("unsupported format_version {0} (this reader supports {FORMAT_VERSION})")]
    UnsupportedVersion(u32),
    /// A header asset entry is invalid or repeated.
    #[error("header: asset {asset}: {reason}")]
    BadAssetMeta {
        /// Asset index.
        asset: u32,
        /// What is wrong.
        reason: String,
    },
    /// An op or book names an asset the header does not declare.
    #[error("line {line}: asset {asset} is not declared in the header")]
    UnknownAsset {
        /// Asset index.
        asset: u32,
        /// 1-based line number.
        line: usize,
    },
    /// Block heights are not contiguous from `first_height`.
    #[error("line {line}: expected block height {expected}, found {found}")]
    HeightGap {
        /// The height the reader expected next.
        expected: u64,
        /// The height the record carried.
        found: u64,
        /// 1-based line number.
        line: usize,
    },
    /// A book record does not follow the block with its height.
    #[error("line {line}: book record for height {height} does not follow its block")]
    BookWithoutBlock {
        /// The height the book record carried.
        height: u64,
        /// 1-based line number.
        line: usize,
    },
    /// A second book for the same asset after one block.
    #[error("line {line}: a second book for asset {asset} after the same block")]
    DuplicateBook {
        /// Asset index.
        asset: u32,
        /// 1-based line number.
        line: usize,
    },
    /// Levels are not strictly best-first on a side.
    #[error("line {line}: {side} levels are not strictly best-first at {px_ticks} ticks")]
    UnsortedLevels {
        /// `bids` or `asks`.
        side: &'static str,
        /// First out-of-order price.
        px_ticks: u64,
        /// 1-based line number.
        line: usize,
    },
    /// An audit decimal string disagrees with its integer.
    #[error("line {line}: decimal {text:?} is not {units} units")]
    DecimalMismatch {
        /// The decimal string.
        text: String,
        /// The integer beside it.
        units: u64,
        /// 1-based line number.
        line: usize,
    },
}

/// Why a record could not be written.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// Underlying I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// Serialisation failure (should not happen for these types).
    #[error("serialisation error: {0}")]
    Json(#[from] serde_json::Error),
    /// A header asset entry is invalid or repeated.
    #[error("header: asset {0}: invalid or repeated")]
    BadAssetMeta(u32),
    /// An op or book names an undeclared asset.
    #[error("asset {0} is not declared in the header")]
    UnknownAsset(u32),
    /// A block's height does not continue the run.
    #[error("expected block height {expected}, found {found}")]
    HeightGap {
        /// The height the writer expected next.
        expected: u64,
        /// The height given.
        found: u64,
    },
    /// A book was written before any block, or for another height than the last block.
    #[error("a book record must follow the block with its height")]
    BookWithoutBlock,
    /// A second book for the same asset after one block.
    #[error("a block has at most one book per asset (asset {0})")]
    DuplicateBook(u32),
}

fn declared_assets(header: &Header) -> Result<BTreeSet<u32>, (u32, String)> {
    let mut seen = BTreeSet::new();
    for a in &header.assets {
        a.validate().map_err(|e| (a.asset_index, e.to_string()))?;
        if !seen.insert(a.asset_index) {
            return Err((a.asset_index, "declared twice".to_owned()));
        }
    }
    Ok(seen)
}

/// Streaming dataset writer. Enforces the structural rules as it goes.
#[derive(Debug)]
pub struct Writer<W: Write> {
    out: W,
    assets: BTreeSet<u32>,
    next_height: u64,
    last_block: Option<u64>,
    books_after_block: BTreeSet<u32>,
}

impl<W: Write> Writer<W> {
    /// Start a dataset by writing its header.
    pub fn new(mut out: W, header: &Header) -> Result<Self, WriteError> {
        let assets = declared_assets(header).map_err(|(a, _)| WriteError::BadAssetMeta(a))?;
        let mut header = header.clone();
        header.extra.remove("kind");
        write_tagged(&mut out, "header", &header)?;
        Ok(Self {
            out,
            assets,
            next_height: header.first_height,
            last_block: None,
            books_after_block: BTreeSet::new(),
        })
    }

    /// Append a block. Its height must be the next in the run.
    pub fn write_block(&mut self, block: &Block) -> Result<(), WriteError> {
        if block.height != self.next_height {
            return Err(WriteError::HeightGap {
                expected: self.next_height,
                found: block.height,
            });
        }
        if let Some(op) = block.ops.iter().find(|o| !self.assets.contains(&o.asset)) {
            return Err(WriteError::UnknownAsset(op.asset));
        }
        write_tagged(&mut self.out, "block", block)?;
        self.last_block = Some(block.height);
        self.next_height = block.height + 1;
        self.books_after_block.clear();
        Ok(())
    }

    /// Append a book state after the block just written (at most one per asset).
    pub fn write_book(&mut self, book: &BookState) -> Result<(), WriteError> {
        if self.last_block != Some(book.height()) {
            return Err(WriteError::BookWithoutBlock);
        }
        let asset = book.asset();
        if !self.assets.contains(&asset) {
            return Err(WriteError::UnknownAsset(asset));
        }
        if !self.books_after_block.insert(asset) {
            return Err(WriteError::DuplicateBook(asset));
        }
        match book {
            BookState::L2(b) => write_tagged(&mut self.out, "l2_book", b),
            BookState::L4(b) => write_tagged(&mut self.out, "l4_book", b),
        }
    }

    /// Flush and return the underlying writer.
    pub fn finish(mut self) -> Result<W, WriteError> {
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Write `{"kind":<kind>, ...fields of value}` and a newline. `value` must
/// serialise to a JSON object.
fn write_tagged<W: Write, T: Serialize>(
    out: &mut W,
    kind: &str,
    value: &T,
) -> Result<(), WriteError> {
    let body = serde_json::to_string(value)?;
    let rest = body
        .strip_prefix('{')
        .ok_or_else(|| std::io::Error::other("record did not serialise to an object"))?;
    let sep = if rest == "}" { "" } else { "," };
    writeln!(
        out,
        "{{\"kind\":{}{sep}{rest}",
        serde_json::to_string(kind)?
    )?;
    Ok(())
}

#[derive(Deserialize)]
struct KindProbe {
    kind: Option<String>,
    format_version: Option<u32>,
}

enum Record {
    Block(Block),
    Book(BookState),
}

/// Streaming dataset reader. Validates the header on construction and the
/// structural rules as records are read.
#[derive(Debug)]
pub struct Reader<R: BufRead> {
    lines: std::io::Lines<R>,
    line_no: usize,
    header: Header,
    assets: BTreeSet<u32>,
    next_height: u64,
}

fn parse<'a, T: Deserialize<'a>>(line: &'a str, line_no: usize) -> Result<T, ReadError> {
    serde_json::from_str(line).map_err(|source| ReadError::Json {
        line: line_no,
        source,
    })
}

impl<R: BufRead> Reader<R> {
    /// Open a dataset: reads and validates the header line.
    pub fn new(input: R) -> Result<Self, ReadError> {
        let mut lines = input.lines();
        let first = match lines.next() {
            Some(line) => line?,
            None => return Err(ReadError::MissingHeader),
        };
        let probe: KindProbe = parse(&first, 1)?;
        if probe.kind.as_deref() != Some("header") {
            return Err(ReadError::MissingHeader);
        }
        // Check the version before the full shape, so an old file says "unsupported version".
        match probe.format_version {
            Some(FORMAT_VERSION) => {}
            Some(v) => return Err(ReadError::UnsupportedVersion(v)),
            None => return Err(ReadError::MissingHeader),
        }
        let mut header: Header = parse(&first, 1)?;
        header.extra.remove("kind");
        let assets = declared_assets(&header)
            .map_err(|(asset, reason)| ReadError::BadAssetMeta { asset, reason })?;
        Ok(Self {
            lines,
            line_no: 1,
            next_height: header.first_height,
            header,
            assets,
        })
    }

    /// The dataset header.
    #[must_use]
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Iterate over blocks, each with the book states recorded after it.
    /// Errors end the iteration.
    pub fn steps(self) -> Steps<R> {
        Steps {
            reader: self,
            pending: None,
            done: false,
        }
    }

    fn meta(&self, asset: u32) -> Option<&AssetMeta> {
        self.header.assets.iter().find(|a| a.asset_index == asset)
    }

    /// Read the next non-header record, validating it. `Ok(None)` at end of file.
    fn next_record(&mut self) -> Result<Option<Record>, ReadError> {
        loop {
            let Some(line) = self.lines.next() else {
                return Ok(None);
            };
            let line = line?;
            self.line_no += 1;
            let line_no = self.line_no;
            if line.trim().is_empty() {
                continue;
            }
            let probe: KindProbe = parse(&line, line_no)?;
            let kind = probe.kind.unwrap_or_default();
            return match kind.as_str() {
                "header" => Err(ReadError::DuplicateHeader { line: line_no }),
                "block" => {
                    let block: Block = parse(&line, line_no)?;
                    if block.height != self.next_height {
                        return Err(ReadError::HeightGap {
                            expected: self.next_height,
                            found: block.height,
                            line: line_no,
                        });
                    }
                    if let Some(op) = block.ops.iter().find(|o| !self.assets.contains(&o.asset)) {
                        return Err(ReadError::UnknownAsset {
                            asset: op.asset,
                            line: line_no,
                        });
                    }
                    self.next_height = block.height + 1;
                    Ok(Some(Record::Block(block)))
                }
                "l2_book" => {
                    let book: L2Book = parse(&line, line_no)?;
                    self.validate_l2(&book, line_no)?;
                    Ok(Some(Record::Book(BookState::L2(book))))
                }
                "l4_book" => {
                    let book: L4Book = parse(&line, line_no)?;
                    self.validate_l4(&book, line_no)?;
                    Ok(Some(Record::Book(BookState::L4(book))))
                }
                _ => Err(ReadError::UnknownKind {
                    line: line_no,
                    kind,
                }),
            };
        }
    }

    fn known_meta(&self, asset: u32, line: usize) -> Result<&AssetMeta, ReadError> {
        self.meta(asset)
            .ok_or(ReadError::UnknownAsset { asset, line })
    }

    fn validate_l2(&self, book: &L2Book, line: usize) -> Result<(), ReadError> {
        let meta = self.known_meta(book.asset, line)?;
        for (side, levels) in [("bids", &book.bids), ("asks", &book.asks)] {
            check_sorted(side, levels.iter().map(|l| l.px_ticks), line)?;
            for l in levels {
                audit(l.px.as_deref(), l.px_ticks, meta.max_price_decimals, line)?;
                audit(l.sz.as_deref(), l.sz_lots, meta.sz_decimals, line)?;
            }
        }
        Ok(())
    }

    fn validate_l4(&self, book: &L4Book, line: usize) -> Result<(), ReadError> {
        let meta = self.known_meta(book.asset, line)?;
        for (side, levels) in [("bids", &book.bids), ("asks", &book.asks)] {
            check_sorted(side, levels.iter().map(|l| l.px_ticks), line)?;
            for l in levels {
                audit(l.px.as_deref(), l.px_ticks, meta.max_price_decimals, line)?;
            }
        }
        Ok(())
    }
}

/// Bids strictly descending, asks strictly ascending.
fn check_sorted(
    side: &'static str,
    mut pxs: impl Iterator<Item = u64>,
    line: usize,
) -> Result<(), ReadError> {
    let Some(mut prev) = pxs.next() else {
        return Ok(());
    };
    for px in pxs {
        let ok = if side == "bids" { px < prev } else { px > prev };
        if !ok {
            return Err(ReadError::UnsortedLevels {
                side,
                px_ticks: px,
                line,
            });
        }
        prev = px;
    }
    Ok(())
}

fn audit(text: Option<&str>, units: u64, decimals: u32, line: usize) -> Result<(), ReadError> {
    let Some(text) = text else { return Ok(()) };
    if decimal::parse_units(text, decimals) == Ok(units) {
        Ok(())
    } else {
        Err(ReadError::DecimalMismatch {
            text: text.to_owned(),
            units,
            line,
        })
    }
}

/// Iterator over [`Step`]s; see [`Reader::steps`].
#[derive(Debug)]
pub struct Steps<R: BufRead> {
    reader: Reader<R>,
    pending: Option<Step>,
    done: bool,
}

impl<R: BufRead> Steps<R> {
    fn advance(&mut self) -> Result<Option<Step>, ReadError> {
        loop {
            match self.reader.next_record()? {
                None => return Ok(self.pending.take()),
                Some(Record::Block(block)) => {
                    let next = Step {
                        block,
                        books: Vec::new(),
                    };
                    if let Some(prev) = self.pending.replace(next) {
                        return Ok(Some(prev));
                    }
                }
                Some(Record::Book(book)) => {
                    let line = self.reader.line_no;
                    let Some(step) = self
                        .pending
                        .as_mut()
                        .filter(|s| s.block.height == book.height())
                    else {
                        return Err(ReadError::BookWithoutBlock {
                            height: book.height(),
                            line,
                        });
                    };
                    if step.books.iter().any(|b| b.asset() == book.asset()) {
                        return Err(ReadError::DuplicateBook {
                            asset: book.asset(),
                            line,
                        });
                    }
                    step.books.push(book);
                }
            }
        }
    }
}

impl<R: BufRead> Iterator for Steps<R> {
    type Item = Result<Step, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.advance() {
            Ok(Some(step)) => Some(Ok(step)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}
