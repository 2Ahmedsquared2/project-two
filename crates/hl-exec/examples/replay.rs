//! Lane 1 diagnostic replay (not the official driver, which is lane 3's):
//! seed `hl-exec` from an `l4Snapshots` file, decode `replica_cmds`
//! (actions-and-responses), execute every block, and compare per block with
//! native's responses and raw book diffs. Prints k/N and the mismatch
//! classes, and times `execute_block` alone (decode is done before timing).
//!
//! ```text
//! cargo run --release -p hl-exec --example replay -- \
//!   --meta DIR --seed l4.json --replica replica.jsonl --diffs diffs.jsonl \
//!   [--statuses statuses.jsonl] [--max N] [--show K]
//! ```
//!
//! `--meta DIR` holds `meta.json`, `spotMeta.json`, `perpDexs.json` and
//! `meta_<dex>.json` from the public info API. Inputs are plain JSONL.
//!
//! Honesty rules (lead planner F3): a block counts only if it has no
//! unsupported action or leg, no oid hint differed from our counter, every
//! response equals native's and our diff sequence equals native's. Leg
//! pre-rejects are derived from the action and static metadata only. The
//! acting user is taken from native's response (`user`): signature recovery
//! and agent-to-master mapping are not implemented (input, not outcome).
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::float_arithmetic,
    clippy::as_conversions,
    clippy::disallowed_types,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::similar_names
)]

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::time::Instant;

use hl_exec::decimal::parse_fixed;
use hl_exec::{
    Asset, BookDiff, CancelReq, Capacity, Cloid, DiffKind, Engine, EngineConfig, ModifyReq, Oid,
    OrderRef, OrderReq, OrderStatus, Px, RejectReason, RestingOrder, Side, Sz, Tif, User,
    UserAction,
};
use serde_json::Value;

#[derive(Clone, Debug)]
struct AssetInfo {
    sz_dec: u32,
    px_dec: u32,
}

struct Meta {
    by_idx: HashMap<Asset, AssetInfo>,
    by_name: HashMap<String, Asset>,
}

fn load_meta(dir: &str) -> Meta {
    let rd = |f: &str| -> Value {
        serde_json::from_reader(BufReader::new(File::open(format!("{dir}/{f}")).unwrap())).unwrap()
    };
    let mut by_idx = HashMap::new();
    let mut by_name = HashMap::new();
    let mut add = |idx: Asset, name: String, sz_dec: u32, spot: bool| {
        let px_dec = if spot { 8 } else { 6 } - sz_dec;
        by_idx.insert(idx, AssetInfo { sz_dec, px_dec });
        by_name.insert(name, idx);
    };
    let meta = rd("meta.json");
    for (i, u) in meta["universe"].as_array().unwrap().iter().enumerate() {
        add(
            i as Asset,
            u["name"].as_str().unwrap().to_owned(),
            u["szDecimals"].as_u64().unwrap() as u32,
            false,
        );
    }
    let spot = rd("spotMeta.json");
    let toks: HashMap<u64, u32> = spot["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["index"].as_u64().unwrap(),
                t["szDecimals"].as_u64().unwrap() as u32,
            )
        })
        .collect();
    for u in spot["universe"].as_array().unwrap() {
        let base = u["tokens"][0].as_u64().unwrap();
        add(
            10000 + u["index"].as_u64().unwrap() as Asset,
            u["name"].as_str().unwrap().to_owned(),
            toks[&base],
            true,
        );
    }
    let dexs = rd("perpDexs.json");
    for (k, d) in dexs.as_array().unwrap().iter().enumerate() {
        let Some(name) = d["name"].as_str() else {
            continue;
        };
        let m = rd(&format!("meta_{name}.json"));
        for (i, u) in m["universe"].as_array().unwrap().iter().enumerate() {
            add(
                (100_000 + k * 10_000 + i) as Asset,
                u["name"].as_str().unwrap().to_owned(),
                u["szDecimals"].as_u64().unwrap() as u32,
                false,
            );
        }
    }
    Meta { by_idx, by_name }
}

