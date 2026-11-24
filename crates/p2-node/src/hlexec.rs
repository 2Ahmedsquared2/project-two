//! Adapter: lane 1's `hl_exec::Engine` behind the replay driver's [`Engine`]
//! trait.
//!
//! What it does, in the order a block goes through it:
//!
//! 1. **Convert** (outside the `exec` timer): each block action's JSON
//!    becomes an `hl_exec::UserAction` using only the action itself and
//!    static asset metadata ([`AssetTable`]: decimals, coin names). Prices
//!    and sizes become integer ticks and lots; a leg whose strings break a
//!    static rule (price off the tick grid or over 5 significant figures,
//!    size off the lot grid, zero) gets a `pre_reject`. **Never** filled from
//!    native's response: no `oid_hint`, no reject reason (gate F3 / the
//!    counted-path rule).
//! 2. **Execute** (the only span [`Engine::core_exec_ns`] reports): one
//!    `hl_exec::Engine::execute_block_bundled` call (native's per-bundle order).
//! 3. **Render**: our statuses as native response JSON, our diffs as native
//!    raw book diffs with canonical decimal strings.
//!
//! On the resync path only (after a block already counted as NOT
//! reproduced), the book and the oid counter are adopted from native: the
//! block's native diffs replace ours, the counter moves past the largest oid
//! native's responses or diffs show for that block, and cloids of new orders
//! are taken from native's responses so later `cancelByCloid` resolve.
//!
//! The signer is the user native resolved for the action (`user` in the
//! response record; the vault address when trading for a vault): this
//! adapter does not recover signers from signatures.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Instant;

use hl_exec::{
    Asset, BookDiff as XDiff, CancelReq, Cloid, DiffKind, ModifyReq, Oid, OrderRef, OrderReq,
    OrderStatus, Px, RejectReason, RestingOrder, Side, Sz, Tif, User, UserAction as XAction,
};
use hl_node_convert::node::{BookDiff, DiffKind as NDiffKind, NodeSide};
use serde_json::{json, Value};

use crate::engine::{ActionResult, BlockOutcome, Engine, UserAction};

/// Lane 1's resting-order seed (`crates/hl-exec/tools/abci_books_to_l4.py`
/// output, `resting_<H>.jsonl`): a header `{"height", "next_oid", ..}`, then
/// one resting order per line, in queue order, with raw integer `px_ticks`
/// and `sz_lots` (the units of [`AssetMeta`]'s decimals). `reduce_only` is
/// read but not kept: hl-exec's resting order has no such field.
#[derive(Clone, Debug)]
pub struct RestingSeed {
    /// The state height the seed was cut at.
    pub height: u64,
    /// Native's next oid at that height, when the header carries it.
    pub next_oid: Option<Oid>,
    /// Resting orders, in file (queue) order.
    pub orders: Vec<RestingOrder>,
}

#[derive(serde::Deserialize)]
struct SeedHeader {
    height: u64,
    next_oid: Option<u64>,
}

#[derive(serde::Deserialize)]
struct SeedOrder {
    asset: u32,
    user: String,
    oid: u64,
    side: String,
    px_ticks: u64,
    sz_lots: u64,
    cloid: Option<String>,
    t_ms: u64,
    #[allow(dead_code)]
    reduce_only: bool,
}

impl RestingSeed {
    /// Reads the whole seed. Any bad line is an error naming its line.
    pub fn read(reader: impl std::io::BufRead) -> Result<Self, String> {
        let mut lines = reader.lines();
        let head = lines
            .next()
            .ok_or("line 1: empty seed")?
            .map_err(|e| format!("line 1: {e}"))?;
        let header: SeedHeader =
            serde_json::from_str(&head).map_err(|e| format!("line 1: header: {e}"))?;
        let mut orders = Vec::new();
        for (idx, line) in lines.enumerate() {
            let n = idx + 2;
            let line = line.map_err(|e| format!("line {n}: {e}"))?;
            if line.trim().is_empty() {
                continue;
            }
            let ord: SeedOrder =
                serde_json::from_str(&line).map_err(|e| format!("line {n}: {e}"))?;
            let side = match ord.side.as_str() {
                "B" => Side::Bid,
                "A" => Side::Ask,
                other => return Err(format!("line {n}: side {other}")),
            };
            let cloid = match &ord.cloid {
                None => None,
                Some(cl) => Some(parse_cloid(cl).ok_or_else(|| format!("line {n}: cloid {cl}"))?),
            };
            orders.push(RestingOrder {
                asset: ord.asset,
                user: parse_user(&ord.user)
                    .ok_or_else(|| format!("line {n}: user {}", ord.user))?,
                oid: Oid(ord.oid),
                side,
                px: Px(ord.px_ticks),
                sz: Sz(ord.sz_lots),
                cloid,
                t_ms: ord.t_ms,
            });
        }
        Ok(Self {
            height: header.height,
            next_oid: header.next_oid.map(Oid),
            orders,
        })
    }
}

/// Every asset id an action names (order and modify legs `a`, cancels `a`,
/// cancel-by-cloid `asset`), whether or not the adapter supports it.
#[must_use]
pub fn named_assets(a: &UserAction<'_>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut take = |v: Option<&Value>| {
        if let Some(x) = v
            .and_then(Value::as_u64)
            .and_then(|x| u32::try_from(x).ok())
        {
            out.push(x);
        }
    };
    let act = a.action;
    for key in ["orders", "cancels"] {
        for l in act.get(key).and_then(Value::as_array).into_iter().flatten() {
            take(l.get("a"));
            take(l.get("asset"));
        }
    }
    take(act.get("order").and_then(|o| o.get("a")));
    for m in act
        .get("modifies")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        take(m.get("order").and_then(|o| o.get("a")));
    }
    out
}

