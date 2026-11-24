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

use std::collections::{HashMap, HashSet};
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

/// Outcome-market asset id base: coin `#E` is asset `100_000_000 + E` (lane
/// 1's `OUTCOME_ASSET_BASE`, from our node's order actions).
pub const OUTCOME_ASSET_BASE: u32 = 100_000_000;

/// Static meta for outcome coin `#E`: szDecimals 0 and 8 price decimals,
/// as lane 1 infers them (every `#E` size in the window is whole; the spot
/// rule 8 - szDecimals gives 8). `None` for any other coin.
#[must_use]
pub fn outcome_meta(coin: &str) -> Option<AssetMeta> {
    let e: u32 = coin.strip_prefix('#')?.parse().ok()?;
    Some(AssetMeta {
        asset: OUTCOME_ASSET_BASE.checked_add(e)?,
        coin: coin.to_owned(),
        sz_decimals: 0,
        px_decimals: 8,
    })
}

/// Book kind by asset id: perp below 10,000, spot 10,000..100,000,
/// builder perp 100,000..100,000,000, outcome from 100,000,000.
#[must_use]
pub fn asset_kind(asset: u32) -> &'static str {
    match asset {
        0..=9_999 => "perp",
        10_000..=99_999 => "spot",
        100_000..=99_999_999 => "builder-perp",
        _ => "outcome",
    }
}

/// Book kind by raw-diff coin name (`#E` outcome, `@N` or `A/B` spot,
/// `dex:NAME` builder perp, else perp).
#[must_use]
pub fn coin_kind(coin: &str) -> &'static str {
    if coin.starts_with('#') {
        "outcome"
    } else if coin.starts_with('@') || coin.contains('/') {
        "spot"
    } else if coin.contains(':') {
        "builder-perp"
    } else {
        "perp"
    }
}

/// Lane 1's l4 book seed (`l4so_<H>.json`, `abci_books_to_l4.py` output):
/// `[height, [[coin, [bids, asks]], ..], {"next_oid", ..}]`, each side a
/// queue-ordered list of `[user, {limitPx, sz, oid, timestamp, cloid,
/// isTrigger, ..}]`. Perp, spot and outcome books alike; a book the seed
/// lists is held even when empty. Trigger orders are not on the book and
/// are skipped, as lane 1's loader does.
#[derive(Clone, Debug)]
pub struct L4Seed {
    /// The state height the seed was cut at.
    pub height: u64,
    /// Native's next oid at that height, when the seed carries it.
    pub next_oid: Option<Oid>,
    /// Resting orders, per book in queue order.
    pub orders: Vec<RestingOrder>,
    /// The held books: every seeded coin's static meta.
    pub table: AssetTable,
    /// Seed coins with no meta (neither in the asset table nor `#E`).
    pub skipped_coins: Vec<String>,
}

impl L4Seed {
    /// Reads the seed, resolving coins through `all` (static meta for every
    /// listed asset) and `#E` outcome coins through [`outcome_meta`].
    pub fn read(reader: impl std::io::Read, all: &AssetTable) -> Result<Self, String> {
        let v: Value = serde_json::from_reader(reader).map_err(|e| format!("l4 seed: {e}"))?;
        let height = v
            .get(0)
            .and_then(Value::as_u64)
            .ok_or("l4 seed: [0] is not a height")?;
        let books = v
            .get(1)
            .and_then(Value::as_array)
            .ok_or("l4 seed: [1] is not a list of books")?;
        let next_oid = v
            .get(2)
            .and_then(|m| m.get("next_oid"))
            .and_then(Value::as_u64)
            .map(Oid);
        let mut orders = Vec::new();
        let mut held = Vec::new();
        let mut skipped_coins = Vec::new();
        for cb in books {
            let coin = cb
                .get(0)
                .and_then(Value::as_str)
                .ok_or("l4 seed: book without a coin")?;
            let Some(m) = all.by_coin(coin).cloned().or_else(|| outcome_meta(coin)) else {
                skipped_coins.push(coin.to_owned());
                continue;
            };
            for (si, side) in [(0, Side::Bid), (1, Side::Ask)] {
                let queue = cb
                    .get(1)
                    .and_then(|s| s.get(si))
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("l4 seed: {coin}: side {si} is not a list"))?;
                for uo in queue {
                    orders.extend(seed_order(&m, side, uo).map_err(|e| format!("{coin}: {e}"))?);
                }
            }
            held.push(m);
        }
        Ok(Self {
            height,
            next_oid,
            orders,
            table: AssetTable::new(held),
            skipped_coins,
        })
    }
}

