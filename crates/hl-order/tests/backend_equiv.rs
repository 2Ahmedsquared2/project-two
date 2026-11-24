//! End to end through the public API: for random keys and random orders,
//! `Signer::sign_l1` and the signer's address are byte-identical to what k256
//! (the backend before the swap, kept as a test oracle) gives for the same
//! EIP-712 digest.
#![allow(clippy::expect_used)]

use hl_order::{connection_id, Action, Cloid, Decimal8, LimitOrder, Network, Signer, Tif};
use k256::ecdsa::SigningKey;
use proptest::prelude::*;
use sha3::{Digest, Keccak256};

fn action() -> impl Strategy<Value = Action> {
    let tif = prop_oneof![Just(Tif::Alo), Just(Tif::Gtc), Just(Tif::Ioc)];
    let order = (
        0u32..10_000,
        any::<bool>(),
        1u64..1_000_000_000_000_000,
        1u64..1_000_000_000_000_000,
        any::<bool>(),
        tif,
        proptest::option::of(any::<[u8; 16]>()),
    )
        .prop_map(|(asset, is_buy, px, sz, reduce_only, tif, cloid)| {
            Action::Order(LimitOrder {
                asset,
                is_buy,
                px: Decimal8::from_units(px),
                sz: Decimal8::from_units(sz),
                reduce_only,
                tif,
                cloid: cloid.map(Cloid),
            })
        });
    prop_oneof![
        4 => order,
        1 => (0u32..10_000, any::<u64>()).prop_map(|(asset, oid)| Action::Cancel { asset, oid }),
        1 => proptest::option::of(any::<u64>()).prop_map(|time| Action::ScheduleCancel { time }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn sign_l1_is_byte_identical_to_k256(
        key in any::<[u8; 32]>().prop_filter("valid key", |k| SigningKey::from_slice(k).is_ok()),
        action in action(),
        nonce in any::<u64>(),
        vault in proptest::option::of(any::<[u8; 20]>()),
        expires_after in proptest::option::of(any::<u64>()),
    ) {
        let signer = Signer::from_bytes(&key).expect("key");
        let got = signer
            .sign_l1(Network::Testnet, &action, nonce, vault.as_ref(), expires_after)
            .expect("sign");

        let cid = connection_id(&action, nonce, vault.as_ref(), expires_after).expect("hash");
        prop_assert_eq!(got.connection_id, cid);
        let digest = signer.agent_digest(Network::Testnet, &cid);
        let oracle = SigningKey::from_slice(&key).expect("oracle key");
        let (sig, recid) = oracle.sign_prehash_recoverable(&digest).expect("oracle sign");
        let (r, s) = sig.split_bytes();
        prop_assert_eq!(&got.signature.r[..], &r[..]);
        prop_assert_eq!(&got.signature.s[..], &s[..]);
        prop_assert_eq!(got.signature.v, 27 + recid.to_byte());

        let point = oracle.verifying_key().to_encoded_point(false);
        let hash = Keccak256::digest(&point.as_bytes()[1..]);
        prop_assert_eq!(&signer.address()[..], &hash[12..]);
    }
}