/// True when the block names, or native's diffs for it touch, a book the
/// asset table does not hold (not seeded): such a block cannot count.
#[must_use]
pub fn touches_unseeded(meta: &AssetTable, b: &crate::replay::ReplayBlock) -> bool {
    b.user_actions()
        .iter()
        .flat_map(named_assets)
        .any(|x| meta.get(x).is_none())
        || b.native_diffs()
            .iter()
            .any(|d| meta.by_coin(&d.coin).is_none())
}

/// One asset's static metadata.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct AssetMeta {
    /// Hyperliquid asset index (perp i, spot 10000+i, builder perp 100000+dex*10000+i).
    pub asset: u32,
    /// Coin name as raw book diffs show it (`BTC`, `@107`, `PURR/USDC`, `xyz:TSLA`).
    pub coin: String,
    /// Size decimals.
    pub sz_decimals: u32,
    /// Maximum price decimals (perp 6 - szDecimals, spot 8 - szDecimals).
    pub px_decimals: u32,
}

#[derive(serde::Deserialize)]
struct MetaFile {
    assets: Vec<AssetMeta>,
}

/// Asset metadata by id and by coin.
#[derive(Clone, Debug, Default)]
pub struct AssetTable {
    by_id: HashMap<u32, AssetMeta>,
    by_coin: HashMap<String, u32>,
}

impl AssetTable {
    /// From the `asset_meta.json` shape: `{"assets":[{asset, coin, sz_decimals, px_decimals}, ..]}`.
    pub fn from_json(s: &str) -> Result<Self, String> {
        let f: MetaFile = serde_json::from_str(s).map_err(|e| format!("asset meta: {e}"))?;
        Ok(Self::new(f.assets))
    }

    /// From a list.
    #[must_use]
    pub fn new(assets: Vec<AssetMeta>) -> Self {
        let mut t = Self::default();
        for a in assets {
            t.by_coin.insert(a.coin.clone(), a.asset);
            t.by_id.insert(a.asset, a);
        }
        t
    }

    /// By asset id.
    #[must_use]
    pub fn get(&self, asset: u32) -> Option<&AssetMeta> {
        self.by_id.get(&asset)
    }

    /// By coin name.
    #[must_use]
    pub fn by_coin(&self, coin: &str) -> Option<&AssetMeta> {
        self.by_coin.get(coin).and_then(|a| self.by_id.get(a))
    }

    /// Only the assets `keep` accepts.
    #[must_use]
    pub fn filtered(&self, keep: impl Fn(&AssetMeta) -> bool) -> Self {
        Self::new(self.by_id.values().filter(|m| keep(m)).cloned().collect())
    }

    /// Every asset id, sorted.
    #[must_use]
    pub fn ids(&self) -> Vec<Asset> {
        let mut v: Vec<Asset> = self.by_id.keys().copied().collect();
        v.sort_unstable();
        v
    }
}

/// Why a decimal string did not convert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units {
    /// Not a plain non-negative decimal.
    Invalid,
    /// More significant decimals than allowed.
    TooManyDecimals,
    /// Does not fit in u64.
    Overflow,
}

/// `"12.340"` at 3 decimals -> 12340. Trailing zeros past `dec` are fine.
pub fn to_units(s: &str, dec: u32) -> Result<u64, Units> {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if (int.is_empty() && frac.is_empty())
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Units::Invalid);
    }
    let frac = frac.trim_end_matches('0');
    let d = usize::try_from(dec).map_err(|_| Units::Overflow)?;
    if frac.len() > d {
        return Err(Units::TooManyDecimals);
    }
    let mut v: u64 = 0;
    for b in int.bytes().chain(frac.bytes()) {
        v = v
            .checked_mul(10)
            .and_then(|x| x.checked_add(u64::from(b - b'0')))
            .ok_or(Units::Overflow)?;
    }
    for _ in frac.len()..d {
        v = v.checked_mul(10).ok_or(Units::Overflow)?;
    }
    Ok(v)
}

/// Units at `dec` decimals as the shortest decimal string (`12.34`, `5`),
/// or with at least one fractional digit (`5.0`) when `point` is set.
#[must_use]
pub fn fmt_units(units: u64, dec: u32, point: bool) -> String {
    let scale = 10u64.checked_pow(dec).unwrap_or(1);
    let (whole, frac) = (units / scale, units % scale);
    if frac == 0 {
        return if point {
            format!("{whole}.0")
        } else {
            whole.to_string()
        };
    }
    let width = usize::try_from(dec).unwrap_or(0);
    let digits = format!("{frac:0width$}");
    format!("{whole}.{}", digits.trim_end_matches('0'))
}

