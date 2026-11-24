//! The ECDSA backend: recoverable secp256k1 signatures over a 32-byte digest.
//!
//! Private to the crate, so the backend can change without touching the public
//! API. The backend is libsecp256k1 (Bitcoin Core's C library) through the
//! `secp256k1` crate: RFC 6979 deterministic nonces, low-s signatures, and the
//! recovery id of the nonce point. The tests below hold it byte-identical to
//! `k256`, the pure-Rust backend this crate used before, kept as a
//! dev-dependency oracle.
//!
//! One signing context is built per process, the first time a key is made, and
//! every signature reuses it: signing creates no context and does not allocate.

use std::sync::OnceLock;

use secp256k1::{Message, PublicKey, Secp256k1, SignOnly};

use crate::signing::SignError;

/// The process-wide signing context.
///
/// It is not re-randomised (libsecp256k1's optional side-channel blinding):
/// that needs a random seed, and this crate takes no randomness source.
fn context() -> &'static Secp256k1<SignOnly> {
    static CONTEXT: OnceLock<Secp256k1<SignOnly>> = OnceLock::new();
    CONTEXT.get_or_init(Secp256k1::signing_only)
}

/// A secp256k1 secret key, bound to the process-wide signing context.
///
/// The key bytes are overwritten when this is dropped.
pub(crate) struct SecretKey {
    key: secp256k1::SecretKey,
    ctx: &'static Secp256k1<SignOnly>,
}

impl Drop for SecretKey {
    fn drop(&mut self) {
        // A volatile overwrite plus a compiler fence (the technique `zeroize`
        // uses). `secp256k1::SecretKey` is `Copy` and has no erasing `Drop` of
        // its own, so this type never copies it out.
        self.key.non_secure_erase();
    }
}

impl SecretKey {
    /// Build from 32 big-endian bytes; zero and values not below the curve
    /// order are refused.
    pub(crate) fn from_bytes(key: &[u8; 32]) -> Result<Self, SignError> {
        let key = secp256k1::SecretKey::from_slice(key).map_err(|_| SignError::InvalidKey)?;
        Ok(Self {
            key,
            ctx: context(),
        })
    }

    /// The uncompressed SEC1 public key: `0x04 || x || y`.
    pub(crate) fn public_key_uncompressed(&self) -> [u8; 65] {
        PublicKey::from_secret_key(self.ctx, &self.key).serialize_uncompressed()
    }

    /// Sign a 32-byte digest: `r || s || v`, RFC 6979 nonce, low-s, `v = 27 +
    /// recovery id`.
    pub(crate) fn sign_recoverable(&self, digest: &[u8; 32]) -> Result<[u8; 65], SignError> {
        let sig = self
            .ctx
            .sign_ecdsa_recoverable(&Message::from_digest(*digest), &self.key);
        let (recid, rs) = sig.serialize_compact();
        // 0 or 1 in practice; 2 or 3 means r overflowed the group order
        // (about 2^-127), which is passed on as 29 or 30, as before the swap.
        let recid = u8::try_from(recid.to_i32())
            .map_err(|_| SignError::Ecdsa("recovery id out of range".to_owned()))?;
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&rs);
        out[64] = 27 + recid;
        Ok(out)
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

    fn hex65(s: &str) -> [u8; 65] {
        let mut out = [0u8; 65];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex");
        }
        out
    }

    /// Digests at the edges, including the ones at or above the group order
    /// (which a random test never draws, and a Keccak output reaches with
    /// probability about 2^-128).
    ///
    /// The expected `r || s || v` come from the Python SDK's signer (`eth_keys`
    /// 0.8.0, native backend, `PrivateKey(0x42 * 32).sign_msg_hash(digest)`),
    /// the same stack the golden vectors come from. Every one is low-s and
    /// recovers the signer's public key. Below the order k256 agrees; at or
    /// above it k256 0.13 gives a different (also valid) signature, so on those
    /// digests this backend matches the SDK where the old one did not.
    #[test]
    fn edge_digests_match_the_python_sdk_and_recover() {
        let key = [0x42u8; 32];
        let sk = SecretKey::from_bytes(&key).expect("key");
        let want_pk = VerifyingKey::from_sec1_bytes(&k256_public(&key)).expect("pk");
        let mut order_plus_1 = ORDER;
        order_plus_1[31] += 1;
        let zero_sig = "5e4b9ba2d1128916544960752c6461583235da20d5e1257e9755a4ac54c28349\
                        36f4b394b30727d7f87dafa22e035007889d5fed186e1b4cdc9a10236f2a291a1c";
        let cases: [([u8; 32], &str, bool); 5] = [
            ([0u8; 32], zero_sig, true),
            (
                order_minus(1),
                "9a33c563b9903390ad28a0157056c2f62f0724954acc5e6fc35146ab134d4d13\
                 22a704ba9b98c54dbc7453eb584ad110913d3cbf41a4a8ad622b5919d7b21b9f1c",
                true,
            ),
            // n is 0 mod n: the same signature as the zero digest.
            (ORDER, zero_sig, false),
            (
                order_plus_1,
                "e4c6695da1caaaf03f2ba3893f4487d48d9173009f9487f60b5108bf9aa0dfe4\
                 462d1836e0f8e8fed6aa78b06fc56d74bf2638d514f99db1da07393d54ae18ec1c",
                false,
            ),
            (
                [0xff; 32],
                "b125adb60c89874e14d857ff3b956d15e61d3b19b362967967561bf4d8d0e818\
                 21634aa9d1feba2a9591bb026fcf7f36b4b1c868daa6c3d99c0b469c19240b981b",
                false,
            ),
        ];
        for (digest, sdk, k256_agrees) in cases {
            let got = sk.sign_recoverable(&digest).expect("sign");
            assert_eq!(got, hex65(sdk), "digest {digest:02x?}");
            assert_eq!(
                got == k256_sign(&key, &digest),
                k256_agrees,
                "{digest:02x?}"
            );
            assert!(got[32..64] <= HALF_ORDER[..], "high s for {digest:02x?}");
            let sig = KSignature::from_slice(&got[..64]).expect("sig");
            let recid = RecoveryId::from_byte(got[64] - 27).expect("recid");
            let rec = VerifyingKey::recover_from_prehash(&digest, &sig, recid).expect("recover");
            assert_eq!(rec, want_pk, "digest {digest:02x?}");
        }
    }
}