fn hexbytes<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.strip_prefix("0x")?;
    if s.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

fn user(s: &str) -> User {
    User(hexbytes::<20>(s).unwrap_or([0; 20]))
}

fn cloid(v: &Value) -> Option<Cloid> {
    v.as_str()
        .and_then(hexbytes::<16>)
        .map(|b| Cloid(u128::from_be_bytes(b)))
}

fn fmt_units(v: u128, dec: u32) -> String {
    let p = 10u128.pow(dec);
    let (i, f) = (v / p, v % p);
    if f == 0 {
        return i.to_string();
    }
    let fs = format!("{f:0>w$}", w = dec as usize);
    format!("{i}.{}", fs.trim_end_matches('0'))
}

/// Decimal places of the fractional part, trailing zeros ignored.
fn decs(s: &str) -> usize {
    s.split_once('.')
        .map_or(0, |(_, f)| f.trim_end_matches('0').len())
}

fn sig_figs(s: &str) -> usize {
    let (i, f) = s.split_once('.').unwrap_or((s, ""));
    let f = f.trim_end_matches('0');
    if f.is_empty() {
        return 0;
    }
    let i = i.trim_start_matches('0');
    if i.is_empty() {
        f.trim_start_matches('0').len()
    } else {
        i.len() + f.len()
    }
}

/// One decoded order leg plus whether it is outside the model.
fn decode_leg(meta: &Meta, o: &Value, priority: u64) -> (OrderReq, bool) {
    let a = o["a"].as_u64().unwrap_or(u64::MAX) as Asset;
    let p = o["p"].as_str().unwrap_or("");
    let s = o["s"].as_str().unwrap_or("");
    let tif = match o["t"]["limit"]["tif"].as_str() {
        Some("Gtc") => Tif::Gtc,
        Some("Alo") => Tif::Alo,
        Some("Ioc") => Tif::Ioc,
        _ => Tif::Other,
    };
    let mut req = OrderReq {
        asset: a,
        side: if o["b"].as_bool().unwrap_or(false) {
            Side::Bid
        } else {
            Side::Ask
        },
        px: Px(0),
        sz: Sz(0),
        reduce_only: o["r"].as_bool().unwrap_or(false),
        tif,
        cloid: cloid(&o["c"]),
        priority,
        pre_reject: None,
        oid_hint: None,
    };
    let Some(info) = meta.by_idx.get(&a) else {
        return (req, true);
    };
    // Static checks, in the order the public sample supports (see PR notes).
    if decs(s) > info.sz_dec as usize {
        req.pre_reject = Some(RejectReason::LotSize);
    } else if decs(p) > info.px_dec as usize {
        req.pre_reject = Some(RejectReason::InvalidPrice);
    } else if sig_figs(p) > 5 {
        req.pre_reject = Some(RejectReason::TickSize);
    }
    req.px = Px(parse_fixed(p.as_bytes(), info.px_dec).unwrap_or(0));
    req.sz = Sz(parse_fixed(s.as_bytes(), info.sz_dec).unwrap_or(0));
    // Minimum notional ($10) is not modelled: the sample shows other checks
    // (margin, reduce-only) win in some cases, so such legs are unsupported.
    let notional = u128::from(req.px.0) * u128::from(req.sz.0);
    let ten = 10u128 * 10u128.pow(info.px_dec + info.sz_dec);
    let unsupported = req.pre_reject.is_none() && notional < ten;
    (req, unsupported)
}

#[derive(Default)]
struct Block {
    height: u64,
    time_ms: u64,
    users: Vec<User>,
    orders: Vec<Vec<OrderReq>>,
    cancels: Vec<Vec<CancelReq>>,
    modifies: Vec<Vec<ModifyReq>>,
    /// (kind, index into the vec of that kind)
    acts: Vec<(u8, usize)>,
    types: Vec<String>,
    native: Vec<Value>,
    expires: Vec<Option<u64>>,
    /// Every asset a decoded leg names (for the unseeded-book check).
    assets: Vec<Asset>,
    /// Actions per signed-action bundle (native's in-block order is per bundle).
    bundle_lens: Vec<u32>,
}