/// A decimal string in canonical form (no leading zeros, no trailing
/// fractional zeros, no trailing point), or `None` if not a plain decimal.
#[must_use]
pub fn canonical_decimal(s: &str) -> Option<String> {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if (int.is_empty() && frac.is_empty())
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let int = int.trim_start_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    let frac = frac.trim_end_matches('0');
    // Native prints some sizes through f64 ("3748301.7000000002"): 16+
    // significant digits is float noise (no asset grid needs that many), so
    // round the decimal (integer digit arithmetic) to 15 significant digits.
    let sig = format!("{int}{frac}").trim_start_matches('0').len();
    if sig >= 16 && !frac.is_empty() {
        return Some(round_sig15(int, frac));
    }
    Some(if frac.is_empty() {
        int.to_owned()
    } else {
        format!("{int}.{frac}")
    })
}

/// `int.frac` (canonical, 16+ significant digits) rounded half-up to 15
/// significant digits, canonical.
fn round_sig15(int: &str, frac: &str) -> String {
    let mut d: Vec<u8> = int.bytes().chain(frac.bytes()).map(|b| b - b'0').collect();
    let point = int.len();
    let lead = d.iter().position(|&x| x != 0).unwrap_or(d.len());
    let keep = lead + 15;
    if keep < d.len() {
        let up = d[keep] >= 5;
        d.truncate(keep);
        let mut i = keep;
        let mut carry = up;
        while carry && i > 0 {
            i -= 1;
            if d[i] == 9 {
                d[i] = 0;
            } else {
                d[i] += 1;
                carry = false;
            }
        }
        if carry {
            d.insert(0, 1);
            let (i2, f2) = digits_split(&d, point + 1);
            return canonical_decimal(&format!("{i2}.{f2}")).unwrap_or_default();
        }
    }
    let (i2, f2) = digits_split(&d, point);
    canonical_decimal(&format!("{i2}.{f2}")).unwrap_or_default()
}

fn digits_split(d: &[u8], point: usize) -> (String, String) {
    let s: String = d.iter().map(|&x| char::from(b'0' + x)).collect();
    let point = point.min(s.len());
    let (a, b) = s.split_at(point);
    (
        if a.is_empty() {
            "0".to_owned()
        } else {
            a.to_owned()
        },
        b.to_owned(),
    )
}

/// Significant figures of a non-integer decimal string; 0 for an integer
/// (native: integer prices are always allowed).
fn sig_figs(s: &str) -> usize {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        return 0;
    }
    let all = format!("{int}{frac}");
    all.trim_start_matches('0').len()
}

