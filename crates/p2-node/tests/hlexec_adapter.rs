//! hl-exec adapter: our engine's answers rendered in native's format, and
//! the counted-path rules (F3: nothing from native's response reaches the
//! engine; oid counter adopted from native only on resync).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hl_exec::{Capacity, EngineConfig, Oid, Px, RestingOrder, Side, Sz, User};
use p2_node::engine::{ActionResult, Engine, UserAction};
use p2_node::hlexec::{AssetMeta, AssetTable, HlExecEngine};
use serde_json::{json, Value};

const ALICE: &str = "0x1111111111111111111111111111111111111111";
const BOB: &str = "0x2222222222222222222222222222222222222222";

fn meta() -> AssetTable {
    AssetTable::new(vec![AssetMeta {
        asset: 7,
        coin: "TST".into(),
        sz_decimals: 2,
        px_decimals: 4,
    }])
}

fn user(b: u8) -> User {
    User([b; 20])
}

/// Book: Bob asks 1.5 @ 10.0 (oid 100). Next oid 200.
fn engine() -> HlExecEngine {
    let e = hl_exec::Engine::from_l4_snapshot(
        &[7],
        [RestingOrder {
            asset: 7,
            user: user(0x22),
            oid: Oid(100),
            side: Side::Ask,
            px: Px(100_000),
            sz: Sz(150),
            cloid: None,
            t_ms: 0,
        }],
        Some(Oid(200)),
        Capacity {
            orders: 1024,
            levels_per_side: 64,
            actions_per_block: 64,
            events_per_block: 256,
        },
        EngineConfig::default(),
    )
    .unwrap();
    HlExecEngine::new(e, meta())
}

fn ua<'a>(t: &'a str, action: &'a Value, u: &'a str) -> UserAction<'a> {
    UserAction {
        action_type: t,
        action,
        nonce: 1,
        user: Some(u),
        vault_address: None,
        expires_after: None,
        bundle: 0,
    }
}

