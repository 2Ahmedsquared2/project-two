//! The ECDSA backend: recoverable secp256k1 signatures over a 32-byte digest.
//!
//! Private to the crate, so the backend can change without touching the public
//! API. RED stub: the libsecp256k1 backend is not written yet. `Signer` still
//! signs with `k256` at this commit; the tests below pin what the new backend
//! must do (byte-identical output to `k256`) and fail until it exists.
#![cfg_attr(not(test), allow(dead_code))]

use crate::signing::SignError;

/// A secp256k1 secret key.
pub(crate) struct SecretKey;

impl SecretKey {
    /// Build from 32 big-endian bytes; zero and values not below the curve
    /// order are refused.
    pub(crate) fn from_bytes(_key: &[u8; 32]) -> Result<Self, SignError> {
        Ok(Self)
    }

    /// The uncompressed SEC1 public key: `0x04 || x || y`.
    pub(crate) fn public_key_uncompressed(&self) -> [u8; 65] {
        [0u8; 65]
    }

    /// Sign a 32-byte digest: `r || s || v`, RFC 6979 nonce, low-s, `v = 27 +
    /// recovery id`.
    pub(crate) fn sign_recoverable(&self, _digest: &[u8; 32]) -> Result<[u8; 65], SignError> {
        Err(SignError::Ecdsa(
            "libsecp256k1 backend not implemented".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use k256::ecdsa::{RecoveryId, Signature as KSignature, SigningKey, VerifyingKey};
    use proptest::prelude::*;

    use super::SecretKey;

    /// The secp256k1 group order n, big-endian.
    const ORDER: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36,
        0x41, 0x41,
    ];
    /// n / 2 (rounded down): `s` must not exceed it (low-s, EIP-2).
    const HALF_ORDER: [u8; 32] = [
        0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b,
        0x20, 0xa0,
    ];

    fn order_minus(k: u8) -> [u8; 32] {
        let mut b = ORDER;
        b[31] -= k; // 0x41 - k, no borrow for k <= 0x41
        b
    }

    /// The oracle: what `hl-order` produced before the swap (k256 0.13,
    /// `sign_prehash_recoverable`), as `r || s || v`.
    fn k256_sign(key: &[u8; 32], digest: &[u8; 32]) -> [u8; 65] {
        let sk = SigningKey::from_slice(key).expect("oracle key");
        let (sig, recid) = sk.sign_prehash_recoverable(digest).expect("oracle sign");
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&sig.to_bytes());
        out[64] = 27 + recid.to_byte();
        out
    }

    fn k256_public(key: &[u8; 32]) -> [u8; 65] {
        let sk = SigningKey::from_slice(key).expect("oracle key");
        let point = sk.verifying_key().to_encoded_point(false);
        point.as_bytes().try_into().expect("65 bytes")
    }

    /// Random keys, plus keys at both ends of the valid range.
    fn valid_key() -> impl Strategy<Value = [u8; 32]> {
        prop_oneof![
            8 => any::<[u8; 32]>(),
            1 => (1u8..=255).prop_map(|k| {
                let mut b = [0u8; 32];
                b[31] = k;
                b
            }),
            1 => (1u8..=0x41).prop_map(order_minus),
        ]
        .prop_filter("valid secp256k1 secret key", |k| {
            SigningKey::from_slice(k).is_ok()
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        /// The gate for the backend swap: for random valid keys and random
        /// 32-byte digests the 65-byte `r || s || v` is byte-identical to k256's.
        #[test]
        fn signature_is_byte_identical_to_k256(key in valid_key(), digest in any::<[u8; 32]>()) {
            let got = SecretKey::from_bytes(&key).expect("key").sign_recoverable(&digest);
            let got = got.expect("sign");
            prop_assert_eq!(got, k256_sign(&key, &digest));
            // Low-s, v is 27 or 28, and signing is deterministic.
            prop_assert!(got[32..64] <= HALF_ORDER[..]);
            prop_assert!(got[64] == 27 || got[64] == 28);
            let again = SecretKey::from_bytes(&key).expect("key").sign_recoverable(&digest);
            prop_assert_eq!(again.expect("sign"), got);
        }

        #[test]
        fn public_key_is_byte_identical_to_k256(key in valid_key()) {
            let got = SecretKey::from_bytes(&key).expect("key").public_key_uncompressed();
            prop_assert_eq!(got, k256_public(&key));
        }
    }

    #[test]
    fn keys_outside_the_valid_range_are_refused() {
        for bad in [[0u8; 32], ORDER, [0xff; 32]] {
            assert!(SecretKey::from_bytes(&bad).is_err());
        }
        assert!(SecretKey::from_bytes(&order_minus(1)).is_ok());
    }

    /// Digests at the edges, including the ones at or above the group order
    /// (which a random test never draws): the signature is low-s and recovers
    /// the signer's public key with the returned recovery id.
    #[test]
    fn edge_digests_sign_low_s_and_recover() {
        let key = [0x42u8; 32];
        let sk = SecretKey::from_bytes(&key).expect("key");
        let want = VerifyingKey::from_sec1_bytes(&k256_public(&key)).expect("pk");
        let mut order_plus_1 = ORDER;
        order_plus_1[31] += 1;
        for digest in [[0u8; 32], order_minus(1), ORDER, order_plus_1, [0xff; 32]] {
            let got = sk.sign_recoverable(&digest).expect("sign");
            assert!(got[32..64] <= HALF_ORDER[..], "high s for {digest:02x?}");
            let sig = KSignature::from_slice(&got[..64]).expect("sig");
            let recid = RecoveryId::from_byte(got[64] - 27).expect("recid");
            let rec = VerifyingKey::recover_from_prehash(&digest, &sig, recid).expect("recover");
            assert_eq!(rec, want, "digest {digest:02x?}");
        }
    }
}