/// One `[user, order]` entry of an l4 queue; `None` for a trigger order.
fn seed_order(m: &AssetMeta, side: Side, uo: &Value) -> Result<Option<RestingOrder>, String> {
    let (u, o) = (&uo[0], &uo[1]);
    if o.get("isTrigger").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(None);
    }
    let oid = o
        .get("oid")
        .and_then(Value::as_u64)
        .ok_or("order without oid")?;
    let dec = |k: &str, d: u32| {
        let s = o
            .get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("oid {oid}: no {k}"))?;
        to_units(&canonical_decimal(s).unwrap_or_else(|| s.to_owned()), d)
            .map_err(|e| format!("oid {oid}: {k} {s}: {e:?}"))
    };
    let user = u
        .as_str()
        .and_then(parse_user)
        .ok_or_else(|| format!("oid {oid}: user {u}"))?;
    Ok(Some(RestingOrder {
        asset: m.asset,
        user,
        oid: Oid(oid),
        side,
        px: Px(dec("limitPx", m.px_decimals)?),
        sz: Sz(dec("sz", m.sz_decimals)?),
        cloid: o.get("cloid").and_then(Value::as_str).and_then(parse_cloid),
        t_ms: o.get("timestamp").and_then(Value::as_u64).unwrap_or(0),
    }))
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

/// Every reason a block did not reproduce, from every action (not only the
/// first mismatch), as short class names for a per-height verdict file, in
/// the order they first occur (actions in block order, then diffs), so the
/// first entry is the first cause:
///
/// - `unseeded-book:<kind>`: an action names, or a native diff touches, a
///   book the seed does not hold but the asset meta lists (outcome coins
///   count as listed); `unknown-book:<kind>` when no meta lists it either.
///   `<kind>` is perp, spot, builder-perp or outcome.
/// - `unsupported:<type>`: the adapter does not model the action.
/// - `resp:<type>:<what>`: our response differs from native's; `<what>` is
///   `unknown-wallet`, `oid-offset` (equal but for oids), a native-only
///   reject the engine does not model (`margin`, `spot-balance`,
///   `min-value`, `reduce-only`, `too-many-orders`), `alo`,
///   `cancel-unknown`, `ioc-no-match`, `modify-unknown`, `filled`,
///   `resting` or `other`.
/// - `diffs:no-action` (book activity with no action): every action matched
///   and native's held diffs are ours behind a non-empty leading run whose
///   orders no action of this block placed (none of their oids has a
///   status with an action `hash` in `placed`, native's hashed order
///   statuses for the block; every non-`open` status has `hash: null`).
///   Native moved resting orders before any action we can see.
/// - `diffs:content` / `diffs:order`: the held books' diffs differ and no
///   action-level class explains the block (unexplained). When an action
///   class exists, a diff mismatch is attributed to it and not listed.
///   `diffs:no-action` needs `placed` (`None`: never given).
/// - `result-count`: our result list has the wrong length.
///
/// `seeded` holds the seeded books, `all` every asset the static meta lists.
/// Empty = every action and diff matched.
#[must_use]
#[allow(clippy::implicit_hasher)] // `None` at call sites; a hasher parameter would need a turbofish
pub fn block_classes(
    seeded: &AssetTable,
    all: &AssetTable,
    b: &crate::replay::ReplayBlock,
    out: &crate::engine::BlockOutcome,
    placed: Option<&HashSet<u64>>,
) -> Vec<String> {
    let mut cls: Vec<String> = Vec::new();
    let book_class = |listed: bool, kind: &str| {
        let what = if listed || kind == "outcome" {
            "unseeded-book"
        } else {
            "unknown-book"
        };
        format!("{what}:{kind}")
    };
    let native = b.native_responses();
    if out.results.len() != native.len() {
        push_once(&mut cls, "result-count".to_owned());
    }
    for (i, (a, nat)) in b.user_actions().iter().zip(native).enumerate() {
        let ours = out.results.get(i);
        if matches!(ours, Some(ActionResult::Response(v)) if v == nat) {
            continue;
        }
        if let Some(x) = named_assets(a)
            .into_iter()
            .find(|x| seeded.get(*x).is_none())
        {
            push_once(&mut cls, book_class(all.get(x).is_some(), asset_kind(x)));
            continue;
        }
        push_once(
            &mut cls,
            match ours {
                Some(ActionResult::Response(v)) => {
                    format!("resp:{}:{}", a.action_type, response_mismatch(nat, v))
                }
                _ => format!("unsupported:{}", a.action_type),
            },
        );
    }
    let action_level = !cls.is_empty();
    let mut held = Vec::new();
    for d in b.native_diffs() {
        if seeded.by_coin(&d.coin).is_some() {
            held.push(canonical_node_diff(d));
        } else {
            push_once(
                &mut cls,
                book_class(all.by_coin(&d.coin).is_some(), coin_kind(&d.coin)),
            );
        }
    }
    let ours: Vec<BookDiff> = out.book_diffs.iter().map(canonical_node_diff).collect();
    if !action_level && ours != held {
        let lead = held.len().saturating_sub(ours.len());
        if let Some(placed) = placed {
            if lead > 0
                && held[lead..] == ours[..]
                && held[..lead].iter().all(|d| !placed.contains(&d.oid))
            {
                push_once(&mut cls, "diffs:no-action".to_owned());
                return cls;
            }
        }
        let key = |d: &BookDiff| format!("{d:?}");
        let (mut x, mut y) = (ours, held);
        x.sort_by_key(key);
        y.sort_by_key(key);
        push_once(
            &mut cls,
            if x == y {
                "diffs:order"
            } else {
                "diffs:content"
            }
            .to_owned(),
        );
    }
    cls
}

