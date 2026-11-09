//! Brief #2 (A): parsing raw exchange actions, the tick/lot rule, and the
//! converter from the Hyperliquid Reason's public-data capture to format v2.
//! Fixtures are small excerpts in the exact shape of the capture files.
//! Written before the implementation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Cursor;

use hl_dataset::action::{self, Action, AssetClass, LimitTif, OrderKind, OrderRef};
use hl_dataset::convert::{assets_from_meta, convert};
use hl_dataset::{AssetMeta, BookState, Reader, TickError};

const META: &str = r#"{"meta":{"universe":[{"szDecimals":5,"name":"BTC","maxLeverage":40},{"szDecimals":4,"name":"ETH","maxLeverage":25}]},
"spotMeta":{"universe":[{"tokens":[1,0],"name":"PURR/USDC","index":0},{"tokens":[7,0],"name":"@9","index":9}],
"tokens":[{"name":"USDC","szDecimals":8,"index":0},{"name":"PURR","szDecimals":0,"index":1},{"name":"HFUN","szDecimals":2,"index":7}]},
"captured_at_ms":1}"#;

// ---- meta -> assets ----

#[test]
fn assets_come_from_perp_and_spot_meta_with_the_documented_indices() {
    let assets = assets_from_meta(META).unwrap();
    assert_eq!(
        assets,
        vec![
            AssetMeta::new(0, "BTC", 5, false).unwrap(),
            AssetMeta::new(1, "ETH", 4, false).unwrap(),
            // spot asset = 10000 + universe[].index; szDecimals from the base token
            // (looked up by the token's `index`, not its array position).
            AssetMeta::new(10000, "PURR/USDC", 0, true).unwrap(),
            AssetMeta::new(10009, "@9", 2, true).unwrap(),
        ]
    );
}

#[test]
fn asset_classes_follow_the_asset_id_ranges() {
    assert_eq!(action::asset_class(0), AssetClass::Perp);
    assert_eq!(action::asset_class(9999), AssetClass::Perp);
    assert_eq!(action::asset_class(10000), AssetClass::Spot);
    assert_eq!(action::asset_class(110_089), AssetClass::BuilderPerp);
    assert_eq!(action::asset_class(100_303_161), AssetClass::Other);
}

// ---- tick and lot ----

#[test]
fn order_prices_obey_decimals_and_five_significant_figures() {
    let btc = AssetMeta::new(0, "BTC", 5, false).unwrap(); // 1 price decimal
    assert_eq!(btc.order_px_ticks("74690"), Ok(746_900));
    assert_eq!(btc.order_px_ticks("74690.0"), Ok(746_900));
    assert_eq!(btc.order_px_ticks("123456"), Ok(1_234_560)); // integers always valid
    assert_eq!(
        btc.order_px_ticks("74690.5"),
        Err(TickError::TooManySignificantFigures)
    );
    assert!(matches!(
        btc.order_px_ticks("7469.05"),
        Err(TickError::Decimal(_))
    ));
    let small = AssetMeta::new(1, "X", 0, false).unwrap(); // 6 price decimals
    assert_eq!(small.order_px_ticks("0.012345"), Ok(12_345)); // leading zeros don't count
    assert_eq!(
        small.order_px_ticks("0.1234567"),
        Err(TickError::Decimal(
            hl_dataset::decimal::DecimalError::TooPrecise
        ))
    );
    assert_eq!(
        small.order_px_ticks("1.23456"),
        Err(TickError::TooManySignificantFigures)
    );
    assert_eq!(btc.sz_lots("0.15247"), Ok(15_247));
    assert!(btc.sz_lots("0.152471").is_err());
}

// ---- action parsing ----

#[test]
fn parses_order_actions_with_every_tif_and_trigger_orders() {
    let raw = r#"{"type":"order","orders":[{"a":0,"b":false,"p":"74690","s":"0.15247","r":false,"t":{"limit":{"tif":"Alo"}},"c":"0x16"},{"a":1,"b":true,"p":"2000","s":"1","r":true,"t":{"limit":{"tif":"FrontendMarket"}}},{"a":1,"b":true,"p":"2000","s":"1","r":true,"t":{"trigger":{"isMarket":true,"triggerPx":"1900","tpsl":"sl"}}}],"grouping":{"p":163}}"#;
    let Action::Order { orders } = action::parse(raw).unwrap() else {
        panic!("not an order");
    };
    assert_eq!(orders.len(), 3);
    assert_eq!(orders[0].a, 0);
    assert!(!orders[0].b);
    assert_eq!(orders[0].p, "74690");
    assert_eq!(orders[0].c.as_deref(), Some("0x16"));
    assert_eq!(orders[0].t, OrderKind::Limit(LimitTif::Alo));
    assert_eq!(orders[1].t, OrderKind::Limit(LimitTif::FrontendMarket));
    assert!(orders[1].r);
    assert_eq!(orders[1].c, None);
    assert_eq!(orders[2].t, OrderKind::Trigger);
}