fn in_bundle(mut a: UserAction<'_>, bundle: u32) -> UserAction<'_> {
    a.bundle = bundle;
    a
}

fn status(r: &ActionResult) -> &Value {
    &resp(r)["response"]["data"]["statuses"][0]
}

fn diff_kinds(out: &p2_node::engine::BlockOutcome) -> Vec<String> {
    out.book_diffs
        .iter()
        .map(|d| {
            let k = format!("{:?}", d.raw_book_diff);
            let k = k.split([' ', '{', '(']).next().unwrap_or("").to_owned();
            format!("{} {k}", d.oid)
        })
        .collect()
}

fn resp(r: &ActionResult) -> &Value {
    match r {
        ActionResult::Response(v) => v,
        ActionResult::Unsupported => panic!("unsupported"),
    }
}

#[test]
fn alo_cross_rest_ioc_fill_cancel_rendered_like_native() {
    let mut e = engine();
    let alo = json!({"type":"order","orders":[{"a":7,"b":true,"p":"10.0","s":"1","r":false,"t":{"limit":{"tif":"Alo"}}}],"grouping":"na"});
    let gtc = json!({"type":"order","orders":[{"a":7,"b":true,"p":"9.5","s":"2","r":false,"t":{"limit":{"tif":"Gtc"}},"c":"0x0000000000000000000000000000abcd"}],"grouping":"na"});
    let ioc = json!({"type":"order","orders":[{"a":7,"b":true,"p":"10","s":"0.5","r":false,"t":{"limit":{"tif":"Ioc"}}}],"grouping":"na"});
    let cancel = json!({"type":"cancel","cancels":[{"a":7,"o":12345}]});
    let acts = [
        ua("order", &alo, ALICE),
        ua("order", &gtc, ALICE),
        ua("order", &ioc, ALICE),
        ua("cancel", &cancel, ALICE),
    ];
    let out = e.execute_block(1_000_000_000_000, &acts);
    assert_eq!(
        resp(&out.results[0]),
        &json!({"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"Post only order would have immediately matched. asset=7"}]}}})
    );
    assert_eq!(
        resp(&out.results[1]),
        &json!({"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":201,"cloid":"0x0000000000000000000000000000abcd"}}]}}})
    );
    assert_eq!(
        resp(&out.results[2]),
        &json!({"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"totalSz":"0.5","avgPx":"10.0","oid":202}}]}}})
    );
    assert_eq!(
        resp(&out.results[3]),
        &json!({"status":"ok","response":{"type":"cancel","data":{"statuses":[{"error":"Order was never placed, already canceled, or filled. asset=7"}]}}})
    );
    // Diffs in native's schema, canonical decimals.
    let d: Vec<String> = out
        .book_diffs
        .iter()
        .map(|d| format!("{} {} {:?}", d.oid, d.px, d.raw_book_diff))
        .collect();
    assert_eq!(d.len(), 2, "{d:?}");
    assert!(d[0].starts_with("201 9.5 New"), "{d:?}");
    assert!(d[1].starts_with("100 10 Update"), "{d:?}");
}

#[test]
fn unsupported_shapes_stay_unsupported() {
    let mut e = engine();
    let trig = json!({"type":"order","orders":[{"a":7,"b":true,"p":"10","s":"1","r":true,"t":{"trigger":{"isMarket":true,"triggerPx":"9","tpsl":"sl"}}}],"grouping":"positionTpsl"});
    let lev = json!({"type":"updateLeverage","asset":7,"isCross":true,"leverage":3});
    let unknown_asset = json!({"type":"order","orders":[{"a":99,"b":true,"p":"1","s":"1","r":false,"t":{"limit":{"tif":"Gtc"}}}],"grouping":"na"});
    let acts = [
        ua("order", &trig, ALICE),
        ua("updateLeverage", &lev, ALICE),
        ua("order", &unknown_asset, ALICE),
    ];
    let out = e.execute_block(1, &acts);
    assert!(out
        .results
        .iter()
        .all(|r| matches!(r, ActionResult::Unsupported)));
}

/// F3: the engine's oid comes from its own counter, never from native's
/// answer; when native's counter differs, the block cannot reproduce, and
/// only the resync path adopts native's counter.
#[test]
fn f3_oid_counter_is_ours_until_resync() {
    let mut e = engine();
    let gtc = json!({"type":"order","orders":[{"a":7,"b":false,"p":"11","s":"1","r":false,"t":{"limit":{"tif":"Gtc"}}}],"grouping":"na"});
    let acts = [ua("order", &gtc, BOB)];
    let out = e.execute_block(1, &acts);
    // Our counter (200), whatever native said.
    assert_eq!(
        resp(&out.results[0]),
        &json!({"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":200}}]}}})
    );
    // Resync with native's answer for that block (native gave oid 500).
    let native_resp = vec![
        json!({"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":500}}]}}}),
    ];
    let native_diffs: Vec<hl_node_convert::node::BookDiff> = serde_json::from_value(json!([
        {"user": BOB, "oid": 500, "coin": "TST", "side": "A", "px": "11.0", "raw_book_diff": {"new": {"sz": "1.0"}}}
    ]))
    .unwrap();
    e.resync(1, &native_diffs, &native_resp).unwrap();
    assert_eq!(e.engine().next_oid(), Oid(501));
    let book = e.final_book().unwrap();
    assert!(book.contains(" 500 "), "{book}");
    assert!(!book.contains(" 200 "), "{book}");
}

#[test]
fn final_book_is_canonical_queue_order() {
    let e = engine();
    let b = e.final_book().unwrap();
    assert_eq!(b, format!("7 A 100000 100 {BOB} 150 -\nnext_oid 200\n"));
}