/// Appends `c` unless `v` already holds it (keeps first-occurrence order).
fn push_once(v: &mut Vec<String>, c: String) {
    if !v.contains(&c) {
        v.push(c);
    }
}

/// What differs between native's response and ours (see [`block_classes`]).
fn response_mismatch(native: &Value, ours: &Value) -> &'static str {
    let (n, o) = (native.to_string(), ours.to_string());
    let either = |s: &str| n.contains(s) || o.contains(s);
    if n.contains("User or API Wallet ") && n.contains(" does not exist.") {
        "unknown-wallet"
    } else if zero_oids(native) == zero_oids(ours) {
        "oid-offset"
    } else if n.contains("Insufficient margin") {
        "margin"
    } else if n.contains("Insufficient spot balance") {
        "spot-balance"
    } else if n.contains("minimum value") {
        "min-value"
    } else if n.contains("educe only") {
        "reduce-only"
    } else if n.contains("Too many") {
        "too-many-orders"
    } else if either("Post only") {
        "alo"
    } else if either("never placed") {
        "cancel-unknown"
    } else if either("could not immediately match") {
        "ioc-no-match"
    } else if either("Cannot modify") {
        "modify-unknown"
    } else if either("\"filled\"") {
        "filled"
    } else if either("\"resting\"") {
        "resting"
    } else {
        "other"
    }
}

/// The legs [`HlExecEngine::with_reduce_only_as_plain`] passes as plain
/// orders: indexes of the action's reduce-only legs (`r: true`) with a Gtc,
/// Alo or Ioc limit, in `orders` (order), `modifies[].order` (batchModify) or
/// `order` (modify, index 0). Empty for any other action.
#[must_use]
pub fn plain_reduce_only_legs(a: &UserAction<'_>) -> Vec<usize> {
    let act = a.action;
    let legs: Vec<&Value> = match a.action_type {
        "order" => act
            .get("orders")
            .and_then(Value::as_array)
            .map(|v| v.iter().collect())
            .unwrap_or_default(),
        "batchModify" => act
            .get("modifies")
            .and_then(Value::as_array)
            .map(|v| v.iter().filter_map(|m| m.get("order")).collect())
            .unwrap_or_default(),
        "modify" => act.get("order").into_iter().collect(),
        _ => Vec::new(),
    };
    legs.iter()
        .enumerate()
        .filter(|(_, l)| {
            l.get("r").and_then(Value::as_bool) == Some(true)
                && matches!(
                    l.pointer("/t/limit/tif").and_then(Value::as_str),
                    Some("Gtc" | "Alo" | "Ioc")
                )
        })
        .map(|(i, _)| i)
        .collect()
}

/// One leg's part of a response: `statuses[leg]` when the response has a
/// status list, else the whole response (modify, action-level errors).
#[must_use]
pub fn leg_status(resp: &Value, leg: usize) -> Value {
    match resp
        .pointer("/response/data/statuses")
        .and_then(Value::as_array)
    {
        Some(sts) => sts.get(leg).cloned().unwrap_or(Value::Null),
        None => resp.clone(),
    }
}