impl Block {
    fn actions(&self) -> Vec<UserAction<'_>> {
        self.acts
            .iter()
            .enumerate()
            .map(|(i, &(k, j))| match k {
                0 => UserAction::Order {
                    user: self.users[i],
                    orders: &self.orders[j],
                },
                1 => UserAction::Cancel {
                    user: self.users[i],
                    cancels: &self.cancels[j],
                },
                2 => UserAction::Modify {
                    user: self.users[i],
                    modifies: &self.modifies[j],
                },
                3 => UserAction::Noop,
                _ => UserAction::Unsupported("x"),
            })
            .collect()
    }
}

fn time_ms(s: &str) -> u64 {
    // "2026-11-23T23:32:07.188849213" -> ms since epoch (UTC).
    let (d, t) = s.split_once('T').unwrap();
    let mut it = d.split('-').map(|x| x.parse::<i64>().unwrap());
    let (y, m, dd) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
    let (hms, frac) = t.split_once('.').unwrap_or((t, "0"));
    let mut it = hms.split(':').map(|x| x.parse::<i64>().unwrap());
    let (h, mi, se) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
    let ms: i64 = format!("{frac:0<3}")[..3].parse().unwrap();
    // days from civil (Howard Hinnant)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (((days * 24 + h) * 60 + mi) * 60 + se) as u64 * 1000 + ms as u64
}

fn decode_block(meta: &Meta, line: &str) -> Block {
    let v: Value = serde_json::from_str(line).unwrap();
    let ab = &v["abci_block"];
    let mut b = Block {
        height: v["height"].as_u64().unwrap(),
        time_ms: time_ms(ab["time"].as_str().unwrap()),
        ..Block::default()
    };
    let resps = v["resps"]["Full"].as_array().unwrap();
    for (bundle, resp) in ab["signed_action_bundles"]
        .as_array()
        .unwrap()
        .iter()
        .zip(resps)
    {
        let sas = bundle[1]["signed_actions"].as_array().unwrap();
        let rs = resp[1].as_array().unwrap();
        assert_eq!(sas.len(), rs.len());
        b.bundle_lens.push(sas.len() as u32);
        for (sa, r) in sas.iter().zip(rs) {
            let a = &sa["action"];
            let t = a["type"].as_str().unwrap_or("?").to_owned();
            let u = user(r["user"].as_str().unwrap_or(""));
            b.users.push(u);
            b.native.push(r["res"].clone());
            let expired = sa["expiresAfter"].as_u64().is_some_and(|e| e < b.time_ms);
            let mut unsupported = expired;
            let kind = match t.as_str() {
                "order" => {
                    let prio = a["grouping"]["p"].as_u64().unwrap_or(0);
                    if !(a["grouping"] == "na" || a["grouping"]["p"].is_u64()) {
                        unsupported = true;
                    }
                    let mut legs = Vec::new();
                    for o in a["orders"].as_array().into_iter().flatten() {
                        let (leg, uns) = decode_leg(meta, o, prio);
                        unsupported |= uns || leg.reduce_only || leg.tif == Tif::Other;
                        b.assets.push(leg.asset);
                        legs.push(leg);
                    }
                    b.orders.push(legs);
                    0
                }
                "cancel" | "cancelByCloid" => {
                    let mut cs = Vec::new();
                    for c in a["cancels"].as_array().into_iter().flatten() {
                        let asset = c["a"].as_u64().or(c["asset"].as_u64()).unwrap_or(0) as Asset;
                        unsupported |= !meta.by_idx.contains_key(&asset);
                        b.assets.push(asset);
                        let target = if t == "cancel" {
                            OrderRef::Oid(Oid(c["o"].as_u64().unwrap_or(0)))
                        } else {
                            OrderRef::Cloid(cloid(&c["cloid"]).unwrap_or_default())
                        };
                        cs.push(CancelReq { asset, target });
                    }
                    b.cancels.push(cs);
                    1
                }
                "modify" | "batchModify" => {
                    let list: Vec<&Value> = if t == "modify" {
                        vec![a]
                    } else {
                        a["modifies"].as_array().into_iter().flatten().collect()
                    };
                    let mut ms = Vec::new();
                    for m in list {
                        let target = match &m["oid"] {
                            Value::String(_) => {
                                OrderRef::Cloid(cloid(&m["oid"]).unwrap_or_default())
                            }
                            x => OrderRef::Oid(Oid(x.as_u64().unwrap_or(0))),
                        };
                        let (leg, uns) = decode_leg(meta, &m["order"], 0);
                        unsupported |= uns || leg.reduce_only || leg.tif == Tif::Other;
                        b.assets.push(leg.asset);
                        ms.push(ModifyReq { target, order: leg });
                    }
                    b.modifies.push(ms);
                    2
                }
                "noop" => 3,
                _ => 4,
            };
            let kind = if unsupported { 4 } else { kind };
            b.acts.push((
                kind,
                match kind {
                    0 => b.orders.len() - 1,
                    1 => b.cancels.len() - 1,
                    2 => b.modifies.len() - 1,
                    _ => 0,
                },
            ));
            b.types.push(t);
        }
    }
    b
}

