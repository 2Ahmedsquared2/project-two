//! Signed-request builder and a transport-agnostic client with RTT stamps.
//!
//! This PR ships no real transport: tests use a fake. The later RTT probe plugs in
//! an HTTP transport; [`RttStamps`] are taken immediately before the transport is
//! handed the body and immediately after it returns the full response.

use std::time::{Duration, Instant};

use crate::action::{write_address, Action};
use crate::hex::write_minimal_hex;
use crate::network::Network;
use crate::signing::{SignError, Signature, Signer};

/// Append the `POST /exchange` JSON body, byte-identical to the SDK's
/// `Exchange._post_action` payload serialised compactly:
/// `{"action":…,"nonce":…,"signature":{"r","s","v"},"vaultAddress":…,"expiresAfter":…}`.
pub fn request_body(
    out: &mut String,
    action: &Action,
    nonce: u64,
    sig: &Signature,
    vault: Option<&[u8; 20]>,
    expires_after: Option<u64>,
) {
    use std::fmt::Write as _;
    out.push_str(r#"{"action":"#);
    action.write_json(out);
    // Writing to a String cannot fail.
    let _ = write!(out, r#","nonce":{nonce},"signature":{{"r":""#);
    write_minimal_hex(out, &sig.r);
    out.push_str(r#"","s":""#);
    write_minimal_hex(out, &sig.s);
    let _ = write!(out, r#"","v":{}}},"vaultAddress":"#, sig.v);
    match vault {
        None => out.push_str("null"),
        Some(a) => {
            out.push('"');
            write_address(out, a);
            out.push('"');
        }
    }
    out.push_str(r#","expiresAfter":"#);
    match expires_after {
        None => out.push_str("null"),
        Some(t) => {
            let _ = write!(out, "{t}");
        }
    }
    out.push('}');
}

/// A transport failure, with a human-readable cause.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("transport: {0}")]
pub struct TransportError(pub String);

/// Sends a request body to a URL and returns the full response body.
pub trait Transport {
    /// POST `body` (JSON) to `url`.
    fn post(&mut self, url: &str, body: &[u8]) -> Result<Vec<u8>, TransportError>;
}

/// Monotonic stamps around one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RttStamps {
    /// Taken just before the body is handed to the transport.
    pub before_write: Instant,
    /// Taken just after the transport returned the full response.
    pub after_read: Instant,
}

impl RttStamps {
    /// `after_read - before_write`.
    #[must_use]
    pub fn rtt(&self) -> Duration {
        self.after_read.saturating_duration_since(self.before_write)
    }
}

/// The raw response and its timing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeResponse {
    /// Response body bytes, unparsed.
    pub body: Vec<u8>,
    /// Stamps around the transport call.
    pub timing: RttStamps,
}

/// A client failure.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Signing failed.
    #[error(transparent)]
    Sign(#[from] SignError),
    /// The transport failed.
    #[error(transparent)]
    Transport(#[from] TransportError),
}

/// Signs actions and posts them through a [`Transport`] to the network's
/// `/exchange` endpoint. Reuses one body buffer across requests.
#[derive(Debug)]
pub struct ExchangeClient<T> {
    transport: T,
    signer: Signer,
    network: Network,
    vault: Option<[u8; 20]>,
    expires_after: Option<u64>,
    body: String,
}

impl<T: Transport> ExchangeClient<T> {
    /// A client with no vault and no `expiresAfter`.
    pub fn new(transport: T, signer: Signer, network: Network) -> Self {
        Self {
            transport,
            signer,
            network,
            vault: None,
            expires_after: None,
            body: String::with_capacity(512),
        }
    }

    /// Act for a vault or subaccount.
    #[must_use]
    pub fn with_vault(mut self, vault: [u8; 20]) -> Self {
        self.vault = Some(vault);
        self
    }

    /// Set `expiresAfter` (UTC ms) on every action.
    #[must_use]
    pub fn with_expires_after(mut self, expires_after: u64) -> Self {
        self.expires_after = Some(expires_after);
        self
    }

    /// The transport (tests inspect what it was sent).
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Sign `action` with `nonce` (UTC ms, unique per signer) and post it.
    pub fn send(&mut self, action: &Action, nonce: u64) -> Result<ExchangeResponse, ClientError> {
        let vault = self.vault.as_ref();
        let signed = self
            .signer
            .sign_l1(self.network, action, nonce, vault, self.expires_after)?;
        self.body.clear();
        request_body(
            &mut self.body,
            action,
            nonce,
            &signed.signature,
            vault,
            self.expires_after,
        );
        let before_write = Instant::now();
        let result = self
            .transport
            .post(self.network.exchange_url(), self.body.as_bytes());
        let after_read = Instant::now();
        Ok(ExchangeResponse {
            body: result?,
            timing: RttStamps {
                before_write,
                after_read,
            },
        })
    }
}