/// Native's in-block order (lane 1, PR #11 at 58c1489): inside one signed
/// action bundle, cancels run before GTC/IOC orders, whatever their input
/// position; across bundles, input order holds. Responses stay indexed by
/// input position.
#[test]
fn cancel_runs_before_ioc_inside_a_bundle_but_not_across_bundles() {
    let ioc = json!({"type":"order","orders":[{"a":7,"b":true,"p":"10","s":"1.5","r":false,"t":{"limit":{"tif":"Ioc"}}}],"grouping":"na"});
    let cancel = json!({"type":"cancel","cancels":[{"a":7,"o":100}]});

    // One bundle [IOC, cancel]: the cancel runs first, so the IOC finds an
    // empty book.
    let mut e = engine();
    let acts = [ua("order", &ioc, ALICE), ua("cancel", &cancel, BOB)];
    let out = e.execute_block(1, &acts);
    assert_eq!(status(&out.results[1]), &json!("success"), "{out:?}");
    assert!(status(&out.results[0]).get("error").is_some(), "{out:?}");
    assert_eq!(diff_kinds(&out), vec!["100 Remove".to_owned()]);

    // Two bundles [IOC] [cancel]: the IOC fills Bob's whole ask first, so the
    // cancel finds nothing. A full fill is written as native writes it:
    // update to 0, then remove.
    let mut e = engine();
    let acts = [
        in_bundle(ua("order", &ioc, ALICE), 0),
        in_bundle(ua("cancel", &cancel, BOB), 1),
    ];
    let out = e.execute_block(1, &acts);
    assert!(status(&out.results[0]).get("filled").is_some(), "{out:?}");
    assert!(status(&out.results[1]).get("error").is_some(), "{out:?}");
    assert_eq!(
        diff_kinds(&out),
        vec!["100 Update".to_owned(), "100 Remove".to_owned()]
    );
}

/// A book the seed does not hold (lane 1's seed is perp books only): an
/// action naming it is unsupported, never answered from an empty book (a
/// cancel there would otherwise answer "never placed" and could match
/// native by accident).
#[test]
fn cancel_on_an_unseeded_book_is_unsupported() {
    let mut e = engine();
    let spot_cancel = json!({"type":"cancel","cancels":[{"a":10007,"o":5}]});
    let spot_cloid = json!({"type":"cancelByCloid","cancels":[{"asset":10007,"cloid":"0x0000000000000000000000000000abcd"}]});
    let acts = [
        ua("cancel", &spot_cancel, ALICE),
        ua("cancelByCloid", &spot_cloid, ALICE),
    ];
    let out = e.execute_block(1, &acts);
    assert!(
        out.results
            .iter()
            .all(|r| matches!(r, ActionResult::Unsupported)),
        "{out:?}"
    );
    assert_eq!(out.book_diffs.len(), 0);
}

/// Resync adopts native's diffs on the books we hold and skips (and counts)
/// those on books the seed does not hold.
#[test]
fn resync_skips_and_counts_diffs_on_unseeded_books() {
    let mut e = engine();
    let native_diffs: Vec<hl_node_convert::node::BookDiff> = serde_json::from_value(json!([
        {"user": BOB, "oid": 500, "coin": "@142", "side": "A", "px": "1.0", "raw_book_diff": {"new": {"sz": "1.0"}}},
        {"user": BOB, "oid": 100, "coin": "TST", "side": "A", "px": "10.0", "raw_book_diff": "remove"}
    ]))
    .unwrap();
    e.resync(1, &native_diffs, &[]).unwrap();
    assert_eq!(e.unadopted_diffs(), 1);
    assert!(!e.final_book().unwrap().contains(" 100 "));
}