/// The value with every `"oid"` number set to 0.
fn zero_oids(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| {
                    let x = if k == "oid" && x.is_number() {
                        json!(0)
                    } else {
                        zero_oids(x)
                    };
                    (k.clone(), x)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(zero_oids).collect()),
        other => other.clone(),
    }
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

    /// From a Hyperliquid info-API meta directory, by lane 1's rules
    /// (`crates/hl-exec/examples/replay.rs` `load_meta`): `meta.json`
    /// universe i = perp i (px 6 - szDecimals); `spotMeta.json` universe
    /// index i = spot 10000 + i, szDecimals of its base token (px 8 -
    /// szDecimals); `perpDexs.json` entry k with name N = builder perps
    /// 100000 + 10000k + i from `meta_N.json` (px 6 - szDecimals).
    pub fn from_meta_dir(dir: &std::path::Path) -> Result<Self, String> {
        let rd = |f: &str| -> Result<Value, String> {
            let p = dir.join(f);
            let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))
        };
        let n32 = |v: &Value, what: &str| -> Result<u32, String> {
            v.as_u64()
                .and_then(|x| u32::try_from(x).ok())
                .ok_or_else(|| format!("{what}: not a small integer: {v}"))
        };
        let mut out = Vec::new();
        let mut perps = |base: u32, m: &Value, f: &str| -> Result<(), String> {
            let uni = m["universe"]
                .as_array()
                .ok_or_else(|| format!("{f}: no universe"))?;
            for (i, u) in uni.iter().enumerate() {
                let sz = n32(&u["szDecimals"], f)?;
                out.push(AssetMeta {
                    asset: base + u32::try_from(i).map_err(|e| e.to_string())?,
                    coin: u["name"]
                        .as_str()
                        .ok_or_else(|| format!("{f}: name"))?
                        .to_owned(),
                    sz_decimals: sz,
                    px_decimals: 6u32.saturating_sub(sz),
                });
            }
            Ok(())
        };
        perps(0, &rd("meta.json")?, "meta.json")?;
        if let Some(dexs) = rd("perpDexs.json")?.as_array() {
            for (k, d) in dexs.iter().enumerate() {
                let Some(name) = d["name"].as_str() else {
                    continue;
                };
                let f = format!("meta_{name}.json");
                let k = u32::try_from(k).map_err(|e| e.to_string())?;
                perps(100_000 + k * 10_000, &rd(&f)?, &f)?;
            }
        }
        let spot = rd("spotMeta.json")?;
        let mut tok = HashMap::new();
        for t in spot["tokens"].as_array().into_iter().flatten() {
            tok.insert(t["index"].as_u64(), n32(&t["szDecimals"], "spot token")?);
        }
        for u in spot["universe"].as_array().into_iter().flatten() {
            let sz = *tok
                .get(&u["tokens"][0].as_u64())
                .ok_or_else(|| format!("spotMeta: pair {u} has no base token"))?;
            out.push(AssetMeta {
                asset: 10_000 + n32(&u["index"], "spot index")?,
                coin: u["name"].as_str().ok_or("spotMeta: name")?.to_owned(),
                sz_decimals: sz,
                px_decimals: 8u32.saturating_sub(sz),
            });
        }
        Ok(Self::new(out))
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
    Order {
        legs: Vec<LegInfo>,
    },
    Cancel {
        assets: Vec<Asset>,
    },
    Modify {
        single: bool,
        legs: Vec<LegInfo>,
    },
    Noop,
    /// `SetGlobalAction`: no book effect; native answers one constant
    /// (223 of 223 in 10,000 blocks). Acknowledged, state not modelled.
    SetGlobal,
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
    /// See [`HlExecEngine::with_reduce_only_as_plain`].
    fn reduce_only_as_plain(&mut self) {
        let legs = self
            .orders
            .iter_mut()
            .chain(self.modifies.iter_mut().map(|m| &mut m.order));
        for o in legs {
            if o.tif != Tif::Other {
                o.reduce_only = false;
            }
        }
    }

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
                ("SetGlobalAction", _) => {
                    c.kinds.push(Kind::SetGlobal);
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
                    Kind::Noop | Kind::SetGlobal => XAction::Noop,
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
        Kind::SetGlobal => Some(json!({"status":"ok","response":{"type":"setGlobal","data":[]}})),
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

/// Highest oid per height in native's order-status file
/// (`node_order_statuses_by_block`: one JSON line per block,
/// `{"block_number", "events": [{"order": {"oid"}, ..}]}`). It records every
/// order that took an oid, rejects past the static checks included, which
/// neither the responses (an error string) nor the book diffs show. Resync
/// only: it sets the counter after a block that did not reproduce.
pub fn load_status_max_oids(reader: impl std::io::BufRead) -> Result<HashMap<u64, u64>, String> {
    load_statuses(reader).map(|i| i.max_oids)
}

/// What resync takes from native's order-status file.
#[derive(Clone, Debug, Default)]
pub struct StatusIndex {
    /// Highest oid per height (see [`load_status_max_oids`]).
    pub max_oids: HashMap<u64, u64>,
    /// Every order's cloid by oid, where the status records one: resync
    /// gives an adopted order its cloid even when the block's responses do
    /// not show it (lane 1's harness does the same).
    pub cloids: HashMap<u64, Cloid>,
    /// Per height, the oids with a status carrying an action `hash` (orders
    /// an action of that block placed); every other status has `hash:
    /// null`. Classification only ([`block_classes`]'s `diffs:no-action`).
    pub placed: HashMap<u64, HashSet<u64>>,
}

/// Reads native's order-status file once for [`StatusIndex`].
pub fn load_statuses(reader: impl std::io::BufRead) -> Result<StatusIndex, String> {
    #[derive(serde::Deserialize)]
    struct Line {
        block_number: u64,
        events: Vec<Event>,
    }
    #[derive(serde::Deserialize)]
    struct Event {
        order: Option<Order>,
        #[serde(default)]
        hash: Option<serde::de::IgnoredAny>,
    }
    #[derive(serde::Deserialize)]
    struct Order {
        oid: Option<u64>,
        cloid: Option<String>,
    }
    let mut out = StatusIndex::default();
    for (i, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| format!("line {}: {e}", i + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let l: Line = serde_json::from_str(&line).map_err(|e| format!("line {}: {e}", i + 1))?;
        let mut max = None;
        for e in &l.events {
            let Some(o) = e.order.as_ref() else { continue };
            let Some(oid) = o.oid else { continue };
            max = max.max(Some(oid));
            if e.hash.is_some() {
                out.placed.entry(l.block_number).or_default().insert(oid);
            }
            if let Some(c) = o.cloid.as_deref().and_then(parse_cloid) {
                out.cloids.insert(oid, c);
            }
        }
        if let Some(m) = max {
            out.max_oids.insert(l.block_number, m);
        }
    }
    Ok(out)
}

/// The hl-exec engine behind the replay driver's trait.
pub struct HlExecEngine {
    eng: hl_exec::Engine,
    meta: AssetTable,
    last_core_ns: Option<u64>,
    unadopted_diffs: u64,
    status_max_oids: HashMap<u64, u64>,
    status_cloids: HashMap<u64, Cloid>,
    reduce_only_as_plain: bool,
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
            status_max_oids: HashMap::new(),
            status_cloids: HashMap::new(),
            reduce_only_as_plain: false,
        }
    }

    /// `--reduce-only-as-plain` (off by default; an approximation): a
    /// reduce-only leg with a Gtc, Alo or Ioc limit goes to the engine as a
    /// plain order (same price, size and TIF) with no position check, since
    /// we hold no positions. Native answers "Reduce only order would
    /// increase position" from position state we do not have. Trigger and
    /// `FrontendMarket` legs (`Tif::Other`) stay unsupported.
    #[must_use]
    pub fn with_reduce_only_as_plain(mut self, on: bool) -> Self {
        self.reduce_only_as_plain = on;
        self
    }

    /// Every order's cloid by oid from native's order-status file
    /// ([`load_statuses`]); resync gives an adopted order its cloid from it
    /// when the block's responses do not show one. Resync only.
    #[must_use]
    pub fn with_status_cloids(mut self, m: HashMap<u64, Cloid>) -> Self {
        self.status_cloids = m;
        self
    }

    /// Native's highest oid per height from its order-status file
    /// ([`load_status_max_oids`]); resync then sets the counter past it.
    #[must_use]
    pub fn with_status_max_oids(mut self, m: HashMap<u64, u64>) -> Self {
        self.status_max_oids = m;
        self
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
        let mut conv = Converted::convert(&self.meta, actions);
        if self.reduce_only_as_plain {
            conv.reduce_only_as_plain();
        }
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
        height: u64,
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
                x.cloid = cloids
                    .get(&x.oid.0)
                    .or_else(|| self.status_cloids.get(&x.oid.0))
                    .copied();
            }
            xs.push(x);
        }
        self.eng.resync(&xs).map_err(|e| format!("{e:?}"))?;
        if let Some(&m) = self.status_max_oids.get(&height) {
            max_oid = max_oid.max(m);
        }
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
            Kind::Noop | Kind::SetGlobal | Kind::Modify { single: true, .. } => {
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