#[test]
fn parses_cancels_modifies_and_leaves_other_actions_opaque() {
    let c = action::parse(r#"{"type":"cancel","cancels":[{"a":5,"o":599442161288}],"f":true}"#)
        .unwrap();
    let Action::Cancel { cancels } = c else {
        panic!()
    };
    assert_eq!((cancels[0].a, cancels[0].o), (5, 599_442_161_288));

    let c = action::parse(
        r#"{"type":"cancelByCloid","cancels":[{"asset":5,"cloid":"0x0052"}],"f":true}"#,
    )
    .unwrap();
    let Action::CancelByCloid { cancels } = c else {
        panic!()
    };
    assert_eq!((cancels[0].asset, cancels[0].cloid.as_str()), (5, "0x0052"));

    let m = action::parse(r#"{"type":"modify","oid":599441888910,"order":{"a":3,"b":false,"p":"136.36","s":"3.91","r":false,"t":{"limit":{"tif":"Alo"}}}}"#).unwrap();
    let Action::Modify(m) = m else { panic!() };
    assert_eq!(m.oid, OrderRef::Oid(599_441_888_910));
    assert_eq!(m.order.p, "136.36");

    let b = action::parse(r#"{"type":"batchModify","modifies":[{"oid":"0x1a1c","order":{"a":3,"b":true,"p":"1","s":"2","r":false,"t":{"limit":{"tif":"Gtc"}},"c":"0x1a1c"}}]}"#).unwrap();
    let Action::BatchModify { modifies } = b else {
        panic!()
    };
    assert_eq!(modifies[0].oid, OrderRef::Cloid("0x1a1c".to_owned()));
    assert_eq!(modifies[0].order.t, OrderKind::Limit(LimitTif::Gtc));

    assert_eq!(
        action::parse(r#"{"type":"noop"}"#).unwrap(),
        Action::Other("noop".to_owned())
    );
    assert_eq!(
        action::parse(r#"{"type":"order","orders":[{"a":0,"b":true,"p":"1","s":"1","r":false,"t":{"limit":{"tif":"Xyz"}}}],"grouping":"na"}"#).unwrap(),
        Action::Order { orders: vec![action::OrderWire { a: 0, b: true, p: "1".into(), s: "1".into(), r: false, t: OrderKind::Limit(LimitTif::Unknown("Xyz".into())), c: None }] }
    );
    assert!(action::parse(r#"{"orders":[]}"#).is_err());
}

// ---- end-to-end conversion ----

const BLOCKS: &str = concat!(
    r#"{"height": 100, "blockTime": 1000, "hash": "0xb100", "proposer": "0xp", "numTxs": 3, "txs": ["#,
    r#"{"time": 1000, "user": "0xa", "action": {"type": "order", "orders": [{"a": 0, "b": false, "p": "74690", "s": "0.15247", "r": false, "t": {"limit": {"tif": "Alo"}}, "c": "0x16"}], "grouping": {"p": 163}}, "block": 100, "hash": "0xt1", "error": null}, "#,
    r#"{"time": 1000, "user": "0xb", "action": {"type": "order", "orders": [{"a": 0, "b": true, "p": "74690.5", "s": "0.1", "r": false, "t": {"limit": {"tif": "Gtc"}}}, {"a": 110089, "b": true, "p": "1", "s": "1", "r": false, "t": {"limit": {"tif": "Gtc"}}}], "grouping": "na"}, "block": 100, "hash": "0xt2", "error": null}, "#,
    r#"{"time": 1000, "user": "0xc", "action": {"type": "cancel", "cancels": [{"a": 0, "o": 7}]}, "block": 100, "hash": "0xt3", "error": "Action already expired"}"#,
    "]}\n",
    r#"{"height": 101, "blockTime": 1070, "hash": "0xb101", "proposer": "0xp", "numTxs": 2, "txs": ["#,
    r#"{"time": 1070, "user": "0xa", "action": {"type": "cancelByCloid", "cancels": [{"asset": 0, "cloid": "0x16"}]}, "block": 101, "hash": "0xt4", "error": null}, "#,
    r#"{"time": 1070, "user": "0xa", "action": {"type": "batchModify", "modifies": [{"oid": 5, "order": {"a": 1, "b": true, "p": "2000", "s": "1", "r": false, "t": {"trigger": {"isMarket": true, "triggerPx": "1900", "tpsl": "sl"}}}}]}, "block": 101, "hash": "0xt5", "error": null}"#,
    "]}\n",
);

const L2: &str = concat!(
    r#"{"height":101,"coin":"BTC","block_time_ms":1070,"src":"rest","levels":[[{"px":"74668.0","sz":"0.0208","n":2}],[{"px":"74690.0","sz":"0.15247","n":1}]]}"#,
    "\n",
    r#"{"height":100,"coin":"ETH","block_time_ms":1000,"src":"ws","levels":[[],[]]}"#,
    "\n",
    r#"{"height":101,"coin":"ETH","block_time_ms":1070,"src":"ws","levels":[[{"px":"2000.123","sz":"1","n":1}],[]]}"#,
    "\n",
    r#"{"height":999,"coin":"BTC","block_time_ms":9,"src":"ws","levels":[[],[]]}"#,
    "\n",
    r#"{"height":100,"coin":"NOPE","block_time_ms":1000,"src":"ws","levels":[[],[]]}"#,
    "\n",
);

fn run() -> (Vec<u8>, hl_dataset::convert::ConvertStats) {
    let mut out = Vec::new();
    let stats = convert(
        &assets_from_meta(META).unwrap(),
        "test",
        Cursor::new(BLOCKS),
        L2,
        &mut out,
    )
    .unwrap();
    (out, stats)
}

#[test]
fn conversion_counts_everything_the_report_needs() {
    let (_, s) = run();
    assert_eq!(
        (s.first_height, s.last_height, s.blocks, s.txs),
        (100, 101, 2, 5)
    );
    assert_eq!(s.errored_txs, 1);
    assert_eq!(s.actions.get("order"), Some(&2));
    assert_eq!(s.actions.get("cancel"), Some(&1));
    assert_eq!(s.actions.get("cancelByCloid"), Some(&1));
    assert_eq!(s.actions.get("batchModify"), Some(&1));
    assert_eq!(s.order_legs.get("Alo"), Some(&1));
    assert_eq!(s.order_legs.get("Gtc"), Some(&2));
    assert_eq!(s.modify_legs.get("trigger"), Some(&1));
    assert_eq!(s.cancel_by_oid_legs, 1);
    assert_eq!(s.cancel_by_cloid_legs, 1);
    assert_eq!(s.modify_by_oid_legs, 1);
    assert_eq!(s.modify_by_cloid_legs, 0);
    // One leg is on a builder perp the meta does not cover; its price is not checked.
    assert_eq!(s.legs_unknown_asset.get("builder_perp"), Some(&1));
    // "74690.5" on BTC breaks the 5-significant-figure rule.
    assert_eq!(s.px_violations, 1);
    assert_eq!(s.sz_violations, 0);
    assert_eq!(s.violation_examples.len(), 1);
    assert_eq!(
        s.violations_by_reason
            .get("perp px: more than 5 significant figures in a non-integer price"),
        Some(&1)
    );
    assert_eq!(s.parse_failures, 0);
    // L2: 3 in range and known; ETH@101 has a 3-decimal price on a 2-decimal grid.
    assert_eq!(s.l2_records, 2);
    assert_eq!(s.l2_level_violations, 1);
    assert_eq!(s.l2_out_of_range, 1);
    assert_eq!(s.l2_unknown_coin, 1);
    assert_eq!(s.l2_time_mismatches, 0);
}

#[test]
fn the_converted_file_reads_back_with_raw_actions_byte_for_byte() {
    let (out, _) = run();
    let reader = Reader::new(Cursor::new(&out)).unwrap();
    assert_eq!(reader.header().first_height, 100);
    assert_eq!(reader.header().assets.len(), 4);
    let steps: Vec<_> = reader.steps().collect::<Result<_, _>>().unwrap();
    assert_eq!(steps.len(), 2);
    let b = &steps[0].block;
    assert_eq!(b.time_ms, Some(1000));
    assert_eq!(b.hash.as_deref(), Some("0xb100"));
    assert_eq!(b.txs.len(), 3);
    assert_eq!(b.ops.len(), 0);
    assert_eq!(b.txs[2].error.as_deref(), Some("Action already expired"));
    assert_eq!(b.txs[0].time_ms, Some(1000));
    assert_eq!(
        b.txs[0].action.get(),
        r#"{"type": "order", "orders": [{"a": 0, "b": false, "p": "74690", "s": "0.15247", "r": false, "t": {"limit": {"tif": "Alo"}}, "c": "0x16"}], "grouping": {"p": 163}}"#
    );
    // ETH@100 follows block 100; BTC@101 follows block 101. ETH@101 is
    // dropped (invalid level), as are the out-of-range and unknown-coin ones.
    assert_eq!(steps[0].books.len(), 1);
    assert_eq!(steps[1].books.len(), 1);
    let BookState::L2(btc) = &steps[1].books[0] else {
        panic!()
    };
    assert_eq!(
        (btc.asset, btc.time_ms, btc.depth),
        (0, Some(1070), Some(20))
    );
    assert_eq!(btc.bids[0].px_ticks, 746_680);
    assert_eq!(btc.bids[0].sz_lots, 2080);
    assert_eq!(btc.bids[0].n, 2);
    assert_eq!(btc.asks[0].px.as_deref(), Some("74690.0"));
}
