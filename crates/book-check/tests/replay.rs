//! Brief #2 (C): replaying raw signed actions through the reference book.
//! Written before the implementation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use book_check::replay::{OidRule, ReplayConfig, Replayer};
use hl_book_ref::{Lots, Oid, Px, Side};
use hl_dataset::{AssetMeta, Block, RawJson, Tx};

/// Asset 0 "GOLD": `sz_decimals` 5, so 1 price decimal ("100.0" = 1000 ticks)
/// and sizes in 1e-5 lots ("1" = 100000 lots).
fn assets() -> Vec<AssetMeta> {
    vec![AssetMeta::new(0, "GOLD", 5, false).unwrap()]
}

fn tx(user: &str, error: Option<&str>, action: &str) -> Tx {
    Tx {
        time_ms: None,
        user: user.to_owned(),
        hash: "0x".to_owned(),
        error: error.map(str::to_owned),
        action: RawJson::from_string(action.to_owned()).unwrap(),
    }
}

fn order(a: u32, buy: bool, px: &str, sz: &str, tif: &str, cloid: Option<&str>) -> String {
    let c = cloid.map(|c| format!(r#","c":"{c}""#)).unwrap_or_default();
    format!(
        r#"{{"type":"order","orders":[{{"a":{a},"b":{buy},"p":"{px}","s":"{sz}","r":false,"t":{{"limit":{{"tif":"{tif}"}}}}{c}}}],"grouping":"na"}}"#
    )
}

fn block(height: u64, txs: Vec<Tx>) -> Block {
    Block {
        height,
        time_ms: None,
        hash: None,
        proposer: None,
        txs,
        ops: vec![],
    }
}

fn queue(r: &Replayer, side: Side, px: u64) -> Vec<(u64, u64)> {
    r.book(0)
        .unwrap()
        .level(side, Px(px))
        .map(|l| l.orders.iter().map(|(o, s)| (o.0, s.0)).collect())
        .unwrap_or_default()
}

#[test]
fn an_errored_tx_changes_nothing_and_consumes_no_oid() {
    let mut r = Replayer::new(&assets(), ReplayConfig::default());
    r.apply_block(&block(
        1,
        vec![
            tx(
                "0xa",
                Some("Action already expired"),
                &order(0, true, "99", "1", "Gtc", None),
            ),
            tx("0xa", None, &order(0, true, "98", "1", "Gtc", None)),
        ],
    ));
    assert_eq!(queue(&r, Side::Bid, 990), vec![]);
    assert_eq!(queue(&r, Side::Bid, 980), vec![(1, 100_000)]);
    assert_eq!(r.stats().errored_txs_skipped, 1);
}

#[test]
fn legs_place_in_array_order_with_one_synthetic_oid_per_leg() {
    let mut r = Replayer::new(&assets(), ReplayConfig::default());
    let two = r#"{"type":"order","orders":[{"a":0,"b":true,"p":"99","s":"1","r":false,"t":{"limit":{"tif":"Alo"}}},{"a":0,"b":true,"p":"99","s":"2","r":false,"t":{"limit":{"tif":"Alo"}}}],"grouping":"na"}"#;
    r.apply_block(&block(1, vec![tx("0xa", None, two)]));
    assert_eq!(queue(&r, Side::Bid, 990), vec![(1, 100_000), (2, 200_000)]);
    assert_eq!(r.config().oid_rule, OidRule::OnePerAcceptedLeg);
}

#[test]
fn trigger_orders_are_no_ops_and_tick_violations_are_rejected() {
    let mut r = Replayer::new(&assets(), ReplayConfig::default());
    let trig = r#"{"type":"order","orders":[{"a":0,"b":true,"p":"99","s":"1","r":false,"t":{"trigger":{"isMarket":true,"triggerPx":"99","tpsl":"sl"}}}],"grouping":"na"}"#;
    r.apply_block(&block(
        1,
        vec![
            tx("0xa", None, trig),
            tx("0xa", None, &order(0, true, "99.05", "1", "Gtc", None)),
            tx("0xa", None, &order(7, true, "99", "1", "Gtc", None)),
        ],
    ));
    assert_eq!(r.book(0).unwrap().order_count(), 0);
    let s = r.stats();
    assert_eq!(s.trigger_legs_skipped, 1);
    assert_eq!(s.legs_invalid_tick_lot, 1);
    assert_eq!(s.legs_unknown_asset, 1);
}

#[test]
fn cancel_by_cloid_resolves_by_user_and_cloid_and_cancel_by_oid_does_not() {
    let mut r = Replayer::new(&assets(), ReplayConfig::default());
    r.apply_block(&block(
        1,
        vec![
            tx("0xa", None, &order(0, true, "99", "1", "Alo", Some("0x01"))),
            tx("0xb", None, &order(0, true, "99", "1", "Alo", Some("0x01"))),
        ],
    ));
    r.apply_block(&block(
        2,
        vec![
            // Another user's cloid does not resolve to A's order.
            tx(
                "0xc",
                None,
                r#"{"type":"cancelByCloid","cancels":[{"asset":0,"cloid":"0x01"}]}"#,
            ),
            tx(
                "0xb",
                None,
                r#"{"type":"cancelByCloid","cancels":[{"asset":0,"cloid":"0x01"}]}"#,
            ),
            tx(
                "0xa",
                None,
                r#"{"type":"cancel","cancels":[{"a":0,"o":1}]}"#,
            ),
        ],
    ));
    // B's order (oid 2) cancelled; A's (oid 1) survives the oid cancel,
    // because public oids and synthetic oids are different number spaces.
    assert_eq!(queue(&r, Side::Bid, 990), vec![(1, 100_000)]);
    let s = r.stats();
    assert_eq!(s.cancel_by_cloid_resolved, 1);
    assert_eq!(s.cancel_by_cloid_unresolved, 1);
    assert_eq!(s.cancel_by_oid_unresolved, 1);
}

#[test]
fn self_trade_prevention_applies_per_user_and_can_be_turned_off() {
    for (stp, expected_bid) in [(true, vec![(3, 100_000)]), (false, vec![])] {
        let mut r = Replayer::new(
            &assets(),
            ReplayConfig {
                self_trade_prevention: stp,
                ..ReplayConfig::default()
            },
        );
        r.apply_block(&block(
            1,
            vec![
                tx("0xa", None, &order(0, false, "100", "1", "Alo", None)),
                tx("0xb", None, &order(0, false, "100", "1", "Alo", None)),
                tx("0xa", None, &order(0, true, "100", "2", "Gtc", None)),
            ],
        ));
        assert_eq!(queue(&r, Side::Bid, 1000), expected_bid, "stp {stp}");
        assert_eq!(r.stats().self_trade_cancels, u64::from(stp));
    }
}

#[test]
fn modify_by_cloid_cancels_and_places_a_new_order_at_the_back() {
    let mut r = Replayer::new(&assets(), ReplayConfig::default());
    r.apply_block(&block(
        1,
        vec![
            tx("0xa", None, &order(0, true, "99", "1", "Alo", Some("0x01"))),
            tx("0xb", None, &order(0, true, "99", "1", "Alo", None)),
        ],
    ));
    let m = r#"{"type":"batchModify","modifies":[{"oid":"0x01","order":{"a":0,"b":true,"p":"99","s":"0.5","r":false,"t":{"limit":{"tif":"Alo"}},"c":"0x02"}}]}"#;
    r.apply_block(&block(2, vec![tx("0xa", None, m)]));
    assert_eq!(queue(&r, Side::Bid, 990), vec![(2, 100_000), (3, 50_000)]);
    // The new cloid resolves to the new order.
    r.apply_block(&block(
        3,
        vec![tx(
            "0xa",
            None,
            r#"{"type":"cancelByCloid","cancels":[{"asset":0,"cloid":"0x02"}]}"#,
        )],
    ));
    assert_eq!(queue(&r, Side::Bid, 990), vec![(2, 100_000)]);
    let s = r.stats();
    assert_eq!(s.modify_old_resolved, 1);
    assert_eq!(s.cancel_by_cloid_resolved, 1);
    assert_eq!(r.book(0).unwrap().get(Oid(2)).unwrap().sz, Lots(100_000));
}

#[test]
fn an_unresolvable_modify_still_places_the_new_order() {
    let mut r = Replayer::new(&assets(), ReplayConfig::default());
    let m = r#"{"type":"modify","oid":599441888910,"order":{"a":0,"b":false,"p":"101","s":"1","r":false,"t":{"limit":{"tif":"Alo"}}}}"#;
    r.apply_block(&block(1, vec![tx("0xa", None, m)]));
    assert_eq!(queue(&r, Side::Ask, 1010), vec![(1, 100_000)]);
    assert_eq!(r.stats().modify_old_unresolved, 1);
}

#[test]
fn legs_rejected_inside_an_ok_action_take_no_oid() {
    // ACTION_ORDERING.md §4.5: a tx without an error can still hold legs that
    // break the tick/lot rule; each is rejected and the action continues.
    // Under OidRule::OnePerAcceptedLeg those legs consume no oid.
    let mut r = Replayer::new(&assets(), ReplayConfig::default());
    let legs = [
        ("99", "1"),        // valid: oid 1
        ("99.05", "1"),     // too many price decimals
        ("12345.6", "1"),   // 6 significant figures
        ("99", "0.000001"), // finer than the lot
        ("98", "1"),        // valid: oid 2
    ];
    let orders: Vec<String> = legs
        .iter()
        .map(|(p, s)| {
            format!(
                r#"{{"a":0,"b":true,"p":"{p}","s":"{s}","r":false,"t":{{"limit":{{"tif":"Gtc"}}}}}}"#
            )
        })
        .collect();
    let action = format!(
        r#"{{"type":"order","orders":[{}],"grouping":"na"}}"#,
        orders.join(",")
    );
    r.apply_block(&block(1, vec![tx("0xa", None, &action)]));
    r.apply_block(&block(
        2,
        vec![tx("0xa", None, &order(0, true, "97", "1", "Gtc", None))],
    ));
    assert_eq!(r.stats().legs_invalid_tick_lot, 3);
    assert_eq!(queue(&r, Side::Bid, 990), vec![(1, 100_000)]);
    assert_eq!(queue(&r, Side::Bid, 980), vec![(2, 100_000)]);
    assert_eq!(queue(&r, Side::Bid, 970), vec![(3, 100_000)]);
}