/// Native-shaped status string for one leg (compared with native's JSON).
fn leg_json(meta: &Meta, asset: Asset, cl: Option<Cloid>, s: OrderStatus) -> Value {
    let a = asset;
    let info = meta.by_idx.get(&a);
    let fmt_px = |p: Px| fmt_units(u128::from(p.0), info.map_or(0, |i| i.px_dec));
    let with_cloid = |mut m: serde_json::Map<String, Value>| {
        if let Some(c) = cl {
            m.insert("cloid".into(), Value::String(format!("0x{:032x}", c.0)));
        }
        Value::Object(m)
    };
    match s {
        OrderStatus::Placed {
            oid,
            filled,
            notional,
            rested,
        } => {
            if rested.0 > 0 {
                let mut m = serde_json::Map::new();
                m.insert("oid".into(), oid.0.into());
                serde_json::json!({ "resting": with_cloid(m) })
            } else {
                let i = info.unwrap();
                let mut m = serde_json::Map::new();
                let tot = fmt_units(u128::from(filled.0), i.sz_dec);
                let tot = if tot.contains('.') {
                    tot
                } else {
                    format!("{tot}.0")
                };
                m.insert("totalSz".into(), tot.into());
                // avgPx: exact notional / filled, rounded half-up to the price grid (UNVERIFIED rule).
                let f = u128::from(filled.0);
                let q = (notional * 2 + f) / (2 * f);
                m.insert("avgPx".into(), fmt_units(q, i.px_dec).into());
                m.insert("oid".into(), oid.0.into());
                serde_json::json!({ "filled": with_cloid(m) })
            }
        }
        OrderStatus::Cancelled => Value::String("success".into()),
        OrderStatus::Rejected(r) => {
            let e = match r {
                RejectReason::AloWouldCross { bid, ask } => format!(
                    "Post only order would have immediately matched, bbo was {}@{}. asset={a}",
                    bid.map_or_else(|| "None".into(), fmt_px),
                    ask.map_or_else(|| "None".into(), fmt_px)
                ),
                RejectReason::IocNoMatch => {
                    format!(
                        "Order could not immediately match against any resting orders. asset={a}"
                    )
                }
                RejectReason::UnknownOrder => {
                    format!("Order was never placed, already canceled, or filled. asset={a}")
                }
                RejectReason::ModifyUnknownOrder => "Cannot modify canceled or filled order".into(),
                RejectReason::TickSize => {
                    format!("Price must be divisible by tick size. asset={a}")
                }
                RejectReason::InvalidPrice => "Order has invalid price.".into(),
                RejectReason::LotSize => "Order has invalid size.".into(),
                other => format!("{other:?}"),
            };
            serde_json::json!({ "error": e })
        }
        OrderStatus::Unsupported => Value::String("UNSUPPORTED".into()),
    }
}

