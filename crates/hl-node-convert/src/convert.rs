//! Node output to dataset: the join of diffs and statuses by block, the seed
//! block and the recorded check books.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, Write};

use hl_book_ref::{BookOp, Lots, Oid, Order, Px};
use hl_dataset::decimal::format_units;
use hl_dataset::{AssetMeta, AssetOp, Block, BookState, Header, Writer, FORMAT_VERSION};
use serde::Serialize;

use crate::node::{Batch, BlockBatches, BookDiff, DiffKind, NodeError, NodeSide, OrderStatus};
use crate::snapshot::{l4_book, order_units, L4Snapshot, SnapshotError};

/// How many examples [`Stats::examples`] keeps.
pub const MAX_EXAMPLES: usize = 20;

/// Conversion settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Header `chain`.
    pub chain: String,
    /// Header `source` (free-text provenance).
    pub source: String,
    /// Convert only these coins (node names). `None`: every asset in the table.
    pub coins: Option<BTreeSet<String>>,
    /// A height that neither stream has becomes an empty block (counted in
    /// [`Stats::gap_blocks`]). When false it is a [`ConvertError::Gap`].
    pub allow_gaps: bool,
    /// Turn the soft findings (unknown coins, `origSz` mismatches, check
    /// snapshots outside the converted range) into errors.
    pub strict: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            chain: "mainnet".to_owned(),
            source: String::new(),
            coins: None,
            allow_gaps: true,
            strict: false,
        }
    }
}

/// What the conversion saw. Printed as JSON by the CLI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    /// First block written.
    pub first_height: u64,
    /// Last block written.
    pub last_height: u64,
    /// Blocks written, the seed block included.
    pub blocks: u64,
    /// Heights neither stream had, written as empty blocks.
    pub gap_blocks: u64,
    /// Orders inserted by the seed block.
    pub seed_orders: u64,
    /// Trigger orders in the seed snapshot, not on the book, skipped.
    pub seed_triggers_skipped: u64,
    /// Diff events at or below the seed height, skipped.
    pub diffs_before_seed: u64,
    /// Diff events converted to ops.
    pub ops: u64,
    /// `insert` ops.
    pub inserts: u64,
    /// `insert_before` ops.
    pub inserts_before: u64,
    /// `resize` ops.
    pub resizes: u64,
    /// `cancel` ops.
    pub cancels: u64,
    /// `update` diffs with `newSz` 0, written as `cancel` (Hyperliquid Reason's
    /// advice; unverified that the node ever writes one).
    pub zero_size_updates: u64,
    /// `update` diffs whose `origSz` differs from the size the converter tracked.
    pub orig_sz_mismatches: u64,
    /// Diffs on a coin not in the asset table (e.g. builder perps), skipped.
    pub unknown_coin_diffs: u64,
    /// Diffs on a coin outside `--coin`, skipped.
    pub filtered_diffs: u64,
    /// Without a seed: diffs on orders that rested before the data started,
    /// skipped (the reference book never saw them).
    pub untracked_diffs: u64,
    /// Status batches for blocks with no diff batch.
    pub status_only_blocks: u64,
    /// `l4_book` records written from check snapshots.
    pub check_books: u64,
    /// Check snapshots whose height is outside the written blocks.
    pub checks_out_of_range: u64,
    /// The first few soft findings, one line each.
    pub examples: Vec<String>,
}

/// Why the conversion stopped.
#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    /// A node stream could not be read.
    #[error("{stream}: {error}")]
    Node {
        /// `diffs` or `statuses`.
        stream: &'static str,
        /// What went wrong.
        error: NodeError,
    },
    /// A snapshot could not be used.
    #[error("snapshot: {0}")]
    Snapshot(#[from] SnapshotError),
    /// The output could not be written.
    #[error("write: {0}")]
    Write(#[from] hl_dataset::WriteError),
    /// No seed and no diff batch: nothing to convert.
    #[error("no seed snapshot and no diff batches")]
    Empty,
    /// A `--coin` that is not in the asset table.
    #[error("coin {0:?} is not in the asset table")]
    UnknownFilterCoin(String),
    /// A height neither stream had, with gaps disallowed.
    #[error("block {expected} missing (next block in the data is {found})")]
    Gap {
        /// The missing height.
        expected: u64,
        /// The next height present.
        found: u64,
    },
    /// A `new` diff with no side and no inserting status in its block.
    #[error(
        "block {height}: new order {oid} has no side (no side field, no open status in the block)"
    )]
    NoSide {
        /// Block.
        height: u64,
        /// Order id.
        oid: u64,
    },
    /// The diff's side and its status's side disagree.
    #[error("block {height}: order {oid} diff side and status side disagree")]
    SideMismatch {
        /// Block.
        height: u64,
        /// Order id.
        oid: u64,
    },
    /// A price or size that does not convert exactly.
    #[error("block {height}: order {oid} {what} {value:?}: {reason}")]
    Decimal {
        /// Block.
        height: u64,
        /// Order id.
        oid: u64,
        /// `price`, `size`, `newSz`.
        what: &'static str,
        /// The string.
        value: String,
        /// Why.
        reason: String,
    },
    /// A soft finding with `strict` set.
    #[error("strict: {0}")]
    Strict(String),
}

