//! A typed view of the exchange actions the reference book cares about.
//!
//! Blocks keep each signed action byte for byte ([`crate::RawJson`]); this
//! module reads the few action types that touch the order book (`order`,
//! `cancel`, `cancelByCloid`, `modify`, `batchModify`) into plain structs and
//! leaves every other type opaque ([`Action::Other`]). Field names follow the
//! wire format in Hyperliquid's exchange-endpoint docs
//! (<https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint>).
//! Prices and sizes stay decimal strings here; [`crate::AssetMeta`] turns them
//! into ticks and lots under the tick/lot rule.

use std::fmt;

use serde::de::{self, Deserializer, IgnoredAny, Visitor};
use serde::Deserialize;

/// Range an asset id falls in (Hyperliquid docs, "Asset IDs").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AssetClass {
    /// `0..10000`: index in `meta.universe`.
    Perp,
    /// `10000..100000`: `10000 + spotMeta.universe[].index`.
    Spot,
    /// `100000..100_000_000`: builder-deployed perp, `100000 + dex * 10000 + index`.
    BuilderPerp,
    /// Anything above (observed: ids around `1e8`, unverified meaning;
    /// possibly outcome markets).
    Other,
}

impl AssetClass {
    /// A short stable name for reports.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            AssetClass::Perp => "perp",
            AssetClass::Spot => "spot",
            AssetClass::BuilderPerp => "builder_perp",
            AssetClass::Other => "other",
        }
    }
}

/// The class of an asset id.
#[must_use]
pub fn asset_class(asset: u32) -> AssetClass {
    match asset {
        0..10_000 => AssetClass::Perp,
        10_000..100_000 => AssetClass::Spot,
        100_000..100_000_000 => AssetClass::BuilderPerp,
        _ => AssetClass::Other,
    }
}

/// Limit order time in force as sent on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LimitTif {
    /// Add liquidity only (post only).
    Alo,
    /// Immediate or cancel.
    Ioc,
    /// Good till cancelled.
    Gtc,
    /// Observed in blocks, not documented as a request tif. INFERENCE: an IOC
    /// market order with the frontend's slippage limit (`ACTION_ORDERING.md` §2.3).
    FrontendMarket,
    /// Any other string, kept verbatim.
    Unknown(String),
}

impl LimitTif {
    /// The wire name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            LimitTif::Alo => "Alo",
            LimitTif::Ioc => "Ioc",
            LimitTif::Gtc => "Gtc",
            LimitTif::FrontendMarket => "FrontendMarket",
            LimitTif::Unknown(s) => s,
        }
    }
}

/// Order type: a limit order with its tif, or a trigger order (TP/SL).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum OrderKind {
    /// `{"limit":{"tif":...}}`.
    Limit(LimitTif),
    /// `{"trigger":{...}}`: not on the book until triggered.
    Trigger,
}

impl OrderKind {
    /// `Alo`, `Ioc`, `Gtc`, `FrontendMarket`, other tif names, or `trigger`.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            OrderKind::Limit(t) => t.name(),
            OrderKind::Trigger => "trigger",
        }
    }
}

/// One order as sent in an `order` action or inside a modify.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
pub struct OrderWire {
    /// Asset id.
    pub a: u32,
    /// Is buy.
    pub b: bool,
    /// Limit price, decimal string.
    pub p: String,
    /// Size, decimal string.
    pub s: String,
    /// Reduce only.
    #[serde(default)]
    pub r: bool,
    /// Order type.
    #[serde(deserialize_with = "de_kind")]
    pub t: OrderKind,
    /// Client order id, if any.
    #[serde(default)]
    pub c: Option<String>,
}

/// A plain cancel leg: asset and oid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub struct CancelWire {
    /// Asset id.
    pub a: u32,
    /// Order id.
    pub o: u64,
}

/// A cancel-by-cloid leg.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
pub struct CancelByCloidWire {
    /// Asset id.
    pub asset: u32,
    /// Client order id.
    pub cloid: String,
}

/// How a modify names the order it replaces: by oid or by cloid.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum OrderRef {
    /// Exchange order id.
    Oid(u64),
    /// Client order id string.
    Cloid(String),
}

