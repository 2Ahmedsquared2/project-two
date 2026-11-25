//! Signer existence ("User or API Wallet 0x… does not exist."), from our node's
//! data, blocks 1232430001..1232440000, every noop recovered (85,748):
//! all 2,092 such errors are noops whose recovered signer is the address in
//! native's message and appears nowhere in the 1232430000 state; all 83,656
//! accepted noops recover to a seeded signer (17,079 of 17,079 in the first
//! 2,000 blocks, 66,577 of 66,577 in the 8,000 after, rule frozen). No accepted
//! noop needed an agent approved inside the window. Addresses below are real.
#![allow(clippy::unwrap_used)]

use hl_account::Signers;

const DNE_AT_1232430002: &str = "0x8ad2a2866a578a2bb4fa23ab858da351a497986e";
const USER_WITH_POSITIONS: &str = "0x000000416889eabb68a5fb740cc74c8c743f204a";

fn seeded() -> Signers {
    Signers::from_lines(["# header", USER_WITH_POSITIONS].into_iter()).unwrap()
}

#[test]
fn unknown_signer_does_not_exist() {
    assert!(!seeded().exists(DNE_AT_1232430002));
}

#[test]
fn seeded_user_exists_any_case() {
    let s = seeded();
    assert!(s.exists(USER_WITH_POSITIONS));
    assert!(s.exists(&USER_WITH_POSITIONS.to_uppercase().replace("0X", "0x")));
}

#[test]
fn approved_agent_exists_from_then_on() {
    let mut s = seeded();
    assert!(!s.exists(DNE_AT_1232430002));
    s.add(DNE_AT_1232430002).unwrap();
    assert!(s.exists(DNE_AT_1232430002));
}

#[test]
fn malformed_lines_are_errors() {
    assert!(Signers::from_lines(["0x1234"].into_iter()).is_err());
}