/// Converts node output to a dataset written to `out`.
///
/// - `assets`: the asset table (`hl_dataset::convert::assets_from_meta`).
/// - `diffs`: `node_raw_book_diffs` lines, files concatenated in order.
/// - `statuses`: `node_order_statuses` lines for the same blocks, if any.
/// - `seed`: an `l4Snapshots` at height `H`. Block `H` is then written with
///   one `insert` per resting order (the seed block) and diffs at or below
///   `H` are skipped. Without a seed the first diff block starts an empty
///   book, and diffs on orders the converter never saw inserted are skipped.
/// - `checks`: `l4Snapshots` whose books are written as `l4_book` records
///   (every header asset, empty when absent) after the block at their height.
pub fn convert<D: BufRead, S: BufRead, W: Write>(
    assets: &[AssetMeta],
    diffs: D,
    statuses: Option<S>,
    seed: Option<&L4Snapshot>,
    checks: &[L4Snapshot],
    opts: &Options,
    out: W,
) -> Result<Stats, ConvertError> {
    let selected = select_assets(assets, opts.coins.as_ref())?;
    let mut diffs = BlockBatches::<D, BookDiff>::new(diffs);
    let first_diff = next_diff(&mut diffs)?;
    let first_height = match (seed, &first_diff) {
        (Some(s), _) => s.height,
        (None, Some(b)) => b.block_number,
        (None, None) => return Err(ConvertError::Empty),
    };
    let mut extra = BTreeMap::new();
    if let Some(s) = seed {
        extra.insert("seed_height".to_owned(), serde_json::Value::from(s.height));
    }
    let header = Header {
        format_version: FORMAT_VERSION,
        chain: opts.chain.clone(),
        first_height,
        source: opts.source.clone(),
        assets: selected.clone(),
        extra,
    };
    let mut conv = Converter {
        writer: Writer::new(out, &header)?,
        by_name: selected
            .iter()
            .map(|a| (a.name.clone(), a.clone()))
            .collect(),
        selected,
        known: assets.iter().map(|a| a.name.clone()).collect(),
        checks: checks.iter().map(|c| (c.height, c)).collect(),
        statuses: statuses.map(BlockBatches::new),
        status_next: None,
        tracked: HashMap::new(),
        seeded: seed.is_some(),
        opts,
        stats: Stats {
            first_height,
            ..Stats::default()
        },
        next_height: first_height,
    };
    if let Some(s) = seed {
        conv.seed_block(s)?;
    }
    let mut batch = first_diff;
    while let Some(b) = batch {
        conv.diff_block(&b)?;
        batch = next_diff(&mut diffs)?;
    }
    conv.finish()
}

fn next_diff<D: BufRead>(
    diffs: &mut BlockBatches<D, BookDiff>,
) -> Result<Option<Batch<BookDiff>>, ConvertError> {
    diffs
        .next()
        .transpose()
        .map_err(|error| ConvertError::Node {
            stream: "diffs",
            error,
        })
}

