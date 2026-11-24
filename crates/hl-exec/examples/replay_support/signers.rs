//! Signer recovery and the known-signer set for the lane 1 replay (lane 2's
//! unknown-wallet rule, PR #45).
//!
//! Native answers `User or API Wallet 0x… does not exist.` and does nothing
//! else when an L1 action's recovered signer is neither a known user nor a
//! registered agent. The replay recovers the signer from the action itself
//! (never from native's response, whose `user` is null on these errors):
//!
//! `connectionId = keccak(msgpack(action) || nonce u64 BE || 0x00 | 0x01 vault
//! || [0x00 || expiresAfter u64 BE])`, the EIP-712 phantom agent
//! `{source, connectionId}` in domain `Exchange/1/1337/0x0`, then ecrecover.
//! This is the SDK's `recover_agent_or_user_from_l1_action` (0.24.0).
//!
//! `msgpack(action)` must keep the action's key order as written, which a
//! `serde_json::Value` (no `preserve_order`) loses, so the action is
//! transcoded straight from the JSON text by a streaming visitor that writes
//! msgpack the way Python's `msgpack.packb` does (smallest int encoding,
//! str8 allowed, float64).

use std::collections::HashSet;
use std::fmt;
use std::fmt::Write as _;
use std::io::BufRead;

use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use secp256k1::{Message, Secp256k1};
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use sha3::{Digest, Keccak256};

/// Mainnet phantom-agent source.
pub(crate) const MAINNET: &str = "a";
/// Testnet phantom-agent source (the hl-order golden vectors).
#[allow(dead_code)]
pub(crate) const TESTNET: &str = "b";

// ---------------------------------------------------------------- msgpack

fn put_len(out: &mut Vec<u8>, n: usize, fix: u8, fix_max: usize, m16: u8, m32: u8) {
    if n <= fix_max {
        out.push(fix | n as u8);
    } else if n <= 0xffff {
        out.push(m16);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(m32);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let n = s.len();
    if n <= 31 {
        out.push(0xa0 | n as u8);
    } else if n <= 0xff {
        out.push(0xd9);
        out.push(n as u8);
    } else if n <= 0xffff {
        out.push(0xda);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(0xdb);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    }
    out.extend_from_slice(s.as_bytes());
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    if v <= 0x7f {
        out.push(v as u8);
    } else if v <= 0xff {
        out.extend_from_slice(&[0xcc, v as u8]);
    } else if v <= 0xffff {
        out.push(0xcd);
        out.extend_from_slice(&(v as u16).to_be_bytes());
    } else if v <= 0xffff_ffff {
        out.push(0xce);
        out.extend_from_slice(&(v as u32).to_be_bytes());
    } else {
        out.push(0xcf);
        out.extend_from_slice(&v.to_be_bytes());
    }
}

fn put_i64(out: &mut Vec<u8>, v: i64) {
    if v >= 0 {
        put_u64(out, v as u64);
    } else if v >= -32 {
        out.push(v as u8);
    } else if v >= i64::from(i8::MIN) {
        out.extend_from_slice(&[0xd0, v as u8]);
    } else if v >= i64::from(i16::MIN) {
        out.push(0xd1);
        out.extend_from_slice(&(v as i16).to_be_bytes());
    } else if v >= i64::from(i32::MIN) {
        out.push(0xd2);
        out.extend_from_slice(&(v as i32).to_be_bytes());
    } else {
        out.push(0xd3);
        out.extend_from_slice(&v.to_be_bytes());
    }
}

/// Streams one JSON value into msgpack, keeping map key order.
struct Enc<'o>(&'o mut Vec<u8>);

