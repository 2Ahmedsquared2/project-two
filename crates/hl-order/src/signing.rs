//! L1 action signing, as `hyperliquid/utils/signing.py::sign_l1_action` does it:
//!
//! 1. `connectionId = keccak256(msgpack(action) || nonce u64 BE || vault || expires)`
//!    where `vault` is `0x00` or `0x01 || address`, and `expires` is empty or
//!    `0x00 || expiresAfter u64 BE`.
//! 2. Phantom agent `{source, connectionId}`; `source` is `"b"` off mainnet.
//! 3. EIP-712 over type `Agent(string source,bytes32 connectionId)` in domain
//!    `{name "Exchange", version "1", chainId 1337, verifyingContract 0x0}`.
//! 4. secp256k1 ECDSA (RFC 6979, low-s), `v = 27 + recovery id`.
//!
//! The action is msgpack-encoded straight into the Keccak hasher, so signing does
//! not allocate.

use k256::ecdsa::SigningKey;
use sha3::{Digest, Keccak256};

use crate::action::{Action, EncodeError};
use crate::hex::{write_hex, write_minimal_hex};
use crate::network::Network;

/// The EIP-712 domain's chain id for L1 actions (both networks; the network is
/// told apart by the phantom agent's `source`).
pub const L1_CHAIN_ID: u64 = 1337;

/// Signing failed.
#[derive(Debug, thiserror::Error)]
pub enum SignError {
    /// The private key is zero or not below the curve order.
    #[error("invalid secp256k1 private key")]
    InvalidKey,
    /// Encoding the action failed.
    #[error(transparent)]
    Encode(#[from] EncodeError),
    /// The ECDSA backend refused to sign.
    #[error("ecdsa: {0}")]
    Ecdsa(String),
}

/// An Ethereum-style recoverable signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature {
    /// r, big-endian.
    pub r: [u8; 32],
    /// s, big-endian, low-s normalised.
    pub s: [u8; 32],
    /// 27 or 28.
    pub v: u8,
}

impl Signature {
    /// r as the SDK prints it (`0x`, no leading zero nibbles).
    #[must_use]
    pub fn r_hex(&self) -> String {
        let mut s = String::with_capacity(66);
        write_minimal_hex(&mut s, &self.r);
        s
    }

    /// s as the SDK prints it (`0x`, no leading zero nibbles).
    #[must_use]
    pub fn s_hex(&self) -> String {
        let mut s = String::with_capacity(66);
        write_minimal_hex(&mut s, &self.s);
        s
    }
}

/// The result of signing one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SignedAction {
    /// The action hash the phantom agent carries.
    pub connection_id: [u8; 32],
    /// The signature over the EIP-712 digest.
    pub signature: Signature,
}

/// Compute the action hash (`connectionId`), as the SDK's `action_hash`.
pub fn connection_id(
    action: &Action,
    nonce: u64,
    vault: Option<&[u8; 20]>,
    expires_after: Option<u64>,
) -> Result<[u8; 32], EncodeError> {
    let mut h = Keccak256::new();
    action.write_msgpack(&mut h)?;
    h.update(nonce.to_be_bytes());
    match vault {
        None => h.update([0x00]),
        Some(addr) => {
            h.update([0x01]);
            h.update(addr);
        }
    }
    if let Some(t) = expires_after {
        h.update([0x00]);
        h.update(t.to_be_bytes());
    }
    Ok(h.finalize().into())
}

fn keccak(data: &[u8]) -> [u8; 32] {
    Keccak256::digest(data).into()
}

/// EIP-712 domain separator for `{Exchange, 1, 1337, 0x0}`.
fn domain_separator() -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(keccak(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    ));
    h.update(keccak(b"Exchange"));
    h.update(keccak(b"1"));
    let mut chain = [0u8; 32];
    chain[24..].copy_from_slice(&L1_CHAIN_ID.to_be_bytes());
    h.update(chain);
    h.update([0u8; 32]); // verifyingContract 0x000…0, left-padded
    h.finalize().into()
}

/// A secp256k1 signing key with the EIP-712 constants precomputed.
pub struct Signer {
    key: SigningKey,
    address: [u8; 20],
    domain: [u8; 32],
    agent_typehash: [u8; 32],
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material.
        f.debug_struct("Signer")
            .field("address", &self.address_hex())
            .finish_non_exhaustive()
    }
}

impl Signer {
    /// Build from a 32-byte big-endian private key.
    pub fn from_bytes(key: &[u8; 32]) -> Result<Self, SignError> {
        let key = SigningKey::from_slice(key).map_err(|_| SignError::InvalidKey)?;
        let point = key.verifying_key().to_encoded_point(false);
        let hash = keccak(&point.as_bytes()[1..]);
        let mut address = [0u8; 20];
        address.copy_from_slice(&hash[12..]);
        Ok(Self {
            key,
            address,
            domain: domain_separator(),
            agent_typehash: keccak(b"Agent(string source,bytes32 connectionId)"),
        })
    }

    /// The signer's 20-byte address.
    #[must_use]
    pub fn address(&self) -> [u8; 20] {
        self.address
    }

    /// The address as `0x` + 40 lowercase hex digits.
    #[must_use]
    pub fn address_hex(&self) -> String {
        let mut s = String::with_capacity(42);
        s.push_str("0x");
        write_hex(&mut s, &self.address);
        s
    }

    /// The EIP-712 digest that gets signed for this connection id.
    #[must_use]
    pub fn agent_digest(&self, network: Network, connection_id: &[u8; 32]) -> [u8; 32] {
        let mut s = Keccak256::new();
        s.update(self.agent_typehash);
        s.update(keccak(network.source().as_bytes()));
        s.update(connection_id);
        let struct_hash: [u8; 32] = s.finalize().into();
        let mut d = Keccak256::new();
        d.update([0x19, 0x01]);
        d.update(self.domain);
        d.update(struct_hash);
        d.finalize().into()
    }

    /// Sign an L1 action exactly as the SDK's `sign_l1_action`.
    pub fn sign_l1(
        &self,
        network: Network,
        action: &Action,
        nonce: u64,
        vault: Option<&[u8; 20]>,
        expires_after: Option<u64>,
    ) -> Result<SignedAction, SignError> {
        let cid = connection_id(action, nonce, vault, expires_after)?;
        let digest = self.agent_digest(network, &cid);
        let (sig, recid) = self
            .key
            .sign_prehash_recoverable(&digest)
            .map_err(|e| SignError::Ecdsa(e.to_string()))?;
        let (r, s) = sig.split_bytes();
        Ok(SignedAction {
            connection_id: cid,
            signature: Signature {
                r: r.into(),
                s: s.into(),
                v: 27 + recid.to_byte(),
            },
        })
    }
}