fn select_assets(
    assets: &[AssetMeta],
    coins: Option<&BTreeSet<String>>,
) -> Result<Vec<AssetMeta>, ConvertError> {
    let Some(coins) = coins else {
        return Ok(assets.to_vec());
    };
    let mut out = coins
        .iter()
        .map(|c| {
            assets
                .iter()
                .find(|a| &a.name == c)
                .cloned()
                .ok_or_else(|| ConvertError::UnknownFilterCoin(c.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    out.sort_by_key(|a| a.asset_index);
    Ok(out)
}

struct Converter<'a, S, W: Write> {
    writer: Writer<W>,
    selected: Vec<AssetMeta>,
    by_name: HashMap<String, AssetMeta>,
    known: HashSet<String>,
    checks: BTreeMap<u64, &'a L4Snapshot>,
    statuses: Option<BlockBatches<S, OrderStatus>>,
    status_next: Option<Batch<OrderStatus>>,
    /// Resting orders the converter knows: oid to remaining lots.
    tracked: HashMap<u64, u64>,
    seeded: bool,
    opts: &'a Options,
    stats: Stats,
    /// Height of the next block to write.
    next_height: u64,
}

impl<S: BufRead, W: Write> Converter<'_, S, W> {
    /// A soft finding: an example line, or an error under `strict`.
    fn soft(&mut self, msg: String) -> Result<(), ConvertError> {
        if self.opts.strict {
            return Err(ConvertError::Strict(msg));
        }
        if self.stats.examples.len() < MAX_EXAMPLES {
            self.stats.examples.push(msg);
        }
        Ok(())
    }

    /// The status batch of block `height`, dropping earlier ones.
    fn statuses_for(&mut self, height: u64) -> Result<Option<Batch<OrderStatus>>, ConvertError> {
        let Some(stream) = self.statuses.as_mut() else {
            return Ok(None);
        };
        loop {
            if self.status_next.is_none() {
                self.status_next =
                    stream
                        .next()
                        .transpose()
                        .map_err(|error| ConvertError::Node {
                            stream: "statuses",
                            error,
                        })?;
            }
            match &self.status_next {
                Some(b) if b.block_number < height => self.status_next = None,
                Some(b) if b.block_number == height => return Ok(self.status_next.take()),
                _ => return Ok(None),
            }
        }
    }

    fn write_block(&mut self, time_ms: Option<u64>, ops: Vec<AssetOp>) -> Result<(), ConvertError> {
        let height = self.next_height;
        self.writer.write_block(&Block {
            height,
            time_ms,
            hash: None,
            proposer: None,
            txs: Vec::new(),
            ops,
        })?;
        self.stats.blocks += 1;
        self.stats.last_height = height;
        if let Some(snap) = self.checks.remove(&height) {
            for asset in &self.selected {
                let book = l4_book(snap.book(&asset.name), asset, height)?;
                self.writer.write_book(&BookState::L4(book))?;
                self.stats.check_books += 1;
            }
        }
        self.next_height = height.saturating_add(1);
        Ok(())
    }

    fn seed_block(&mut self, seed: &L4Snapshot) -> Result<(), ConvertError> {
        let mut ops = Vec::new();
        for book in &seed.books {
            let Some(asset) = self.by_name.get(&book.coin) else {
                continue;
            };
            for (list, listed) in [(&book.bids, NodeSide::Bid), (&book.asks, NodeSide::Ask)] {
                for o in list {
                    if o.is_trigger {
                        self.stats.seed_triggers_skipped += 1;
                        continue;
                    }
                    let (px, sz) = order_units(&book.coin, o, listed, asset)?;
                    ops.push(AssetOp {
                        asset: asset.asset_index,
                        op: BookOp::Insert(Order {
                            oid: Oid(o.oid),
                            side: listed.into(),
                            px: Px(px),
                            sz: Lots(sz),
                        }),
                    });
                    self.tracked.insert(o.oid, sz);
                    self.stats.seed_orders += 1;
                }
            }
        }
        self.write_block(None, ops)
    }

    fn diff_block(&mut self, batch: &Batch<BookDiff>) -> Result<(), ConvertError> {
        let height = batch.block_number;
        if height < self.next_height {
            // Only possible with a seed: the batch is at or below its height.
            self.stats.diffs_before_seed += u64::try_from(batch.events.len()).unwrap_or(u64::MAX);
            return Ok(());
        }
        while self.next_height < height {
            let missing = self.next_height;
            if let Some(st) = self.statuses_for(missing)? {
                self.stats.status_only_blocks += 1;
                self.write_block(Some(st.block_time_ms), Vec::new())?;
            } else if self.opts.allow_gaps {
                self.stats.gap_blocks += 1;
                self.write_block(None, Vec::new())?;
            } else {
                return Err(ConvertError::Gap {
                    expected: missing,
                    found: height,
                });
            }
        }
        let statuses = self.statuses_for(height)?;
        let inserted: HashMap<u64, NodeSide> = statuses
            .iter()
            .flat_map(|b| &b.events)
            .filter(|s| s.is_inserted_into_book())
            .map(|s| (s.order.oid, s.order.side))
            .collect();
        let mut ops = Vec::with_capacity(batch.events.len());
        for ev in &batch.events {
            if let Some(op) = self.diff_op(height, ev, &inserted)? {
                ops.push(op);
            }
        }
        self.write_block(Some(batch.block_time_ms), ops)
    }

    fn untracked(&mut self, oid: u64) -> bool {
        if self.seeded || self.tracked.contains_key(&oid) {
            return false;
        }
        self.stats.untracked_diffs += 1;
        true
    }

    fn diff_op(
        &mut self,
        height: u64,
        ev: &BookDiff,
        inserted: &HashMap<u64, NodeSide>,
    ) -> Result<Option<AssetOp>, ConvertError> {
        let Some(asset) = self.by_name.get(&ev.coin).cloned() else {
            if self.known.contains(&ev.coin) {
                self.stats.filtered_diffs += 1;
            } else {
                self.stats.unknown_coin_diffs += 1;
                self.soft(format!(
                    "block {height}: diff for coin {} (oid {}) not in the asset table",
                    ev.coin, ev.oid
                ))?;
            }
            return Ok(None);
        };
        let oid = ev.oid;
        let lots = |what: &'static str, value: &str| {
            asset.sz_lots(value).map_err(|e| ConvertError::Decimal {
                height,
                oid,
                what,
                value: value.to_owned(),
                reason: e.to_string(),
            })
        };
        let op = match &ev.raw_book_diff {
            DiffKind::New { sz, insert_before } => {
                match self.new_op(height, ev, &asset, sz, *insert_before, inserted)? {
                    Some(op) => op,
                    None => return Ok(None),
                }
            }
            DiffKind::Update { orig_sz, new_sz } => {
                if self.untracked(oid) {
                    return Ok(None);
                }
                let orig = lots("origSz", orig_sz)?;
                let new = lots("newSz", new_sz)?;
                if let Some(&had) = self.tracked.get(&oid) {
                    if had != orig {
                        self.stats.orig_sz_mismatches += 1;
                        self.soft(format!(
                            "block {height}: order {oid} update origSz {orig_sz} but tracked size is {}",
                            format_units(had, asset.sz_decimals)
                        ))?;
                    }
                }
                if new == 0 {
                    self.stats.zero_size_updates += 1;
                    self.stats.cancels += 1;
                    self.tracked.remove(&oid);
                    BookOp::Cancel { oid: Oid(oid) }
                } else {
                    self.stats.resizes += 1;
                    self.tracked.insert(oid, new);
                    BookOp::Resize {
                        oid: Oid(oid),
                        sz: Lots(new),
                    }
                }
            }
            DiffKind::Remove => {
                if self.untracked(oid) {
                    return Ok(None);
                }
                self.stats.cancels += 1;
                self.tracked.remove(&oid);
                BookOp::Cancel { oid: Oid(oid) }
            }
        };
        self.stats.ops += 1;
        Ok(Some(AssetOp {
            asset: asset.asset_index,
            op,
        }))
    }

    /// The op of a `new` diff; `None` when its anchor is untracked (no seed).
    fn new_op(
        &mut self,
        height: u64,
        ev: &BookDiff,
        asset: &AssetMeta,
        sz: &str,
        insert_before: Option<u64>,
        inserted: &HashMap<u64, NodeSide>,
    ) -> Result<Option<BookOp>, ConvertError> {
        let oid = ev.oid;
        let side = match (ev.side, inserted.get(&oid).copied()) {
            (Some(a), Some(b)) if a != b => return Err(ConvertError::SideMismatch { height, oid }),
            (Some(a), _) | (None, Some(a)) => a,
            (None, None) => return Err(ConvertError::NoSide { height, oid }),
        };
        let px = asset.px_ticks(&ev.px).map_err(|e| ConvertError::Decimal {
            height,
            oid,
            what: "price",
            value: ev.px.clone(),
            reason: e.to_string(),
        })?;
        let sz = asset.sz_lots(sz).map_err(|e| ConvertError::Decimal {
            height,
            oid,
            what: "size",
            value: sz.to_owned(),
            reason: e.to_string(),
        })?;
        if let Some(before) = insert_before {
            if self.untracked(before) {
                return Ok(None);
            }
        }
        let order = Order {
            oid: Oid(oid),
            side: side.into(),
            px: Px(px),
            sz: Lots(sz),
        };
        self.tracked.insert(oid, sz);
        Ok(Some(if let Some(before) = insert_before {
            self.stats.inserts_before += 1;
            BookOp::InsertBefore {
                before: Oid(before),
                order,
            }
        } else {
            self.stats.inserts += 1;
            BookOp::Insert(order)
        }))
    }

    fn finish(mut self) -> Result<Stats, ConvertError> {
        let left: Vec<u64> = self.checks.keys().copied().collect();
        for h in left {
            self.stats.checks_out_of_range += 1;
            self.soft(format!(
                "check snapshot at height {h} is outside the written blocks {}..={}",
                self.stats.first_height, self.stats.last_height
            ))?;
        }
        self.writer.finish()?;
        Ok(self.stats)
    }
}