impl<'de> DeserializeSeed<'de> for Enc<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Enc<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        self.0.push(0xc0);
        Ok(())
    }
    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        self.visit_unit()
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<(), E> {
        self.0.push(if v { 0xc3 } else { 0xc2 });
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<(), E> {
        put_u64(self.0, v);
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<(), E> {
        put_i64(self.0, v);
        Ok(())
    }
    #[allow(clippy::disallowed_types)] // msgpack float64, as the SDK packs a float
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<(), E> {
        self.0.push(0xcb);
        self.0.extend_from_slice(&v.to_bits().to_be_bytes());
        Ok(())
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<(), E> {
        put_str(self.0, v);
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<(), A::Error> {
        let mut body = Vec::new();
        let mut n = 0usize;
        while s.next_element_seed(Enc(&mut body))?.is_some() {
            n += 1;
        }
        put_len(self.0, n, 0x90, 15, 0xdc, 0xdd);
        self.0.extend_from_slice(&body);
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<(), A::Error> {
        let mut body = Vec::new();
        let mut n = 0usize;
        while let Some(k) = m.next_key::<std::borrow::Cow<'de, str>>()? {
            put_str(&mut body, &k);
            m.next_value_seed(Enc(&mut body))?;
            n += 1;
        }
        put_len(self.0, n, 0x80, 15, 0xde, 0xdf);
        self.0.extend_from_slice(&body);
        Ok(())
    }
}

fn msgpack_of<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let mut out = Vec::new();
    Enc(&mut out).deserialize(d)?;
    Ok(out)
}

// ---------------------------------------------------------------- the line

#[derive(Deserialize)]
struct Sig {
    r: String,
    s: String,
    v: u64,
}

/// What recovery needs from one signed action, action already msgpacked.
#[derive(Deserialize)]
pub(crate) struct SignedL1 {
    #[serde(deserialize_with = "msgpack_of")]
    action: Vec<u8>,
    #[serde(default)]
    nonce: Option<u64>,
    #[serde(default, rename = "vaultAddress")]
    vault: Option<String>,
    #[serde(default, rename = "expiresAfter")]
    expires_after: Option<u64>,
    #[serde(default)]
    signature: Option<Sig>,
}

#[derive(Deserialize)]
struct Bundle {
    signed_actions: Vec<SignedL1>,
}

#[derive(Deserialize)]
struct Abci {
    signed_action_bundles: Vec<(IgnoredAny, Bundle)>,
}

#[derive(Deserialize)]
struct Line {
    abci_block: Abci,
}

/// Every signed action of one `replica_cmds` line, in block order.
pub(crate) fn signed_actions(line: &str) -> Vec<SignedL1> {
    let l: Line = serde_json::from_str(line).expect("replica line: signed actions");
    l.abci_block
        .signed_action_bundles
        .into_iter()
        .flat_map(|(_, b)| b.signed_actions)
        .collect()
}

/// Parse a JSON body (`{"action":…,"nonce":…,"signature":…}`) as one action.
#[allow(dead_code)]
pub(crate) fn signed_action_body(body: &str) -> SignedL1 {
    serde_json::from_str(body).expect("signed action body")
}

// ---------------------------------------------------------------- recovery

fn keccak(b: &[u8]) -> [u8; 32] {
    Keccak256::digest(b).into()
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    let h = s.strip_prefix("0x").unwrap_or(s);
    if h.len() > 64 || h.is_empty() {
        return None;
    }
    let p = format!("{h:0>64}");
    let mut o = [0u8; 32];
    for (i, b) in o.iter_mut().enumerate() {
        *b = u8::from_str_radix(&p[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(o)
}

fn hex20(s: &str) -> Option<[u8; 20]> {
    let h = s.strip_prefix("0x").unwrap_or(s);
    if h.len() != 40 {
        return None;
    }
    let mut o = [0u8; 20];
    for (i, b) in o.iter_mut().enumerate() {
        *b = u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(o)
}

/// `0x` + 40 lowercase hex digits.
pub(crate) fn addr_hex(a: &[u8; 20]) -> String {
    let mut s = String::with_capacity(42);
    s.push_str("0x");
    for b in a {
        let _ = write!(s, "{b:02x}");
    }
    s
}

impl SignedL1 {
    /// The msgpack bytes of the action (key order as written).
    #[allow(dead_code)]
    pub(crate) fn action_msgpack(&self) -> &[u8] {
        &self.action
    }

    /// The action hash (`connectionId`), as the SDK's `action_hash`.
    pub(crate) fn connection_id(&self) -> Option<[u8; 32]> {
        let mut h = Keccak256::new();
        h.update(&self.action);
        h.update(self.nonce?.to_be_bytes());
        match self.vault.as_deref() {
            None => h.update([0x00]),
            Some(v) => {
                h.update([0x01]);
                h.update(hex20(v)?);
            }
        }
        if let Some(t) = self.expires_after {
            h.update([0x00]);
            h.update(t.to_be_bytes());
        }
        Some(h.finalize().into())
    }

    /// The L1 signer (agent or user), or `None` when the action carries no
    /// usable L1 signature.
    pub(crate) fn recover(
        &self,
        ctx: &Secp256k1<secp256k1::VerifyOnly>,
        source: &str,
    ) -> Option<[u8; 20]> {
        let sig = self.signature.as_ref()?;
        let cid = self.connection_id()?;
        let domain = {
            let mut h = Keccak256::new();
            h.update(keccak(
                b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
            ));
            h.update(keccak(b"Exchange"));
            h.update(keccak(b"1"));
            let mut chain = [0u8; 32];
            chain[24..].copy_from_slice(&1337u64.to_be_bytes());
            h.update(chain);
            h.update([0u8; 32]);
            let d: [u8; 32] = h.finalize().into();
            d
        };
        let mut s = Keccak256::new();
        s.update(keccak(b"Agent(string source,bytes32 connectionId)"));
        s.update(keccak(source.as_bytes()));
        s.update(cid);
        let st: [u8; 32] = s.finalize().into();
        let mut d = Keccak256::new();
        d.update([0x19, 0x01]);
        d.update(domain);
        d.update(st);
        let digest: [u8; 32] = d.finalize().into();
        let rec = match sig.v {
            27 | 0 => 0,
            28 | 1 => 1,
            _ => return None,
        };
        let mut rs = [0u8; 64];
        rs[..32].copy_from_slice(&hex32(&sig.r)?);
        rs[32..].copy_from_slice(&hex32(&sig.s)?);
        let rsig = RecoverableSignature::from_compact(&rs, RecoveryId::from_i32(rec).ok()?).ok()?;
        let pk = ctx
            .recover_ecdsa(&Message::from_digest(digest), &rsig)
            .ok()?;
        let h = keccak(&pk.serialize_uncompressed()[1..]);
        let mut a = [0u8; 20];
        a.copy_from_slice(&h[12..]);
        Some(a)
    }
}

// ---------------------------------------------------------------- the set

/// Known users and agents (lane 2's `signers_<height>.txt` format: one
/// lowercase `0x` address per line, `#` comments). Same contract as
/// `hl_account::Signers` in PR #45; switch to it once that crate is on main.
#[derive(Default)]
pub(crate) struct Signers(HashSet<[u8; 20]>);

impl Signers {
    pub(crate) fn from_reader<R: BufRead>(r: R) -> Self {
        let mut set = HashSet::new();
        for l in r.lines() {
            let l = l.expect("signers line");
            let t = l.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            set.insert(hex20(&t.to_ascii_lowercase()).expect("signers: bad address"));
        }
        Self(set)
    }
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
    pub(crate) fn exists(&self, a: &[u8; 20]) -> bool {
        self.0.contains(a)
    }
    pub(crate) fn add_hex(&mut self, s: &str) -> bool {
        match hex20(&s.to_ascii_lowercase()) {
            Some(a) => self.0.insert(a),
            None => false,
        }
    }
}
