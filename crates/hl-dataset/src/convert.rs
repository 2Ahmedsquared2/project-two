//! Converter from the Hyperliquid Reason's public-data capture
//! (`blocks_core.ndjson`, `l2_anchored.ndjson`, `meta.json`; see its
//! `README.md`) to dataset format v2.
//!
//! - Every explorer `blockDetails` line becomes a `block` record. Each tx's
//!   action is copied byte for byte; `ops` stay empty, because public data has
//!   no oids for new orders, so per-order book effects cannot be derived.
//! - Every anchored `l2Book` snapshot becomes an `l2_book` record (depth 20)
//!   after the block with its height.
//! - `meta.json` becomes the header's asset table: all perps (`meta.universe`,
//!   asset = position) and all spot pairs (asset = `10000 + index`,
//!   `szDecimals` of the base token).
//!
//! While converting, every order and modify leg on a known asset is checked
//! against the tick/lot rule (`ACTION_ORDERING.md` §4.1) and the counts the
//! Phase 1 report needs are collected in [`ConvertStats`].

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::action::{self, Action, OrderRef, OrderWire};
use crate::{AssetMeta, Block, BookState, Header, L2Book, L2Level, Tx, Writer, FORMAT_VERSION};

/// Most violation examples kept in [`ConvertStats::violation_examples`].
pub const MAX_EXAMPLES: usize = 20;

/// Levels per side the public `l2Book` returns at most.
pub const PUBLIC_L2_DEPTH: u32 = 20;

/// Why a conversion failed.
#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    /// `meta.json` is not in the expected shape.
    #[error("meta.json: {0}")]
    Meta(String),
    /// I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// A blocks line is not a `blockDetails` object.
    #[error("blocks line {line}: {source}")]
    Block {
        /// 1-based line.
        line: usize,
        /// The JSON error.
        source: serde_json::Error,
    },
    /// An `l2_anchored` line is not a snapshot object.
    #[error("l2 line {line}: {source}")]
    L2 {
        /// 1-based line.
        line: usize,
        /// The JSON error.
        source: serde_json::Error,
    },
    /// The blocks input is empty.
    #[error("no blocks in the input")]
    NoBlocks,
    /// Writing the dataset failed (e.g. a height gap in the blocks input).
    #[error("writing the dataset: {0}")]
    Write(#[from] crate::WriteError),
}

/// What a conversion saw. Counts are per leg where the name says so.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ConvertStats {
    /// Height of the first block.
    pub first_height: u64,
    /// Height of the last block.
    pub last_height: u64,
    /// Blocks written.
    pub blocks: u64,
    /// Transactions written.
    pub txs: u64,
    /// Transactions with a non-null `error`.
    pub errored_txs: u64,
    /// Actions by `type` (every tx, errored or not).
    pub actions: BTreeMap<String, u64>,
    /// `order` legs by tif (`Alo`, `Gtc`, `Ioc`, `FrontendMarket`, `trigger`, ...).
    pub order_legs: BTreeMap<String, u64>,
    /// New-order legs of `modify`/`batchModify` by tif.
    pub modify_legs: BTreeMap<String, u64>,
    /// `cancel` legs (by oid: not resolvable from public data).
    pub cancel_by_oid_legs: u64,
    /// `cancelByCloid` legs (resolvable by user + cloid).
    pub cancel_by_cloid_legs: u64,
    /// Modify legs naming the old order by oid.
    pub modify_by_oid_legs: u64,
    /// Modify legs naming the old order by cloid.
    pub modify_by_cloid_legs: u64,
    /// Order/modify legs whose asset is not in the header, by asset class.
    pub legs_unknown_asset: BTreeMap<String, u64>,
    /// Order/modify legs on known assets checked against the tick/lot rule.
    pub legs_checked: u64,
    /// Legs whose price breaks the tick rule.
    pub px_violations: u64,
    /// Legs whose size breaks the lot rule.
    pub sz_violations: u64,
    /// Violating legs by `asset class: reason` (a leg breaking both rules counts twice).
    pub violations_by_reason: BTreeMap<String, u64>,
    /// Of the violations, how many are in txs that carry an `error`.
    pub violations_in_errored_txs: u64,
    /// The first few violations: `height tx_hash asset px sz reason`.
    pub violation_examples: Vec<String>,
    /// Actions whose book-relevant shape failed to parse.
    pub parse_failures: u64,
    /// The first few parse failures: `height tx_hash error`.
    pub parse_failure_examples: Vec<String>,
    /// `l2_book` records written.
    pub l2_records: u64,
    /// Snapshots dropped because a level breaks the tick/lot rule.
    pub l2_level_violations: u64,
    /// Snapshots whose height is not in the block range.
    pub l2_out_of_range: u64,
    /// Snapshots whose coin is not in the asset table.
    pub l2_unknown_coin: u64,
    /// Second snapshots for the same (height, coin), dropped.
    pub l2_duplicates: u64,
    /// Snapshots whose `block_time_ms` differs from the block's `blockTime`.
    pub l2_time_mismatches: u64,
}