/// One modify leg: the order it replaces and the new order.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
pub struct ModifyWire {
    /// The order replaced.
    #[serde(deserialize_with = "de_order_ref")]
    pub oid: OrderRef,
    /// The new order.
    pub order: OrderWire,
}

/// An action, as far as the order book is concerned.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// `order`: legs applied in array order.
    Order {
        /// The legs.
        orders: Vec<OrderWire>,
    },
    /// `cancel` by oid.
    Cancel {
        /// The legs.
        cancels: Vec<CancelWire>,
    },
    /// `cancelByCloid`.
    CancelByCloid {
        /// The legs.
        cancels: Vec<CancelByCloidWire>,
    },
    /// `modify`.
    Modify(ModifyWire),
    /// `batchModify`.
    BatchModify {
        /// The legs.
        modifies: Vec<ModifyWire>,
    },
    /// Any other action type, by name. Does not touch the book in Phase 1.
    Other(String),
}

impl Action {
    /// The wire `type`.
    #[must_use]
    pub fn type_name(&self) -> &str {
        match self {
            Action::Order { .. } => "order",
            Action::Cancel { .. } => "cancel",
            Action::CancelByCloid { .. } => "cancelByCloid",
            Action::Modify(_) => "modify",
            Action::BatchModify { .. } => "batchModify",
            Action::Other(t) => t,
        }
    }
}

#[derive(Deserialize)]
struct TypeProbe {
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Deserialize)]
struct OrdersBody {
    orders: Vec<OrderWire>,
}

#[derive(Deserialize)]
struct CancelsBody {
    cancels: Vec<CancelWire>,
}

#[derive(Deserialize)]
struct CloidCancelsBody {
    cancels: Vec<CancelByCloidWire>,
}

#[derive(Deserialize)]
struct BatchModifyBody {
    modifies: Vec<ModifyWire>,
}

/// Parse one raw action JSON text.
pub fn parse(raw: &str) -> Result<Action, serde_json::Error> {
    let TypeProbe { ty } = serde_json::from_str(raw)?;
    Ok(match ty.as_str() {
        "order" => Action::Order {
            orders: serde_json::from_str::<OrdersBody>(raw)?.orders,
        },
        "cancel" => Action::Cancel {
            cancels: serde_json::from_str::<CancelsBody>(raw)?.cancels,
        },
        "cancelByCloid" => Action::CancelByCloid {
            cancels: serde_json::from_str::<CloidCancelsBody>(raw)?.cancels,
        },
        "modify" => Action::Modify(serde_json::from_str(raw)?),
        "batchModify" => Action::BatchModify {
            modifies: serde_json::from_str::<BatchModifyBody>(raw)?.modifies,
        },
        _ => Action::Other(ty),
    })
}

#[derive(Deserialize)]
struct LimitBody {
    tif: String,
}

#[derive(Deserialize)]
struct KindBody {
    limit: Option<LimitBody>,
    trigger: Option<IgnoredAny>,
}

fn de_kind<'de, D: Deserializer<'de>>(d: D) -> Result<OrderKind, D::Error> {
    let k = KindBody::deserialize(d)?;
    match (k.limit, k.trigger) {
        (Some(l), None) => Ok(OrderKind::Limit(match l.tif.as_str() {
            "Alo" => LimitTif::Alo,
            "Ioc" => LimitTif::Ioc,
            "Gtc" => LimitTif::Gtc,
            "FrontendMarket" => LimitTif::FrontendMarket,
            _ => LimitTif::Unknown(l.tif),
        })),
        (None, Some(_)) => Ok(OrderKind::Trigger),
        _ => Err(de::Error::custom(
            "order type must have exactly one of `limit` or `trigger`",
        )),
    }
}

struct OrderRefVisitor;

impl Visitor<'_> for OrderRefVisitor {
    type Value = OrderRef;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an integer oid or a cloid string")
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<OrderRef, E> {
        Ok(OrderRef::Oid(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<OrderRef, E> {
        u64::try_from(v)
            .map(OrderRef::Oid)
            .map_err(|_| E::custom("negative oid"))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<OrderRef, E> {
        Ok(OrderRef::Cloid(v.to_owned()))
    }
}

fn de_order_ref<'de, D: Deserializer<'de>>(d: D) -> Result<OrderRef, D::Error> {
    d.deserialize_any(OrderRefVisitor)
}