fn parse_user(s: &str) -> Option<User> {
    let h = s.strip_prefix("0x")?;
    if h.len() != 40 {
        return None;
    }
    let mut u = [0u8; 20];
    for (i, b) in u.iter_mut().enumerate() {
        *b = u8::from_str_radix(h.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(User(u))
}

fn fmt_user(u: &User) -> String {
    let mut s = String::with_capacity(42);
    s.push_str("0x");
    for b in u.0 {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn parse_cloid(s: &str) -> Option<Cloid> {
    let h = s.strip_prefix("0x")?;
    if h.len() != 32 {
        return None;
    }
    u128::from_str_radix(h, 16).ok().map(Cloid)
}

fn fmt_cloid(c: Cloid) -> String {
    format!("0x{:032x}", c.0)
}

/// How one action was converted, for rendering it back.
#[derive(Clone, Debug)]
enum Kind {
    Order { legs: Vec<LegInfo> },
    Cancel { assets: Vec<Asset> },
    Modify { single: bool, legs: Vec<LegInfo> },
    Noop,
    Unsupported,
}

#[derive(Clone, Copy, Debug)]
struct LegInfo {
    asset: Asset,
    cloid: Option<Cloid>,
}

#[derive(Default)]
struct Converted {
    kinds: Vec<Kind>,
    users: Vec<User>,
    orders: Vec<OrderReq>,
    cancels: Vec<CancelReq>,
    modifies: Vec<ModifyReq>,
    ranges: Vec<(usize, usize)>,
    types: Vec<String>,
}

fn leg_req(meta: &AssetTable, leg: &Value, priority: u64) -> Option<(OrderReq, LegInfo)> {
    let asset = u32::try_from(leg.get("a")?.as_u64()?).ok()?;
    let m = meta.get(asset)?;
    let side = if leg.get("b")?.as_bool()? {
        Side::Bid
    } else {
        Side::Ask
    };
    let p = leg.get("p")?.as_str()?;
    let s = leg.get("s")?.as_str()?;
    let reduce_only = leg.get("r").and_then(Value::as_bool).unwrap_or(false);
    let tif = match leg
        .get("t")
        .and_then(|t| t.get("limit"))
        .and_then(|l| l.get("tif"))
        .and_then(Value::as_str)
    {
        Some("Gtc") => Tif::Gtc,
        Some("Alo") => Tif::Alo,
        Some("Ioc") => Tif::Ioc,
        _ => Tif::Other,
    };
    let cloid = match leg.get("c") {
        None | Some(Value::Null) => None,
        Some(c) => Some(parse_cloid(c.as_str()?)?),
    };
    // Static rules, from the action and asset metadata only. Price before
    // size: the data has p=0,s=0 answered "Order has invalid price.".
    let mut pre = None;
    let px = match to_units(p, m.px_decimals) {
        Ok(0) => {
            pre = Some(RejectReason::ZeroPrice);
            0
        }
        Ok(v) if sig_figs(p) > 5 => {
            pre = Some(RejectReason::TickSize);
            v
        }
        Ok(v) => v,
        Err(Units::TooManyDecimals) => {
            pre = Some(RejectReason::TickSize);
            0
        }
        Err(_) => return None,
    };
    let sz = if pre.is_some() {
        to_units(s, m.sz_decimals).unwrap_or(0)
    } else {
        match to_units(s, m.sz_decimals) {
            Ok(0) => {
                pre = Some(RejectReason::ZeroSize);
                0
            }
            Ok(v) => v,
            Err(Units::TooManyDecimals) => {
                pre = Some(RejectReason::LotSize);
                0
            }
            Err(_) => return None,
        }
    };
    Some((
        OrderReq {
            asset,
            side,
            px: Px(px),
            sz: Sz(sz),
            reduce_only,
            tif,
            cloid,
            priority,
            pre_reject: pre,
            oid_hint: None,
        },
        LegInfo { asset, cloid },
    ))
}

fn order_ref(v: &Value) -> Option<OrderRef> {
    match v {
        Value::Number(n) => n.as_u64().map(|o| OrderRef::Oid(Oid(o))),
        Value::String(s) => parse_cloid(s).map(OrderRef::Cloid),
        _ => None,
    }
}

impl Converted {
    fn push_unsupported(&mut self, t: &str) {
        self.kinds.push(Kind::Unsupported);
        self.users.push(User::default());
        self.ranges.push((0, 0));
        self.types.push(t.to_owned());
    }

    fn convert(meta: &AssetTable, acts: &[UserAction<'_>]) -> Self {
        let mut c = Converted::default();
        for a in acts {
            let user = a.user.and_then(parse_user);
            let act = a.action;
            let done = match (a.action_type, user) {
                ("noop", _) => {
                    c.kinds.push(Kind::Noop);
                    c.users.push(user.unwrap_or_default());
                    c.ranges.push((0, 0));
                    c.types.push(String::new());
                    true
                }
                ("order", Some(u)) => c.order(meta, u, act),
                ("cancel" | "cancelByCloid", Some(u)) => c.cancel(meta, u, act, a.action_type),
                ("modify", Some(u)) => c.modify(meta, u, act, true),
                ("batchModify", Some(u)) => c.modify(meta, u, act, false),
                _ => false,
            };
            if !done {
                c.push_unsupported(a.action_type);
            }
        }
        c
    }

    fn order(&mut self, meta: &AssetTable, u: User, act: &Value) -> bool {
        let priority = match act.get("grouping") {
            None | Some(Value::Null) => 0,
            Some(Value::String(g)) if g == "na" => 0,
            Some(g) => match g.get("p").and_then(Value::as_u64) {
                Some(p) if g.as_object().is_some_and(|o| o.len() == 1) => p,
                // TP/SL groupings need trigger and position state.
                _ => return false,
            },
        };
        let Some(legs) = act.get("orders").and_then(Value::as_array) else {
            return false;
        };
        let start = self.orders.len();
        let mut infos = Vec::with_capacity(legs.len());
        for l in legs {
            let Some((req, info)) = leg_req(meta, l, priority) else {
                self.orders.truncate(start);
                return false;
            };
            self.orders.push(req);
            infos.push(info);
        }
        self.kinds.push(Kind::Order { legs: infos });
        self.users.push(u);
        self.ranges.push((start, self.orders.len()));
        self.types.push("order".into());
        true
    }

    fn cancel(&mut self, meta: &AssetTable, u: User, act: &Value, t: &str) -> bool {
        let Some(cs) = act.get("cancels").and_then(Value::as_array) else {
            return false;
        };
        let start = self.cancels.len();
        let mut assets = Vec::with_capacity(cs.len());
        for c in cs {
            let req = if t == "cancel" {
                c.get("a")
                    .and_then(Value::as_u64)
                    .zip(c.get("o").and_then(Value::as_u64))
                    .and_then(|(a, o)| {
                        Some(CancelReq {
                            asset: u32::try_from(a).ok()?,
                            target: OrderRef::Oid(Oid(o)),
                        })
                    })
            } else {
                c.get("asset")
                    .and_then(Value::as_u64)
                    .zip(c.get("cloid").and_then(Value::as_str).and_then(parse_cloid))
                    .and_then(|(a, cl)| {
                        Some(CancelReq {
                            asset: u32::try_from(a).ok()?,
                            target: OrderRef::Cloid(cl),
                        })
                    })
            };
            // A book the asset table does not hold (not seeded) is not ours
            // to answer for: unsupported, like an order there.
            let Some(req) = req.filter(|r| meta.get(r.asset).is_some()) else {
                self.cancels.truncate(start);
                return false;
            };
            assets.push(req.asset);
            self.cancels.push(req);
        }
        self.kinds.push(Kind::Cancel { assets });
        self.users.push(u);
        self.ranges.push((start, self.cancels.len()));
        self.types.push(t.to_owned());
        true
    }

    fn modify(&mut self, meta: &AssetTable, u: User, act: &Value, single: bool) -> bool {
        let one;
        let mods: &[Value] = if single {
            one = [act.clone()];
            &one
        } else {
            match act.get("modifies").and_then(Value::as_array) {
                Some(m) => m,
                None => return false,
            }
        };
        let start = self.modifies.len();
        let mut infos = Vec::with_capacity(mods.len());
        for m in mods {
            let parsed = m
                .get("oid")
                .and_then(order_ref)
                .zip(m.get("order").and_then(|o| leg_req(meta, o, 0)));
            let Some((target, (order, info))) = parsed else {
                self.modifies.truncate(start);
                return false;
            };
            self.modifies.push(ModifyReq { target, order });
            infos.push(info);
        }
        self.kinds.push(Kind::Modify {
            single,
            legs: infos,
        });
        self.users.push(u);
        self.ranges.push((start, self.modifies.len()));
        self.types
            .push(if single { "modify" } else { "batchModify" }.into());
        true
    }

    fn actions(&self) -> Vec<XAction<'_>> {
        self.kinds
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let (s, e) = self.ranges[i];
                let user = self.users[i];
                match k {
                    Kind::Order { .. } => XAction::Order {
                        user,
                        orders: &self.orders[s..e],
                    },
                    Kind::Cancel { .. } => XAction::Cancel {
                        user,
                        cancels: &self.cancels[s..e],
                    },
                    Kind::Modify { .. } => XAction::Modify {
                        user,
                        modifies: &self.modifies[s..e],
                    },
                    Kind::Noop => XAction::Noop,
                    Kind::Unsupported => XAction::Unsupported(self.types[i].as_str()),
                }
            })
            .collect()
    }
}

fn px_str(meta: &AssetTable, asset: Asset, px: Option<Px>) -> String {
    match (px, meta.get(asset)) {
        (Some(p), Some(m)) => fmt_units(p.0, m.px_decimals, false),
        _ => "None".into(),
    }
}

/// Native's text for a reject, as the data shows it. `in_modify` = the leg
/// came from `modify` / `batchModify`.
fn reject_text(meta: &AssetTable, asset: Asset, r: RejectReason, in_modify: bool) -> String {
    let base = match r {
        RejectReason::AloWouldCross { bid, ask } => {
            if bid.is_none() || ask.is_none() {
                "Post only order would have immediately matched.".to_owned()
            } else {
                format!(
                    "Post only order would have immediately matched, bbo was {}@{}.",
                    px_str(meta, asset, bid),
                    px_str(meta, asset, ask)
                )
            }
        }
        RejectReason::IocNoMatch => {
            "Order could not immediately match against any resting orders.".into()
        }
        RejectReason::UnknownOrder => "Order was never placed, already canceled, or filled.".into(),
        RejectReason::ModifyUnknownOrder => return "Cannot modify canceled or filled order".into(),
        RejectReason::TickSize => "Price must be divisible by tick size.".into(),
        RejectReason::ZeroPrice => return "Order has invalid price.".into(),
        RejectReason::LotSize => return "Order has invalid size.".into(),
        RejectReason::ZeroSize => return "Order has zero size.".into(),
        other => return format!("p2 engine internal: {other:?}"),
    };
    if in_modify {
        // From the data: modify prefixes the order error and drops the
        // asset suffix (2 classes seen; the rest is INFERENCE).
        format!("Error placing new order during modify: {base}")
    } else {
        format!("{base} asset={asset}")
    }
}

fn order_status_json(
    meta: &AssetTable,
    st: OrderStatus,
    leg: LegInfo,
    in_modify: bool,
) -> Option<Value> {
    let with_cloid = |mut v: serde_json::Map<String, Value>| {
        if let Some(c) = leg.cloid {
            v.insert("cloid".into(), Value::String(fmt_cloid(c)));
        }
        Value::Object(v)
    };
    Some(match st {
        OrderStatus::Placed {
            oid,
            filled,
            notional,
            rested,
        } => {
            if rested.0 > 0 || filled.0 == 0 {
                let mut m = serde_json::Map::new();
                m.insert("oid".into(), json!(oid.0));
                json!({ "resting": with_cloid(m) })
            } else {
                let am = meta.get(leg.asset)?;
                // Average in ticks, rounded half up (UNVERIFIED: native's rounding).
                let f = u128::from(filled.0);
                let avg = u64::try_from((notional + f / 2) / f).ok()?;
                let mut m = serde_json::Map::new();
                m.insert(
                    "totalSz".into(),
                    Value::String(fmt_units(filled.0, am.sz_decimals, true)),
                );
                m.insert(
                    "avgPx".into(),
                    Value::String(fmt_units(avg, am.px_decimals, true)),
                );
                m.insert("oid".into(), json!(oid.0));
                json!({ "filled": with_cloid(m) })
            }
        }
        OrderStatus::Rejected(r) => {
            json!({ "error": reject_text(meta, leg.asset, r, in_modify) })
        }
        OrderStatus::Cancelled | OrderStatus::Unsupported => return None,
    })
}

#[allow(clippy::needless_pass_by_value)]
fn ok(kind: &str, statuses: Vec<Value>) -> Value {
    json!({"status":"ok","response":{"type":kind,"data":{"statuses":statuses}}})
}

/// Renders one action's outcome as native's response JSON; `None` =
/// unsupported (the block cannot count).
fn render_action(
    meta: &AssetTable,
    kind: &Kind,
    expired: bool,
    unsupported: bool,
    sts: &[OrderStatus],
) -> Option<Value> {
    if expired {
        return Some(json!({"status":"err","response":"Action already expired"}));
    }
    if unsupported || sts.contains(&OrderStatus::Unsupported) {
        return None;
    }
    match kind {
        Kind::Unsupported => None,
        Kind::Noop => Some(json!({"status":"ok","response":{"type":"default"}})),
        Kind::Order { legs } => {
            let mut v = Vec::with_capacity(sts.len());
            for (s, l) in sts.iter().zip(legs) {
                v.push(order_status_json(meta, *s, *l, false)?);
            }
            Some(ok("order", v))
        }
        Kind::Cancel { assets } => {
            let mut v = Vec::with_capacity(sts.len());
            for (s, a) in sts.iter().zip(assets) {
                v.push(match s {
                    OrderStatus::Cancelled => Value::String("success".into()),
                    OrderStatus::Rejected(r) => {
                        json!({"error": reject_text(meta, *a, *r, false)})
                    }
                    _ => return None,
                });
            }
            Some(ok("cancel", v))
        }
        Kind::Modify { single: true, legs } => {
            let (s, l) = (sts.first()?, legs.first()?);
            match s {
                OrderStatus::Placed { .. } => {
                    Some(json!({"status":"ok","response":{"type":"default"}}))
                }
                OrderStatus::Rejected(r) => {
                    Some(json!({"status":"err","response": reject_text(meta, l.asset, *r, true)}))
                }
                _ => None,
            }
        }
        Kind::Modify {
            single: false,
            legs,
        } => {
            let mut v = Vec::with_capacity(sts.len());
            for (s, l) in sts.iter().zip(legs) {
                v.push(order_status_json(meta, *s, *l, true)?);
            }
            Some(ok("order", v))
        }
    }
}

/// One of our diffs in native's raw book diff schema (canonical decimals).
fn to_node_diff(meta: &AssetTable, d: &XDiff) -> BookDiff {
    let (coin, pxd, szd) = meta.get(d.asset).map_or_else(
        || (format!("asset{}", d.asset), 0, 0),
        |m| (m.coin.clone(), m.px_decimals, m.sz_decimals),
    );
    let kind = match d.kind {
        DiffKind::New { sz, insert_before } => NDiffKind::New {
            sz: fmt_units(sz.0, szd, false),
            insert_before: insert_before.map(|o| o.0),
        },
        DiffKind::Update { orig, new } => NDiffKind::Update {
            orig_sz: fmt_units(orig.0, szd, false),
            new_sz: fmt_units(new.0, szd, false),
        },
        DiffKind::Remove => NDiffKind::Remove,
    };
    BookDiff {
        user: fmt_user(&d.user),
        oid: d.oid.0,
        coin,
        px: fmt_units(d.px.0, pxd, false),
        side: Some(match d.side {
            Side::Bid => NodeSide::Bid,
            Side::Ask => NodeSide::Ask,
        }),
        raw_book_diff: kind,
    }
}

/// A native raw book diff with canonical decimal strings and a lowercase
/// user, so it compares by value with what this adapter emits.
#[must_use]
pub fn canonical_node_diff(d: &BookDiff) -> BookDiff {
    let c = |s: &str| canonical_decimal(s).unwrap_or_else(|| s.to_owned());
    BookDiff {
        user: d.user.to_ascii_lowercase(),
        oid: d.oid,
        coin: d.coin.clone(),
        px: c(&d.px),
        side: d.side,
        raw_book_diff: match &d.raw_book_diff {
            NDiffKind::New { sz, insert_before } => NDiffKind::New {
                sz: c(sz),
                insert_before: *insert_before,
            },
            NDiffKind::Update { orig_sz, new_sz } => NDiffKind::Update {
                orig_sz: c(orig_sz),
                new_sz: c(new_sz),
            },
            NDiffKind::Remove => NDiffKind::Remove,
        },
    }
}

/// Native diff -> engine diff (for resync).
fn from_node_diff(meta: &AssetTable, d: &BookDiff) -> Result<XDiff, String> {
    let m = meta
        .by_coin(&d.coin)
        .ok_or_else(|| format!("coin {} not in asset table", d.coin))?;
    // Through canonical_decimal: native prints some sizes with f64 noise.
    let units =
        |s: &str, dec: u32| to_units(&canonical_decimal(s).unwrap_or_else(|| s.to_owned()), dec);
    let px = units(&d.px, m.px_decimals).map_err(|e| format!("px {}: {e:?}", d.px))?;
    let sz = |s: &str| units(s, m.sz_decimals).map_err(|e| format!("sz {s}: {e:?}"));
    let side = match d.side {
        Some(NodeSide::Bid) => Side::Bid,
        Some(NodeSide::Ask) => Side::Ask,
        None => return Err(format!("oid {}: diff without side", d.oid)),
    };
    let kind = match &d.raw_book_diff {
        NDiffKind::New {
            sz: s,
            insert_before,
        } => DiffKind::New {
            sz: Sz(sz(s)?),
            insert_before: insert_before.map(Oid),
        },
        NDiffKind::Update { orig_sz, new_sz } => DiffKind::Update {
            orig: Sz(sz(orig_sz)?),
            new: Sz(sz(new_sz)?),
        },
        NDiffKind::Remove => DiffKind::Remove,
    };
    Ok(XDiff {
        asset: m.asset,
        user: parse_user(&d.user).ok_or_else(|| format!("user {}", d.user))?,
        oid: Oid(d.oid),
        side,
        px: Px(px),
        kind,
        cloid: None,
    })
}

/// The hl-exec engine behind the replay driver's trait.
pub struct HlExecEngine {
    eng: hl_exec::Engine,
    meta: AssetTable,
    last_core_ns: Option<u64>,
    unadopted_diffs: u64,
}

impl std::fmt::Debug for HlExecEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HlExecEngine")
            .field("orders", &self.eng.order_count())
            .field("next_oid", &self.eng.next_oid())
            .finish_non_exhaustive()
    }
}