#[derive(Deserialize)]
struct MetaFile {
    meta: PerpMeta,
    #[serde(rename = "spotMeta")]
    spot_meta: SpotMeta,
}

#[derive(Deserialize)]
struct PerpMeta {
    universe: Vec<PerpAsset>,
}

#[derive(Deserialize)]
struct PerpAsset {
    name: String,
    #[serde(rename = "szDecimals")]
    sz_decimals: u32,
}

#[derive(Deserialize)]
struct SpotMeta {
    universe: Vec<SpotPair>,
    tokens: Vec<SpotToken>,
}

#[derive(Deserialize)]
struct SpotPair {
    name: String,
    index: u32,
    tokens: Vec<u32>,
}

#[derive(Deserialize)]
struct SpotToken {
    index: u32,
    #[serde(rename = "szDecimals")]
    sz_decimals: u32,
}

/// The asset table from a `meta.json` holding `{"meta": ..., "spotMeta": ...}`:
/// perps first (asset = position in `meta.universe`), then spot pairs (asset =
/// `10000 + index`, `szDecimals` of the base token found by its `index` field).
pub fn assets_from_meta(json: &str) -> Result<Vec<AssetMeta>, ConvertError> {
    let m: MetaFile = serde_json::from_str(json).map_err(|e| ConvertError::Meta(e.to_string()))?;
    let mut out = Vec::with_capacity(m.meta.universe.len() + m.spot_meta.universe.len());
    for (i, p) in m.meta.universe.iter().enumerate() {
        let idx = u32::try_from(i).map_err(|_| ConvertError::Meta("too many perps".into()))?;
        out.push(
            AssetMeta::new(idx, &p.name, p.sz_decimals, false)
                .map_err(|e| ConvertError::Meta(format!("perp {}: {e}", p.name)))?,
        );
    }
    let token_sz: HashMap<u32, u32> = m
        .spot_meta
        .tokens
        .iter()
        .map(|t| (t.index, t.sz_decimals))
        .collect();
    for pair in &m.spot_meta.universe {
        let base = *pair
            .tokens
            .first()
            .ok_or_else(|| ConvertError::Meta(format!("spot {} has no tokens", pair.name)))?;
        let sz = *token_sz.get(&base).ok_or_else(|| {
            ConvertError::Meta(format!("spot {}: unknown token {base}", pair.name))
        })?;
        let asset = pair
            .index
            .checked_add(10_000)
            .ok_or_else(|| ConvertError::Meta("spot index overflow".into()))?;
        out.push(
            AssetMeta::new(asset, &pair.name, sz, true)
                .map_err(|e| ConvertError::Meta(format!("spot {}: {e}", pair.name)))?,
        );
    }
    Ok(out)
}

#[derive(Deserialize)]
struct ExplorerBlock {
    height: u64,
    #[serde(rename = "blockTime")]
    block_time: u64,
    hash: String,
    proposer: String,
    txs: Vec<ExplorerTx>,
}

#[derive(Deserialize)]
struct ExplorerTx {
    time: u64,
    user: String,
    action: Box<RawValue>,
    hash: String,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ExplorerL2 {
    height: u64,
    coin: String,
    block_time_ms: u64,
    levels: (Vec<ExplorerLevel>, Vec<ExplorerLevel>),
}

#[derive(Deserialize)]
struct ExplorerLevel {
    px: String,
    sz: String,
    n: u32,
}

fn bump(map: &mut BTreeMap<String, u64>, key: &str) {
    *map.entry(key.to_owned()).or_default() += 1;
}

fn push_example(v: &mut Vec<String>, s: String) {
    if v.len() < MAX_EXAMPLES {
        v.push(s);
    }
}

struct Ctx<'a> {
    assets: &'a HashMap<u32, AssetMeta>,
    stats: ConvertStats,
}