/// Our response for action `i` in native's shape.
fn response_json(meta: &Meta, b: &Block, i: usize, st: &[OrderStatus], expired: bool) -> Value {
    if expired {
        return serde_json::json!({"status":"err","response":"Action already expired"});
    }
    let (k, j) = b.acts[i];
    match k {
        0 => {
            let legs = &b.orders[j];
            let s: Vec<Value> = legs
                .iter()
                .zip(st)
                .map(|(l, &s)| leg_json(meta, l.asset, l.cloid, s))
                .collect();
            serde_json::json!({"status":"ok","response":{"type":"order","data":{"statuses":s}}})
        }
        1 => {
            let s: Vec<Value> = b.cancels[j]
                .iter()
                .zip(st)
                .map(|(c, &s)| leg_json(meta, c.asset, None, s))
                .collect();
            serde_json::json!({"status":"ok","response":{"type":"cancel","data":{"statuses":s}}})
        }
        2 => {
            let ms = &b.modifies[j];
            if b.types[i] == "modify" {
                match st[0] {
                    OrderStatus::Placed { rested, .. } if rested.0 > 0 => {
                        serde_json::json!({"status":"ok","response":{"type":"default"}})
                    }
                    OrderStatus::Rejected(RejectReason::ModifyUnknownOrder) => {
                        serde_json::json!({"status":"err","response":"Cannot modify canceled or filled order"})
                    }
                    s => {
                        let l = leg_json(meta, ms[0].order.asset, None, s);
                        match l.get("error") {
                            Some(e) => {
                                serde_json::json!({"status":"err","response":format!("Error placing new order during modify: {}", e.as_str().unwrap_or(""))})
                            }
                            None => {
                                serde_json::json!({"status":"ok","response":{"type":"default"}})
                            }
                        }
                    }
                }
            } else {
                let s: Vec<Value> = ms
                    .iter()
                    .zip(st)
                    .map(|(m, &s)| {
                        let l = leg_json(meta, m.order.asset, m.order.cloid, s);
                        match (&s, l.get("error")) {
                            (OrderStatus::Rejected(RejectReason::ModifyUnknownOrder), _) | (_, None) => l,
                            (_, Some(e)) => serde_json::json!({"error": format!("Error placing new order during modify: {}", e.as_str().unwrap_or(""))}),
                        }
                    })
                    .collect();
                serde_json::json!({"status":"ok","response":{"type":"order","data":{"statuses":s}}})
            }
        }
        3 => serde_json::json!({"status":"ok","response":{"type":"default"}}),
        _ => Value::Null,
    }
}

/// Parse a decimal from native's OUTPUT (diffs), which may print trailing
/// zeros past the asset's decimals ("2923.0" for a 0-decimal coin). Inputs
/// (order legs) stay on the strict `parse_fixed` path.
fn parse_out(s: &str, dec: u32) -> u64 {
    let t = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    };
    if let Some(v) = parse_fixed(t.as_bytes(), dec) {
        return v;
    }
    // Native prints some sizes via f64 ("3748301.7000000002"): round to the
    // asset's units and require the residual to be float noise only.
    let f: f64 = s
        .parse()
        .unwrap_or_else(|_| panic!("bad native decimal {s}"));
    let scaled = f * 10f64.powi(dec.cast_signed());
    let r = scaled.round();
    assert!(
        (scaled - r).abs() < 1e-3 && r >= 0.0,
        "native decimal {s} is not on a {dec}-decimal grid"
    );
    r as u64
}

