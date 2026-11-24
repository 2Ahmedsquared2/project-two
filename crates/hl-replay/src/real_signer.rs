//! The real signer: `hl-order`'s L1 action signer behind the harness's
//! [`Signer`] trait.

use hl_order::{Action, Decimal8, LimitOrder, Network, Tif};
use sha3::{Digest, Keccak256};

use crate::stage::{OrderIntent, Signer, Stage, StageLabel};
use crate::ReplayError;

/// `1e-8` units ([`Decimal8`]) per price tick of 0.1.
const UNITS_PER_TICK: u64 = 10_000_000;
/// `1e-8` units ([`Decimal8`]) per size lot of 0.00001.
const UNITS_PER_LOT: u64 = 1_000;

/// [`hl_order::Signer`] as a harness stage: msgpack streamed into
/// keccak-256 (`connectionId`), the EIP-712 phantom-agent digest, one
/// RFC 6979 secp256k1 signature (libsecp256k1) and the SDK's signed-request JSON
/// body, built in a reused buffer.
///
/// It signs for testnet only (`hl-order` has no mainnet variant) and sends
/// nothing: the body goes to the caller's buffer, which the harness writes to
/// its loopback sink.
#[derive(Debug)]
pub struct HlOrderSigner {
    signer: hl_order::Signer,
    body: String,
}

impl HlOrderSigner {
    /// A signer for `secret`. Pass throwaway or public test keys only.
    ///
    /// # Errors
    ///
    /// When `secret` is not a valid secp256k1 secret key.
    pub fn new(secret: &[u8; 32]) -> Result<Self, ReplayError> {
        let signer =
            hl_order::Signer::from_bytes(secret).map_err(|e| ReplayError::Signer(e.to_string()))?;
        Ok(Self {
            signer,
            body: String::with_capacity(512),
        })
    }

    /// A signer with a key made up on the spot: a hash of the process id and
    /// the current time. It exists for one run, is never stored or printed,
    /// is never read from the environment and controls nothing.
    #[must_use]
    pub fn throwaway() -> Self {
        let pid = std::process::id().to_le_bytes();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
            .to_le_bytes();
        let mut round = 0_u8;
        loop {
            let mut k = Keccak256::new();
            k.update(b"hl-replay throwaway key, never funded");
            k.update(pid);
            k.update(now);
            k.update([round]);
            let secret: [u8; 32] = k.finalize().into();
            // A hash is an invalid key with probability about 2^-128.
            if let Ok(s) = Self::new(&secret) {
                return s;
            }
            round = round.wrapping_add(1);
        }
    }

    /// The 20-byte address of the signing key.
    #[must_use]
    pub fn address(&self) -> [u8; 20] {
        self.signer.address()
    }

    /// The L1 action the harness signs for `intent`: one ALO limit order,
    /// not reduce-only, no client order id.
    ///
    /// # Errors
    ///
    /// When the price or size does not fit [`Decimal8`].
    pub fn action_for(intent: &OrderIntent) -> Result<Action, ReplayError> {
        let units = |n: u64, per: u64, what: &'static str| {
            n.checked_mul(per)
                .map(Decimal8::from_units)
                .ok_or_else(|| ReplayError::Signer(format!("{what} {n} does not fit 1e-8 units")))
        };
        Ok(Action::Order(LimitOrder {
            asset: intent.asset,
            is_buy: intent.is_buy,
            px: units(intent.px_ticks, UNITS_PER_TICK, "price ticks")?,
            sz: units(intent.sz_lots, UNITS_PER_LOT, "size lots")?,
            reduce_only: false,
            tif: Tif::Alo,
            cloid: None,
        }))
    }

    /// Signs `action` as the SDK's `sign_l1_action` does and appends the
    /// signed-request body plus a newline to `out`. This is the whole
    /// signing path; [`Signer::sign`] only maps the intent first.
    ///
    /// # Errors
    ///
    /// When `hl-order` refuses to encode or sign.
    pub fn sign_action(
        &mut self,
        action: &Action,
        nonce: u64,
        vault: Option<&[u8; 20]>,
        expires_after: Option<u64>,
        out: &mut Vec<u8>,
    ) -> Result<(), ReplayError> {
        let signed = self
            .signer
            .sign_l1(Network::Testnet, action, nonce, vault, expires_after)
            .map_err(|e| ReplayError::Signer(e.to_string()))?;
        self.body.clear();
        hl_order::request_body(
            &mut self.body,
            action,
            nonce,
            &signed.signature,
            vault,
            expires_after,
        );
        out.extend_from_slice(self.body.as_bytes());
        out.push(b'\n');
        Ok(())
    }
}

impl Stage for HlOrderSigner {
    const NAME: &'static str =
        "HlOrderSigner: hl_order::Signer::sign_l1 (msgpack -> keccak256 connectionId, EIP-712 agent digest, libsecp256k1 RFC 6979) + request_body; testnet source, matches the Python SDK golden vectors";
    /// Real because `tests/real_signer.rs` signs every `hl-order` golden
    /// vector through this type and compares the bytes with the SDK's.
    const LABEL: StageLabel = StageLabel::Real;
}

impl Signer for HlOrderSigner {
    #[inline]
    fn sign(&mut self, intent: &OrderIntent, out: &mut Vec<u8>) -> Result<(), ReplayError> {
        let action = Self::action_for(intent)?;
        self.sign_action(&action, intent.nonce, None, None, out)
    }
}
