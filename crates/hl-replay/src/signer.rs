//! The prototype signer: the right amount of hashing and one ECDSA signature,
//! in the shape of an L1 order action. Not checked against the SDK vectors.

use std::fmt::Write as _;

use secp256k1::{Message, PublicKey, Secp256k1, SecretKey, SignOnly};
use sha3::{Digest, Keccak256};

use crate::stage::{OrderIntent, Signer, Stage, StageLabel};
use crate::ReplayError;

#[derive(serde::Serialize)]
struct Limit<'a> {
    tif: &'a str,
}
#[derive(serde::Serialize)]
struct OrderType<'a> {
    limit: Limit<'a>,
}
#[derive(serde::Serialize)]
struct OrderWire<'a> {
    a: u32,
    b: bool,
    p: &'a str,
    s: &'a str,
    r: bool,
    t: OrderType<'a>,
}
#[derive(serde::Serialize)]
struct Action<'a> {
    r#type: &'a str,
    orders: [OrderWire<'a>; 1],
    grouping: &'a str,
}

fn keccak(parts: &[&[u8]]) -> [u8; 32] {
    let mut k = Keccak256::new();
    for p in parts {
        k.update(p);
    }
    k.finalize().into()
}

/// A stand-in signer with the cost profile of the real one: msgpack-encode
/// the action, three keccak-256 passes (connection id, struct hash, EIP-712
/// digest) and one recoverable secp256k1 signature from a reused libsecp256k1
/// context, then a JSON body built in the caller's buffer.
///
/// **Prototype.** The domain separator is a placeholder and nothing here has
/// been compared with the SDK's signing vectors, so the bytes it produces are
/// not a valid order and its timings are indicative only.
#[derive(Debug)]
pub struct PrototypeSigner {
    ctx: Secp256k1<SignOnly>,
    key: SecretKey,
    domain: [u8; 32],
    agent_typehash: [u8; 32],
    source_hash: [u8; 32],
    packed: Vec<u8>,
    px: String,
    sz: String,
}

impl PrototypeSigner {
    /// A signer for `secret`. Pass throwaway keys only.
    ///
    /// # Errors
    ///
    /// When `secret` is not a valid secp256k1 secret key.
    pub fn new(secret: &[u8; 32]) -> Result<Self, ReplayError> {
        let key = SecretKey::from_slice(secret).map_err(|e| ReplayError::Signer(e.to_string()))?;
        Ok(Self {
            ctx: Secp256k1::signing_only(),
            key,
            domain: keccak(&[b"hl-replay prototype domain separator (placeholder)"]),
            agent_typehash: keccak(&[b"Agent(string source,bytes32 connectionId)"]),
            source_hash: keccak(&[b"a"]),
            packed: Vec::with_capacity(256),
            px: String::with_capacity(32),
            sz: String::with_capacity(32),
        })
    }

    /// A signer with a key made up on the spot: a hash of the process id and
    /// the current time. It exists for one run, is never stored or printed,
    /// and controls nothing.
    #[must_use]
    pub fn throwaway() -> Self {
        let pid = std::process::id().to_le_bytes();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
            .to_le_bytes();
        let mut round = 0_u8;
        loop {
            let secret = keccak(&[
                b"hl-replay throwaway key, never funded",
                &pid,
                &now,
                &[round],
            ]);
            // A hash is an invalid key with probability about 2^-128.
            if let Ok(s) = Self::new(&secret) {
                return s;
            }
            round = round.wrapping_add(1);
        }
    }

    /// The 32-byte digest that gets signed for `intent`.
    pub fn digest(&mut self, intent: &OrderIntent) -> [u8; 32] {
        self.format_px_sz(intent);
        self.digest_formatted(intent)
    }

    /// The compressed public key of the signing key.
    #[must_use]
    pub fn public_key(&self) -> [u8; 33] {
        PublicKey::from_secret_key(&self.ctx, &self.key).serialize()
    }

    /// Ticks of 0.1 and lots of 0.00001 as wire strings without trailing
    /// zeros, in the signer's reused buffers.
    fn format_px_sz(&mut self, intent: &OrderIntent) {
        self.px.clear();
        self.sz.clear();
        // Writing to a `String` cannot fail.
        let _ = write!(self.px, "{}.{}", intent.px_ticks / 10, intent.px_ticks % 10);
        let _ = write!(
            self.sz,
            "{}.{:05}",
            intent.sz_lots / 100_000,
            intent.sz_lots % 100_000
        );
        for s in [&mut self.px, &mut self.sz] {
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
    }

    fn digest_formatted(&mut self, intent: &OrderIntent) -> [u8; 32] {
        let action = Action {
            r#type: "order",
            orders: [OrderWire {
                a: intent.asset,
                b: intent.is_buy,
                p: &self.px,
                s: &self.sz,
                r: false,
                t: OrderType {
                    limit: Limit { tif: "Alo" },
                },
            }],
            grouping: "na",
        };
        self.packed.clear();
        // Encoding these plain structs into a `Vec` cannot fail.
        let _ = rmp_serde::encode::write_named(&mut self.packed, &action);
        self.packed.extend_from_slice(&intent.nonce.to_be_bytes());
        self.packed.push(0); // no vault address
        let connection_id = keccak(&[&self.packed]);
        let struct_hash = keccak(&[&self.agent_typehash, &self.source_hash, &connection_id]);
        keccak(&[&[0x19, 0x01], &self.domain, &struct_hash])
    }
}

impl Stage for PrototypeSigner {
    const NAME: &'static str =
        "PrototypeSigner: rmp-serde + 3x keccak256 + libsecp256k1 (reused context) + JSON body; not checked against SDK vectors";
    const LABEL: StageLabel = StageLabel::Prototype;
}

fn push_u64(out: &mut Vec<u8>, mut n: u64) {
    let mut digits = [0_u8; 20];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + u8::try_from(n % 10).unwrap_or(0);
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[i..]);
}

fn push_hex(out: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(HEX[usize::from(b >> 4)]);
        out.push(HEX[usize::from(b & 15)]);
    }
}

impl Signer for PrototypeSigner {
    #[inline]
    fn sign(&mut self, intent: &OrderIntent, out: &mut Vec<u8>) -> Result<(), ReplayError> {
        self.format_px_sz(intent);
        let digest = self.digest_formatted(intent);
        let sig = self
            .ctx
            .sign_ecdsa_recoverable(&Message::from_digest(digest), &self.key);
        let (recovery, compact) = sig.serialize_compact();
        out.extend_from_slice(br#"{"action":{"type":"order","orders":[{"a":"#);
        push_u64(out, u64::from(intent.asset));
        out.extend_from_slice(br#","b":"#);
        out.extend_from_slice(if intent.is_buy { b"true" } else { b"false" });
        out.extend_from_slice(br#","p":""#);
        out.extend_from_slice(self.px.as_bytes());
        out.extend_from_slice(br#"","s":""#);
        out.extend_from_slice(self.sz.as_bytes());
        out.extend_from_slice(
            br#"","r":false,"t":{"limit":{"tif":"Alo"}}}],"grouping":"na"},"nonce":"#,
        );
        push_u64(out, intent.nonce);
        out.extend_from_slice(br#","signature":{"r":"0x"#);
        push_hex(out, &compact[..32]);
        out.extend_from_slice(br#"","s":"0x"#);
        push_hex(out, &compact[32..]);
        out.extend_from_slice(br#"","v":"#);
        push_u64(out, if recovery.to_i32() == 0 { 27 } else { 28 });
        out.extend_from_slice(b"}}\n");
        Ok(())
    }
}