impl HlExecEngine {
    /// Wraps an engine already loaded with a book.
    #[must_use]
    pub fn new(eng: hl_exec::Engine, meta: AssetTable) -> Self {
        Self {
            eng,
            meta,
            last_core_ns: None,
            unadopted_diffs: 0,
        }
    }

    /// The wrapped engine.
    #[must_use]
    pub fn engine(&self) -> &hl_exec::Engine {
        &self.eng
    }

    /// Native diffs that resync skipped because they are on a coin the asset
    /// table does not hold (a book the seed does not hold, e.g. spot).
    #[must_use]
    pub fn unadopted_diffs(&self) -> u64 {
        self.unadopted_diffs
    }
}

/// Action counts of the block's consecutive signed-action bundles, from each
/// action's bundle index (native's in-block order is per bundle).
fn bundle_lens(actions: &[UserAction<'_>]) -> Vec<u32> {
    let mut lens: Vec<u32> = Vec::new();
    let mut cur: Option<u32> = None;
    for a in actions {
        if cur == Some(a.bundle) {
            if let Some(l) = lens.last_mut() {
                *l = l.saturating_add(1);
            }
        } else {
            cur = Some(a.bundle);
            lens.push(1);
        }
    }
    lens
}

impl Engine for HlExecEngine {
    fn execute_block(&mut self, block_time_ns: u64, actions: &[UserAction<'_>]) -> BlockOutcome {
        let conv = Converted::convert(&self.meta, actions);
        let xa = conv.actions();
        let exp: Vec<Option<u64>> = actions.iter().map(|a| a.expires_after).collect();
        let lens = bundle_lens(actions);
        let t = Instant::now();
        let out = self
            .eng
            .execute_block_bundled(block_time_ns / 1_000_000, &xa, &exp, &lens);
        self.last_core_ns = Some(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
        let mut results = Vec::with_capacity(actions.len());
        for (i, k) in conv.kinds.iter().enumerate() {
            let rendered = out.responses.get(i).copied().and_then(|r| {
                render_action(&self.meta, k, r.expired, r.unsupported, out.statuses_of(i))
            });
            results.push(rendered.map_or(ActionResult::Unsupported, ActionResult::Response));
        }
        let book_diffs = out
            .diffs
            .iter()
            .map(|d| to_node_diff(&self.meta, d))
            .collect();
        BlockOutcome {
            results,
            book_diffs,
        }
    }

    fn core_exec_ns(&self) -> Option<u64> {
        self.last_core_ns
    }

    fn resync(
        &mut self,
        _height: u64,
        native_diffs: &[BookDiff],
        native_responses: &[Value],
    ) -> Result<(), String> {
        // Resync path only: the block is already counted as not reproduced.
        let mut cloids: HashMap<u64, Cloid> = HashMap::new();
        let mut max_oid = 0u64;
        for r in native_responses {
            let Some(sts) = r
                .pointer("/response/data/statuses")
                .and_then(Value::as_array)
            else {
                continue;
            };
            for s in sts {
                for k in ["resting", "filled"] {
                    let Some(o) = s.get(k) else { continue };
                    let Some(oid) = o.get("oid").and_then(Value::as_u64) else {
                        continue;
                    };
                    max_oid = max_oid.max(oid);
                    if let Some(c) = o.get("cloid").and_then(Value::as_str).and_then(parse_cloid) {
                        cloids.insert(oid, c);
                    }
                }
            }
        }
        let mut xs = Vec::with_capacity(native_diffs.len());
        for d in native_diffs {
            if self.meta.by_coin(&d.coin).is_none() {
                // Not a book we hold: every block touching it is already
                // unsupported or mismatched, so it never counts.
                self.unadopted_diffs += 1;
                continue;
            }
            let mut x = from_node_diff(&self.meta, d)?;
            if matches!(x.kind, DiffKind::New { .. }) {
                max_oid = max_oid.max(x.oid.0);
                x.cloid = cloids.get(&x.oid.0).copied();
            }
            xs.push(x);
        }
        self.eng.resync(&xs).map_err(|e| format!("{e:?}"))?;
        if max_oid > 0 && self.eng.next_oid().0 <= max_oid {
            self.eng.set_next_oid(Oid(max_oid + 1));
        }
        Ok(())
    }

    fn final_book(&self) -> Option<String> {
        let mut s = String::new();
        let mut books: Vec<_> = self.eng.books().collect();
        books.sort_by_key(|b| b.asset());
        for b in books {
            for side in [Side::Bid, Side::Ask] {
                for px in b.levels(side) {
                    for n in b.queue(side, px) {
                        let _ = writeln!(
                            s,
                            "{} {} {} {} {} {} {}",
                            b.asset(),
                            if side == Side::Bid { "B" } else { "A" },
                            n.px.0,
                            n.oid.0,
                            fmt_user(&n.user),
                            n.sz.0,
                            n.cloid.map_or_else(|| "-".into(), fmt_cloid)
                        );
                    }
                }
            }
        }
        let _ = writeln!(s, "next_oid {}", self.eng.next_oid().0);
        Some(s)
    }
}

/// Renderer check: parses native's own response for `a` into our status
/// types (with the action and asset metadata), renders it with this
/// adapter's renderer and says whether the result is value-equal.
/// `None` = a response the adapter never renders (unsupported action, or a
/// state-dependent error the engine does not model).
#[must_use]
pub fn roundtrip(meta: &AssetTable, a: &UserAction<'_>, native: &Value) -> Option<bool> {
    let conv = Converted::convert(meta, std::slice::from_ref(a));
    let kind = conv.kinds.first()?;
    if native.get("status").and_then(Value::as_str) == Some("err") {
        let msg = native.get("response").and_then(Value::as_str)?;
        if msg == "Action already expired" {
            return Some(render_action(meta, kind, true, false, &[])? == *native);
        }
        if let Kind::Modify { single: true, legs } = kind {
            let st = parse_error(meta, legs.first()?.asset, msg)?;
            return Some(render_action(meta, kind, false, false, &[st])? == *native);
        }
        return None;
    }
    let Some(Value::Array(sts_json)) = native.pointer("/response/data/statuses") else {
        return match kind {
            Kind::Noop | Kind::Modify { single: true, .. } => {
                let st = OrderStatus::Placed {
                    oid: Oid(0),
                    filled: Sz(0),
                    notional: 0,
                    rested: Sz(1),
                };
                Some(render_action(meta, kind, false, false, &[st])? == *native)
            }
            _ => None,
        };
    };
    let assets: Vec<Asset> = match kind {
        Kind::Order { legs } | Kind::Modify { legs, .. } => legs.iter().map(|l| l.asset).collect(),
        Kind::Cancel { assets } => assets.clone(),
        _ => return None,
    };
    if sts_json.len() != assets.len() {
        return None;
    }
    let mut sts = Vec::with_capacity(sts_json.len());
    for (s, asset) in sts_json.iter().zip(&assets) {
        sts.push(match s {
            Value::String(x) if x == "success" => OrderStatus::Cancelled,
            Value::Object(o) if o.contains_key("resting") => OrderStatus::Placed {
                oid: Oid(o["resting"].get("oid")?.as_u64()?),
                filled: Sz(0),
                notional: 0,
                rested: Sz(1),
            },
            Value::Object(o) if o.contains_key("filled") => {
                let f = &o["filled"];
                let m = meta.get(*asset)?;
                let filled = to_units(f.get("totalSz")?.as_str()?, m.sz_decimals).ok()?;
                let avg = to_units(f.get("avgPx")?.as_str()?, m.px_decimals).ok()?;
                OrderStatus::Placed {
                    oid: Oid(f.get("oid")?.as_u64()?),
                    filled: Sz(filled),
                    notional: u128::from(avg) * u128::from(filled),
                    rested: Sz(0),
                }
            }
            Value::Object(o) if o.contains_key("error") => {
                parse_error(meta, *asset, o["error"].as_str()?)?
            }
            _ => return None,
        });
    }
    Some(render_action(meta, kind, false, false, &sts)? == *native)
}

fn parse_error(meta: &AssetTable, asset: Asset, msg: &str) -> Option<OrderStatus> {
    let m = meta.get(asset)?;
    let body = msg
        .strip_prefix("Error placing new order during modify: ")
        .unwrap_or(msg);
    let r = if let Some(rest) =
        body.strip_prefix("Post only order would have immediately matched, bbo was ")
    {
        let core = rest.split(" asset=").next()?.trim_end_matches('.');
        let (b, a) = core.split_once('@')?;
        RejectReason::AloWouldCross {
            bid: Some(Px(to_units(b, m.px_decimals).ok()?)),
            ask: Some(Px(to_units(a, m.px_decimals).ok()?)),
        }
    } else if body.starts_with("Post only order would have immediately matched.") {
        RejectReason::AloWouldCross {
            bid: None,
            ask: None,
        }
    } else if body.starts_with("Order could not immediately match") {
        RejectReason::IocNoMatch
    } else if body.starts_with("Order was never placed") {
        RejectReason::UnknownOrder
    } else if body == "Cannot modify canceled or filled order" {
        RejectReason::ModifyUnknownOrder
    } else if body.starts_with("Price must be divisible by tick size") {
        RejectReason::TickSize
    } else if body == "Order has invalid price." {
        RejectReason::ZeroPrice
    } else if body == "Order has invalid size." {
        RejectReason::LotSize
    } else if body == "Order has zero size." {
        RejectReason::ZeroSize
    } else {
        return None;
    };
    Some(OrderStatus::Rejected(r))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn units_roundtrip() {
        assert_eq!(to_units("12.340", 3), Ok(12340));
        assert_eq!(to_units("12.3401", 3), Err(Units::TooManyDecimals));
        assert_eq!(to_units("0.041673", 6), Ok(41673));
        assert_eq!(fmt_units(41673, 6, false), "0.041673");
        assert_eq!(fmt_units(2_923_000, 3, true), "2923.0");
        assert_eq!(fmt_units(2_923_000, 3, false), "2923");
        assert_eq!(canonical_decimal("2923.0").as_deref(), Some("2923"));
        assert_eq!(canonical_decimal("0.0400").as_deref(), Some("0.04"));
        assert_eq!(sig_figs("1707.85"), 6);
        assert_eq!(sig_figs("0.041673"), 5);
        assert_eq!(sig_figs("61223"), 0);
    }

    #[test]
    fn user_and_cloid() {
        let u = "0x31ca8395cf837de08b24da3f660e77761dfb974b";
        assert_eq!(fmt_user(&parse_user(u).unwrap()), u);
        let c = "0xd2c92ce801457e8cb7e4869d2c1c15ff";
        assert_eq!(fmt_cloid(parse_cloid(c).unwrap()), c);
    }
}