impl Ctx<'_> {
    fn check_leg(&mut self, height: u64, tx: &ExplorerTx, leg: &OrderWire) {
        let Some(meta) = self.assets.get(&leg.a) else {
            bump(
                &mut self.stats.legs_unknown_asset,
                action::asset_class(leg.a).name(),
            );
            return;
        };
        self.stats.legs_checked += 1;
        let px = meta.order_px_ticks(&leg.p).err().map(|e| e.to_string());
        let sz = meta.sz_lots(&leg.s).err().map(|e| e.to_string());
        let class = action::asset_class(leg.a).name();
        if let Some(e) = &px {
            self.stats.px_violations += 1;
            bump(
                &mut self.stats.violations_by_reason,
                &format!("{class} px: {e}"),
            );
        }
        if let Some(e) = &sz {
            self.stats.sz_violations += 1;
            bump(
                &mut self.stats.violations_by_reason,
                &format!("{class} sz: {e}"),
            );
        }
        if px.is_some() || sz.is_some() {
            if tx.error.is_some() {
                self.stats.violations_in_errored_txs += 1;
            }
            let reason = [
                px.map(|e| format!("px: {e}")),
                sz.map(|e| format!("sz: {e}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
            push_example(
                &mut self.stats.violation_examples,
                format!(
                    "height {height} tx {} asset {} ({}) px {:?} sz {:?}: {reason}",
                    tx.hash, leg.a, meta.name, leg.p, leg.s
                ),
            );
        }
    }

    fn modify_leg(&mut self, height: u64, tx: &ExplorerTx, m: &action::ModifyWire) {
        bump(&mut self.stats.modify_legs, m.order.t.name());
        match m.oid {
            OrderRef::Oid(_) => self.stats.modify_by_oid_legs += 1,
            OrderRef::Cloid(_) => self.stats.modify_by_cloid_legs += 1,
        }
        self.check_leg(height, tx, &m.order);
    }

    fn tx(&mut self, height: u64, tx: &ExplorerTx) {
        self.stats.txs += 1;
        if tx.error.is_some() {
            self.stats.errored_txs += 1;
        }
        let parsed = match action::parse(tx.action.get()) {
            Ok(a) => a,
            Err(e) => {
                self.stats.parse_failures += 1;
                push_example(
                    &mut self.stats.parse_failure_examples,
                    format!("height {height} tx {}: {e}", tx.hash),
                );
                return;
            }
        };
        bump(&mut self.stats.actions, parsed.type_name());
        match &parsed {
            Action::Order { orders } => {
                for leg in orders {
                    bump(&mut self.stats.order_legs, leg.t.name());
                    self.check_leg(height, tx, leg);
                }
            }
            Action::Cancel { cancels } => {
                self.stats.cancel_by_oid_legs += u64::try_from(cancels.len()).unwrap_or(0);
            }
            Action::CancelByCloid { cancels } => {
                self.stats.cancel_by_cloid_legs += u64::try_from(cancels.len()).unwrap_or(0);
            }
            Action::Modify(m) => self.modify_leg(height, tx, m),
            Action::BatchModify { modifies } => {
                for m in modifies {
                    self.modify_leg(height, tx, m);
                }
            }
            Action::Other(_) => {}
        }
    }

    fn l2_book(&mut self, snap: &ExplorerL2, asset: &AssetMeta) -> Option<L2Book> {
        let conv = |levels: &[ExplorerLevel]| -> Option<Vec<L2Level>> {
            levels
                .iter()
                .map(|l| {
                    Some(L2Level {
                        px_ticks: asset.order_px_ticks(&l.px).ok()?,
                        sz_lots: asset.sz_lots(&l.sz).ok()?,
                        n: l.n,
                        px: Some(l.px.clone()),
                        sz: Some(l.sz.clone()),
                    })
                })
                .collect()
        };
        let (Some(bids), Some(asks)) = (conv(&snap.levels.0), conv(&snap.levels.1)) else {
            self.stats.l2_level_violations += 1;
            return None;
        };
        Some(L2Book {
            height: snap.height,
            asset: asset.asset_index,
            time_ms: Some(snap.block_time_ms),
            depth: Some(PUBLIC_L2_DEPTH),
            bids,
            asks,
        })
    }
}

/// Snapshots grouped by height, each group ordered by asset; unknown coins
/// and duplicates are counted in `stats` and dropped.
fn group_snapshots(
    l2: &str,
    by_name: &HashMap<&str, &AssetMeta>,
    stats: &mut ConvertStats,
) -> Result<BTreeMap<u64, BTreeMap<u32, ExplorerL2>>, ConvertError> {
    let mut snaps: BTreeMap<u64, BTreeMap<u32, ExplorerL2>> = BTreeMap::new();
    for (i, line) in l2.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let s: ExplorerL2 = serde_json::from_str(line).map_err(|source| ConvertError::L2 {
            line: i + 1,
            source,
        })?;
        let Some(meta) = by_name.get(s.coin.as_str()) else {
            stats.l2_unknown_coin += 1;
            continue;
        };
        match snaps.entry(s.height).or_default().entry(meta.asset_index) {
            std::collections::btree_map::Entry::Occupied(_) => stats.l2_duplicates += 1,
            std::collections::btree_map::Entry::Vacant(v) => {
                v.insert(s);
            }
        }
    }
    Ok(snaps)
}

/// Convert a capture to a v2 dataset written to `out`.
///
/// `blocks` holds one explorer `blockDetails` object per line, heights
/// contiguous; `l2` is the whole `l2_anchored.ndjson` text. `source` goes in
/// the header verbatim.
pub fn convert<R: BufRead, W: Write>(
    assets: &[AssetMeta],
    source: &str,
    blocks: R,
    l2: &str,
    out: W,
) -> Result<ConvertStats, ConvertError> {
    let by_index: HashMap<u32, AssetMeta> =
        assets.iter().map(|a| (a.asset_index, a.clone())).collect();
    let by_name: HashMap<&str, &AssetMeta> = assets.iter().map(|a| (a.name.as_str(), a)).collect();
    let mut ctx = Ctx {
        assets: &by_index,
        stats: ConvertStats::default(),
    };

    let mut snaps = group_snapshots(l2, &by_name, &mut ctx.stats)?;

    let mut lines = blocks.lines().enumerate();
    let mut writer: Option<Writer<W>> = None;
    let mut out = Some(out);
    for (i, line) in lines.by_ref() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let b: ExplorerBlock =
            serde_json::from_str(&line).map_err(|source| ConvertError::Block {
                line: i + 1,
                source,
            })?;
        let w = if let Some(w) = writer.as_mut() {
            w
        } else {
            ctx.stats.first_height = b.height;
            let header = Header {
                format_version: FORMAT_VERSION,
                chain: "mainnet".to_owned(),
                first_height: b.height,
                source: source.to_owned(),
                assets: assets.to_vec(),
                extra: BTreeMap::new(),
            };
            let Some(o) = out.take() else {
                unreachable!("writer is created once");
            };
            writer.insert(Writer::new(o, &header)?)
        };
        for tx in &b.txs {
            ctx.tx(b.height, tx);
        }
        let block_time = b.block_time;
        let height = b.height;
        let block = Block {
            height,
            time_ms: Some(block_time),
            hash: Some(b.hash),
            proposer: Some(b.proposer),
            txs: b
                .txs
                .into_iter()
                .map(|t| Tx {
                    time_ms: Some(t.time),
                    user: t.user,
                    hash: t.hash,
                    error: t.error,
                    action: t.action.into(),
                })
                .collect(),
            ops: Vec::new(),
        };
        w.write_block(&block)?;
        ctx.stats.blocks += 1;
        ctx.stats.last_height = height;
        if let Some(group) = snaps.remove(&height) {
            for (asset, snap) in group {
                if snap.block_time_ms != block_time {
                    ctx.stats.l2_time_mismatches += 1;
                }
                let Some(meta) = by_index.get(&asset) else {
                    unreachable!("snapshot asset {asset} came from the table");
                };
                if let Some(book) = ctx.l2_book(&snap, meta) {
                    w.write_book(&BookState::L2(book))?;
                    ctx.stats.l2_records += 1;
                }
            }
        }
    }
    let Some(w) = writer else {
        return Err(ConvertError::NoBlocks);
    };
    w.finish()?;
    ctx.stats.l2_out_of_range = snaps
        .values()
        .map(|g| u64::try_from(g.len()).unwrap_or(0))
        .sum();
    Ok(ctx.stats)
}
