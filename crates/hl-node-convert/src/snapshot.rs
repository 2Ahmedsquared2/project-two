//! The node's `l4Snapshots` file: every resting order of every book at one height.
//!
//! Produced by the node's local info server, `POST /info`
//! `{"type":"fileSnapshot","request":{"type":"l4Snapshots","includeUsers":true,"includeTriggerOrders":false},"outPath":...,"includeHeightInOutput":true}`
//! (`hyperliquid-dex/node` README, "EVM and Info servers"). With users and
//! height the file is `[height, [[coin, [bids, asks]], ...]]` where each order
//! is `[user, order]` (`load_snapshots_from_str` in `order_book_server`
//! `server/src/order_book/multi_book.rs`; order fields as `L4Order` in
//! `server/src/types/mod.rs` and the docs page "L1 data schemas", section
//! "L4 snapshots"). The book is the state after block `height` (INFERENCE from
//! `order_book_server`, which applies only batches with a higher block number).

use std::collections::{BTreeMap, BTreeSet};

use hl_dataset::{AssetMeta, L4Book, L4Level, L4Order};
use serde::Deserialize;

use crate::node::NodeSide;

/// One resting order of a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotOrder {
    /// Supplied market name, if present. Omission is accepted for compatibility;
    /// an explicit name must exactly match the containing book's coin.
    pub coin: Option<String>,
    /// Owner address.
    pub user: String,
    /// Side the order claims.
    pub side: NodeSide,
    /// Limit price, decimal string.
    pub limit_px: String,
    /// Remaining size, decimal string.
    pub sz: String,
    /// Order id.
    pub oid: u64,
    /// Trigger orders are listed after the resting orders and are not on the book.
    pub is_trigger: bool,
}

/// One coin's book: bids and asks, each in the node's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinBook {
    /// Coin name as the node writes it.
    pub coin: String,
    /// Bid orders.
    pub bids: Vec<SnapshotOrder>,
    /// Ask orders.
    pub asks: Vec<SnapshotOrder>,
}

/// A parsed `l4Snapshots` file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct L4Snapshot {
    /// Height the books are at (after this block).
    pub height: u64,
    /// One entry per coin, in file order.
    pub books: Vec<CoinBook>,
}