fn parse_diffs(meta: &Meta, line: &str) -> (u64, Vec<BookDiff>, usize) {
    let v: Value = serde_json::from_str(line).unwrap();
    let h = v["block_number"].as_u64().unwrap();
    let mut out = Vec::new();
    let mut unknown = 0;
    for e in v["events"].as_array().unwrap() {
        let Some(&asset) = meta.by_name.get(e["coin"].as_str().unwrap_or("")) else {
            unknown += 1;
            continue;
        };
        let info = &meta.by_idx[&asset];
        let side = match e["side"].as_str() {
            Some("B" | "Bid") => Side::Bid,
            _ => Side::Ask,
        };
        let px = Px(parse_out(e["px"].as_str().unwrap(), info.px_dec));
        let sz = |s: &Value| Sz(parse_out(s.as_str().unwrap(), info.sz_dec));
        let d = &e["raw_book_diff"];
        let kind = if d == "remove" {
            DiffKind::Remove
        } else if let Some(n) = d.get("new") {
            DiffKind::New {
                sz: sz(&n["sz"]),
                insert_before: n["insertBefore"].as_u64().map(Oid),
            }
        } else {
            let u = &d["update"];
            DiffKind::Update {
                orig: sz(&u["origSz"]),
                new: sz(&u["newSz"]),
            }
        };
        out.push(BookDiff {
            asset,
            user: user(e["user"].as_str().unwrap_or("")),
            oid: Oid(e["oid"].as_u64().unwrap()),
            side,
            px,
            kind,
            cloid: None,
        });
    }
    (h, out, unknown)
}

struct Seed {
    height: u64,
    orders: Vec<RestingOrder>,
    skipped: usize,
    /// Native's next oid at the seed height, when the seed carries it.
    next_oid: Option<u64>,
    /// Assets whose book the seed holds (empty books included).
    seeded: std::collections::HashSet<Asset>,
}

fn load_seed(meta: &Meta, path: &str) -> Seed {
    let v: Value = serde_json::from_reader(BufReader::new(File::open(path).unwrap())).unwrap();
    let h = v[0].as_u64().unwrap();
    let mut out = Vec::new();
    let mut skipped = 0;
    let mut seeded = std::collections::HashSet::new();
    for cb in v[1].as_array().unwrap() {
        let coin = cb[0].as_str().unwrap();
        let Some(&asset) = meta.by_name.get(coin) else {
            skipped += 1;
            continue;
        };
        seeded.insert(asset);
        let info = &meta.by_idx[&asset];
        for (si, side) in [(0, Side::Bid), (1, Side::Ask)] {
            for uo in cb[1][si].as_array().unwrap() {
                let (u, o) = (&uo[0], &uo[1]);
                if o["isTrigger"].as_bool().unwrap_or(false) {
                    continue;
                }
                out.push(RestingOrder {
                    asset,
                    user: user(u.as_str().unwrap()),
                    oid: Oid(o["oid"].as_u64().unwrap()),
                    side,
                    px: Px(
                        parse_fixed(o["limitPx"].as_str().unwrap().as_bytes(), info.px_dec)
                            .unwrap(),
                    ),
                    sz: Sz(parse_fixed(o["sz"].as_str().unwrap().as_bytes(), info.sz_dec).unwrap()),
                    cloid: cloid(&o["cloid"]),
                    t_ms: o["timestamp"].as_u64().unwrap_or(0),
                });
            }
        }
    }
    Seed {
        height: h,
        orders: out,
        skipped,
        next_oid: v[2]["next_oid"].as_u64(),
        seeded,
    }
}

fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == name)
        .and_then(|i| a.get(i + 1).cloned())
}