/// Lane 1's resting-order seed (`resting_<H>.jsonl`): a header line, then one
/// resting order per line in queue order, raw integer ticks and lots.
#[test]
fn resting_seed_loads() {
    let text = format!(
        "{}\n{}\n{}\n",
        r#"{"height": 1232430000, "next_oid": 614579392802, "source": "test"}"#,
        format_args!(
            r#"{{"asset": 7, "user": "{BOB}", "oid": 9, "side": "A", "px_ticks": 100000, "sz_lots": 150, "cloid": null, "t_ms": 5, "reduce_only": false}}"#
        ),
        format_args!(
            r#"{{"asset": 7, "user": "{ALICE}", "oid": 8, "side": "B", "px_ticks": 90000, "sz_lots": 20, "cloid": "0x0000000000000000000000000000abcd", "t_ms": 6, "reduce_only": true}}"#
        ),
    );
    let s = p2_node::hlexec::RestingSeed::read(text.as_bytes()).unwrap();
    assert_eq!(s.height, 1_232_430_000);
    assert_eq!(s.next_oid, Some(Oid(614_579_392_802)));
    assert_eq!(s.orders.len(), 2);
    assert_eq!(s.orders[0].side, Side::Ask);
    assert_eq!(s.orders[0].px, Px(100_000));
    assert_eq!(s.orders[1].sz, Sz(20));
    assert!(s.orders[1].cloid.is_some());
    // A bad line is an error with its line number, not a skipped order.
    let bad = format!("{}\n{{\"asset\": 7}}\n", r#"{"height": 1}"#);
    let err = p2_node::hlexec::RestingSeed::read(bad.as_bytes()).unwrap_err();
    assert!(err.contains("line 2"), "{err}");
}

/// Native prints some diff sizes through f64 ("3748301.7000000002", 55 such
/// strings in our node's 2,000 diff lines). They are the asset-grid value
/// plus float noise: compare and resync read them as that value; plain
/// decimals keep every digit.
#[test]
fn native_f64_noise_in_diff_sizes_reads_as_the_grid_value() {
    use p2_node::hlexec::canonical_decimal;
    assert_eq!(
        canonical_decimal("3748301.7000000002").as_deref(),
        Some("3748301.7")
    );
    assert_eq!(
        canonical_decimal("3746131.6000000001").as_deref(),
        Some("3746131.6")
    );
    assert_eq!(
        canonical_decimal("0.30000000000000004").as_deref(),
        Some("0.3")
    );
    assert_eq!(canonical_decimal("12.5000").as_deref(), Some("12.5"));
    assert_eq!(
        canonical_decimal("9.9999999999999998").as_deref(),
        Some("10")
    );
    assert_eq!(
        canonical_decimal("1.2999999999999998").as_deref(),
        Some("1.3")
    );
    assert_eq!(
        canonical_decimal("0.000123456789012").as_deref(),
        Some("0.000123456789012")
    );
    let mut e = engine();
    let native_diffs: Vec<hl_node_convert::node::BookDiff> = serde_json::from_value(json!([
        {"user": BOB, "oid": 100, "coin": "TST", "side": "A", "px": "10.0", "raw_book_diff": {"update": {"origSz": "1.5", "newSz": "1.2000000000000002"}}}
    ]))
    .unwrap();
    e.resync(1, &native_diffs, &[]).unwrap();
    assert!(
        e.final_book().unwrap().contains(" 120"),
        "{}",
        e.final_book().unwrap()
    );
}

/// Rejected orders past the static checks spend an oid that neither native's
/// responses (an error string) nor its diffs show; native's order-status
/// file does. Block 1232430073 ended with two IOC no-match rejects that took
/// ..430 and ..431, so 074 started at ..432 while a counter taken from
/// responses and diffs started at ..430 (53 blocks lane 1 reproduces).
#[test]
fn resync_adopts_the_oid_counter_from_native_order_statuses() {
    let statuses = concat!(
        r#"{"block_number":1,"events":[{"status":"open","order":{"oid":500}},"#,
        r#"{"status":"iocCancelRejected","order":{"oid":502}}]}"#,
        "\n",
        r#"{"block_number":2,"events":[]}"#,
        "\n"
    );
    let max = p2_node::hlexec::load_status_max_oids(statuses.as_bytes()).unwrap();
    assert_eq!(max.get(&1), Some(&502));
    assert_eq!(max.get(&2), None);
    let mut e = engine().with_status_max_oids(max);
    let native_resp = vec![
        json!({"status":"ok","response":{"type":"order","data":{"statuses":[
            {"resting":{"oid":500}},
            {"error":"Order could not immediately match against any resting orders. asset=7"}
        ]}}}),
    ];
    let native_diffs: Vec<hl_node_convert::node::BookDiff> = serde_json::from_value(json!([
        {"user": BOB, "oid": 500, "coin": "TST", "side": "A", "px": "11.0", "raw_book_diff": {"new": {"sz": "1.0"}}}
    ]))
    .unwrap();
    e.resync(1, &native_diffs, &native_resp).unwrap();
    assert_eq!(e.engine().next_oid(), Oid(503));
}