/// Why a snapshot is unusable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    /// Not the `[height, [[coin, [bids, asks]]]]` shape with users.
    #[error("bad l4Snapshots json (need includeUsers and includeHeightInOutput): {0}")]
    Json(String),
    /// More than one outer book record names the same market.
    #[error("duplicate l4Snapshots coin {0:?}")]
    DuplicateCoin(String),
    /// An order explicitly names a market other than its containing book.
    #[error("{coin} oid {oid}: order coin {order_coin:?} differs from containing book")]
    CoinMismatch {
        /// Containing book's coin.
        coin: String,
        /// Order id.
        oid: u64,
        /// The order's supplied coin.
        order_coin: String,
    },
    /// A price or size that does not convert exactly.
    #[error("{coin} oid {oid}: {what} {value:?} does not convert: {reason}")]
    Decimal {
        /// Coin.
        coin: String,
        /// Order id.
        oid: u64,
        /// `price` or `size`.
        what: &'static str,
        /// The string.
        value: String,
        /// Why.
        reason: String,
    },
    /// An order listed on the other side's list.
    #[error("{coin} oid {oid} listed as a {listed:?} but has side {side:?}")]
    SideMismatch {
        /// Coin.
        coin: String,
        /// Order id.
        oid: u64,
        /// The list it is in.
        listed: NodeSide,
        /// Its own side field.
        side: NodeSide,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOrder {
    #[serde(default)]
    coin: Option<String>,
    side: NodeSide,
    limit_px: String,
    sz: String,
    oid: u64,
    #[serde(default)]
    is_trigger: bool,
}

type RawFile = (u64, Vec<(String, [Vec<(String, RawOrder)>; 2])>);

impl L4Snapshot {
    /// Parses the file text.
    pub fn from_json(text: &str) -> Result<Self, SnapshotError> {
        let (height, coins): RawFile =
            serde_json::from_str(text).map_err(|e| SnapshotError::Json(e.to_string()))?;
        let conv = |list: Vec<(String, RawOrder)>| -> Vec<SnapshotOrder> {
            list.into_iter()
                .map(|(user, o)| SnapshotOrder {
                    coin: o.coin,
                    user,
                    side: o.side,
                    limit_px: o.limit_px,
                    sz: o.sz,
                    oid: o.oid,
                    is_trigger: o.is_trigger,
                })
                .collect()
        };
        let books = coins
            .into_iter()
            .map(|(coin, [bids, asks])| CoinBook {
                coin,
                bids: conv(bids),
                asks: conv(asks),
            })
            .collect();
        let snapshot = Self { height, books };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Checks identities, including trigger orders and unselected markets.
    ///
    /// Public fields allow manual construction, so consumers must call this
    /// before selecting books or filtering orders. Market names follow the
    /// existing exact-name lookup; absence of an inner coin is compatible.
    pub fn validate(&self) -> Result<(), SnapshotError> {
        let mut seen = BTreeSet::new();
        for book in &self.books {
            if !seen.insert(&book.coin) {
                return Err(SnapshotError::DuplicateCoin(book.coin.clone()));
            }
            for order in book.bids.iter().chain(&book.asks) {
                validate_order_coin(&book.coin, order)?;
            }
        }
        Ok(())
    }

    /// The book of `coin`, if the snapshot lists it.
    #[must_use]
    pub fn book(&self, coin: &str) -> Option<&CoinBook> {
        self.books.iter().find(|b| b.coin == coin)
    }
}

fn validate_order_coin(coin: &str, order: &SnapshotOrder) -> Result<(), SnapshotError> {
    if let Some(order_coin) = &order.coin {
        if order_coin != coin {
            return Err(SnapshotError::CoinMismatch {
                coin: coin.to_owned(),
                oid: order.oid,
                order_coin: order_coin.clone(),
            });
        }
    }
    Ok(())
}

/// Price ticks and size lots of one snapshot order, checked against its list.
pub(crate) fn order_units(
    coin: &str,
    o: &SnapshotOrder,
    listed: NodeSide,
    asset: &AssetMeta,
) -> Result<(u64, u64), SnapshotError> {
    validate_order_coin(coin, o)?;
    if o.side != listed {
        return Err(SnapshotError::SideMismatch {
            coin: coin.to_owned(),
            oid: o.oid,
            listed,
            side: o.side,
        });
    }
    let bad = |what: &'static str, value: &str, reason: String| SnapshotError::Decimal {
        coin: coin.to_owned(),
        oid: o.oid,
        what,
        value: value.to_owned(),
        reason,
    };
    let px = asset
        .px_ticks(&o.limit_px)
        .map_err(|e| bad("price", &o.limit_px, e.to_string()))?;
    let sz = asset
        .sz_lots(&o.sz)
        .map_err(|e| bad("size", &o.sz, e.to_string()))?;
    Ok((px, sz))
}

/// The recorded `l4_book` of one asset after block `height`: trigger orders
/// dropped, orders grouped into levels by price (queue order kept as listed),
/// levels best first. A coin the snapshot does not list has an empty book.
pub fn l4_book(
    book: Option<&CoinBook>,
    asset: &AssetMeta,
    height: u64,
) -> Result<L4Book, SnapshotError> {
    let mut out = L4Book {
        height,
        asset: asset.asset_index,
        time_ms: None,
        bids: Vec::new(),
        asks: Vec::new(),
    };
    let Some(book) = book else {
        return Ok(out);
    };
    let side = |list: &[SnapshotOrder], listed: NodeSide| -> Result<Vec<L4Level>, SnapshotError> {
        let mut levels: BTreeMap<u64, L4Level> = BTreeMap::new();
        for o in list {
            validate_order_coin(&book.coin, o)?;
            if o.is_trigger {
                continue;
            }
            let (px, sz) = order_units(&book.coin, o, listed, asset)?;
            levels
                .entry(px)
                .or_insert_with(|| L4Level {
                    px_ticks: px,
                    px: Some(o.limit_px.clone()),
                    orders: Vec::new(),
                })
                .orders
                .push(L4Order {
                    oid: o.oid,
                    sz_lots: sz,
                    user: Some(o.user.clone()),
                });
        }
        let levels = levels.into_values();
        Ok(match listed {
            NodeSide::Bid => levels.rev().collect(),
            NodeSide::Ask => levels.collect(),
        })
    };
    out.bids = side(&book.bids, NodeSide::Bid)?;
    out.asks = side(&book.asks, NodeSide::Ask)?;
    Ok(out)
}