fn main() {
    let meta = load_meta(&arg("--meta").expect("--meta"));
    let max: usize = arg("--max").map_or(usize::MAX, |s| s.parse().unwrap());
    let show: usize = arg("--show").map_or(5, |s| s.parse().unwrap());
    let Seed {
        height: seed_h,
        orders: seed,
        skipped,
        next_oid: seed_next,
        seeded,
    } = load_seed(&meta, &arg("--seed").expect("--seed"));
    eprintln!(
        "seed height {seed_h}: {} resting orders in {} books, {skipped} coins not in meta, next_oid {seed_next:?}",
        seed.len(),
        seeded.len()
    );
    let assets: Vec<Asset> = meta.by_idx.keys().copied().collect();
    let max_oid = seed.iter().map(|o| o.oid.0).max().unwrap_or(0);
    let mut e = Engine::from_l4_snapshot(
        &assets,
        seed,
        Some(Oid(seed_next.unwrap_or(max_oid + 1))),
        Capacity::default(),
        EngineConfig::default(),
    )
    .expect("seed loads");
    assert!(e.check_invariants());
    // Decode everything first so timing covers execute_block only.
    let rep = BufReader::new(File::open(arg("--replica").expect("--replica")).unwrap());
    let mut blocks = Vec::new();
    for line in rep.lines() {
        let line = line.unwrap();
        let b = decode_block(&meta, &line);
        if b.height <= seed_h {
            continue;
        }
        blocks.push(b);
        if blocks.len() >= max {
            break;
        }
    }
    let mut native_diffs: BTreeMap<u64, (Vec<BookDiff>, usize)> = BTreeMap::new();
    let mut unseeded_diffs: HashMap<u64, usize> = HashMap::new();
    let first = blocks.first().map_or(0, |b| b.height);
    let last = blocks.last().map_or(0, |b| b.height);
    for line in BufReader::new(File::open(arg("--diffs").expect("--diffs")).unwrap()).lines() {
        let line = line.unwrap();
        let (h, d, unk) = parse_diffs(&meta, &line);
        if h < first || h > last {
            continue;
        }
        // Diffs on books the seed does not hold are not compared or adopted;
        // the block is flagged unseeded-book instead.
        let (keep, drop): (Vec<BookDiff>, Vec<BookDiff>) =
            d.into_iter().partition(|x| seeded.contains(&x.asset));
        if !drop.is_empty() {
            *unseeded_diffs.entry(h).or_default() += drop.len();
        }
        let ent = native_diffs.entry(h).or_default();
        ent.0.extend(keep);
        ent.1 += unk;
    }
    // Native next-oid per block from statuses (resync path only).
    let mut status_max: HashMap<u64, u64> = HashMap::new();
    if let Some(p) = arg("--statuses") {
        for line in BufReader::new(File::open(p).unwrap()).lines() {
            let v: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let h = v["block_number"].as_u64().unwrap();
            if h < first || h > last {
                continue;
            }
            let m = v["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|s| s["order"]["oid"].as_u64())
                .max()
                .unwrap_or(0);
            let ent = status_max.entry(h).or_default();
            *ent = (*ent).max(m);
        }
    }
    let mut k = 0usize;
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    let mut examples: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut times_ns: Vec<u64> = Vec::with_capacity(blocks.len());
    let mut counted_ns: Vec<u64> = Vec::new();
    let mut global_max_oid = max_oid;
    let empty = (Vec::new(), 0usize);
    for b in &blocks {
        let acts = b.actions();
        let t0 = Instant::now();
        let out = e.execute_block_bundled(b.time_ms, &acts, &b.expires, &b.bundle_lens);
        let ns = t0.elapsed().as_nanos() as u64;
        times_ns.push(ns);
        let mut why: Vec<String> = Vec::new();
        // A block touching a book the seed does not hold cannot count.
        if b.assets.iter().any(|a| !seeded.contains(a)) || unseeded_diffs.contains_key(&b.height) {
            why.push("unseeded-book".into());
        }
        if out.has_unsupported {
            let mut kinds: Vec<&str> = b
                .acts
                .iter()
                .zip(&b.types)
                .filter(|(a, _)| a.0 == 4)
                .map(|(_, t)| t.as_str())
                .collect();
            kinds.sort_unstable();
            kinds.dedup();
            why.push(format!(
                "unsupported:{}",
                kinds.first().copied().unwrap_or("leg")
            ));
        }
        if out.oid_mismatches > 0 {
            why.push("oid-hint".into());
        }
        let mut resp_bad = None;
        for i in 0..b.acts.len() {
            let expired = out.responses[i].expired;
            if b.acts[i].0 == 4 && !expired {
                continue;
            }
            let ours = response_json(&meta, b, i, out.statuses_of(i), expired);
            if ours != b.native[i] {
                resp_bad = Some(format!(
                    "h{} act{i} {}: ours {} native {}",
                    b.height, b.types[i], ours, b.native[i]
                ));
                break;
            }
        }
        if let Some(ex) = resp_bad {
            let cls = if ex.contains("Post only") {
                "resp:alo"
            } else if ex.contains("never placed") {
                "resp:cancel-unknown"
            } else if ex.contains("\"filled\"") {
                "resp:filled"
            } else if ex.contains("\"resting\"") {
                "resp:resting"
            } else if ex.contains("modify") {
                "resp:modify"
            } else {
                "resp:other"
            };
            why.push(cls.into());
            let v = examples.entry(cls.into()).or_default();
            if v.len() < show {
                v.push(ex);
            }
        }
        let (nd, unk) = native_diffs.get(&b.height).unwrap_or(&empty);
        if *unk > 0 {
            why.push("diffs:unmodelled-coin".into());
        }
        let ours_d: Vec<BookDiff> = out
            .diffs
            .iter()
            .filter(|d| seeded.contains(&d.asset))
            .map(|d| BookDiff { cloid: None, ..*d })
            .collect();
        if &ours_d != nd {
            let mut a = ours_d.clone();
            let mut c = nd.clone();
            let key = |d: &BookDiff| {
                (
                    d.asset,
                    d.oid.0,
                    d.side == Side::Bid,
                    d.px.0,
                    format!("{:?}", d.kind),
                )
            };
            a.sort_by_key(key);
            c.sort_by_key(key);
            let cls = if a == c {
                "diffs:order"
            } else {
                "diffs:content"
            };
            why.push(cls.into());
            let v = examples.entry(cls.into()).or_default();
            if v.len() < show {
                let first_bad = ours_d
                    .iter()
                    .zip(nd.iter())
                    .position(|(x, y)| x != y)
                    .unwrap_or(ours_d.len().min(nd.len()));
                v.push(format!(
                    "h{} n_ours={} n_native={} first differing #{first_bad}: ours {:?} native {:?}",
                    b.height,
                    ours_d.len(),
                    nd.len(),
                    ours_d.get(first_bad),
                    nd.get(first_bad)
                ));
            }
        }
        for d in nd {
            if matches!(d.kind, DiffKind::New { .. }) {
                global_max_oid = global_max_oid.max(d.oid.0);
            }
        }
        if let Some(&m) = status_max.get(&b.height) {
            global_max_oid = global_max_oid.max(m);
        }
        if why.is_empty() {
            k += 1;
            counted_ns.push(ns);
        } else {
            for w in &why {
                *classes.entry(w.clone()).or_default() += 1;
            }
            // Adopt native's book and counter (non-reproduced block only).
            if let Err(err) = e.resync(nd) {
                eprintln!("h{}: resync failed: {err:?}; stopping", b.height);
                break;
            }
            e.set_next_oid(Oid(global_max_oid + 1));
        }
    }
    let n = blocks.len();
    println!("blocks N={n} heights {first}..={last}  reproduced k={k}");
    println!("non-reproduced blocks by class (a block can be in several):");
    let mut cl: Vec<_> = classes.into_iter().collect();
    cl.sort_by_key(|c| std::cmp::Reverse(c.1));
    for (c, v) in &cl {
        println!("  {v:6}  {c}");
    }
    for (c, ex) in &examples {
        println!("examples {c}:");
        for x in ex {
            println!("    {}", &x[..x.len().min(700)]);
        }
    }
    let stat = |v: &mut Vec<u64>, label: &str| {
        if v.is_empty() {
            return;
        }
        v.sort_unstable();
        let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        let mean = v.iter().sum::<u64>() / v.len() as u64;
        println!(
            "execute_block ns ({label}, n={}): median {} p90 {} p99 {} max {} mean {}",
            v.len(),
            q(0.5),
            q(0.9),
            q(0.99),
            v[v.len() - 1],
            mean
        );
    };
    stat(&mut times_ns, "all blocks");
    stat(&mut counted_ns, "reproduced blocks");
    assert!(e.check_invariants(), "engine invariants after replay");
}
