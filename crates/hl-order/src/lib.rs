//! RED skeleton: API only, bodies land in the GREEN commit.
#![allow(missing_docs, clippy::missing_panics_doc, unused_variables, clippy::needless_pass_by_value)]

use std::time::{Duration, Instant};

pub const TESTNET_API_URL: &str = "https://api.hyperliquid-testnet.xyz";
pub const EXCHANGE_PATH: &str = "/exchange";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Testnet,
}

impl Network {
    #[must_use]
    pub fn source(self) -> &'static str {
        todo!()
    }
    #[must_use]
    pub fn base_url(self) -> &'static str {
        todo!()
    }
    #[must_use]
    pub fn exchange_url(self) -> &'static str {
        todo!()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Decimal8(u64);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseDecimalError {
    #[error("bad decimal")]
    Bad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PxRuleError {
    #[error("szDecimals {0} out of range")]
    SzDecimals(u8),
    #[error("overflow")]
    Overflow,
}

impl Decimal8 {
    #[must_use]
    pub const fn from_units(units: u64) -> Self {
        Self(units)
    }
    #[must_use]
    pub const fn units(self) -> u64 {
        self.0
    }
    pub fn parse(s: &str) -> Result<Self, ParseDecimalError> {
        todo!()
    }
    #[must_use]
    pub fn to_wire(self) -> String {
        todo!()
    }
    pub fn round_px_perp(self, sz_decimals: u8) -> Result<Self, PxRuleError> {
        todo!()
    }
    #[must_use]
    pub fn is_valid_px_perp(self, sz_decimals: u8) -> bool {
        todo!()
    }
    pub fn floor_to_lot(self, sz_decimals: u8) -> Result<Self, PxRuleError> {
        todo!()
    }
    #[must_use]
    pub fn is_valid_sz(self, sz_decimals: u8) -> bool {
        todo!()
    }
}

impl std::fmt::Display for Decimal8 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tif {
    Alo,
    Gtc,
    Ioc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cloid(pub [u8; 16]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitOrder {
    pub asset: u32,
    pub is_buy: bool,
    pub px: Decimal8,
    pub sz: Decimal8,
    pub reduce_only: bool,
    pub tif: Tif,
    pub cloid: Option<Cloid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Order(LimitOrder),
    Cancel { asset: u32, oid: u64 },
    ScheduleCancel { time: Option<u64> },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("todo")]
    Todo,
}

impl Action {
    pub fn write_msgpack<W: std::io::Write>(&self, w: &mut W) -> Result<(), Error> {
        todo!()
    }
}

pub fn connection_id(
    action: &Action,
    nonce: u64,
    vault: Option<&[u8; 20]>,
    expires_after: Option<u64>,
) -> Result<[u8; 32], Error> {
    todo!()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signature {
    pub r: [u8; 32],
    pub s: [u8; 32],
    pub v: u8,
}

impl Signature {
    #[must_use]
    pub fn r_hex(&self) -> String {
        todo!()
    }
    #[must_use]
    pub fn s_hex(&self) -> String {
        todo!()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedAction {
    pub connection_id: [u8; 32],
    pub signature: Signature,
}

pub struct Signer;

impl Signer {
    pub fn from_bytes(key: &[u8; 32]) -> Result<Self, Error> {
        todo!()
    }
    #[must_use]
    pub fn address_hex(&self) -> String {
        todo!()
    }
    pub fn sign_l1(
        &self,
        network: Network,
        action: &Action,
        nonce: u64,
        vault: Option<&[u8; 20]>,
        expires_after: Option<u64>,
    ) -> Result<SignedAction, Error> {
        todo!()
    }
}

pub fn request_body(
    out: &mut String,
    action: &Action,
    nonce: u64,
    sig: &Signature,
    vault: Option<&[u8; 20]>,
    expires_after: Option<u64>,
) {
    todo!()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("transport: {0}")]
pub struct TransportError(pub String);

pub trait Transport {
    fn post(&mut self, url: &str, body: &[u8]) -> Result<Vec<u8>, TransportError>;
}

#[derive(Debug, Clone, Copy)]
pub struct RttStamps {
    pub before_write: Instant,
    pub after_read: Instant,
}

impl RttStamps {
    #[must_use]
    pub fn rtt(&self) -> Duration {
        todo!()
    }
}

#[derive(Debug)]
pub struct ExchangeResponse {
    pub body: Vec<u8>,
    pub timing: RttStamps,
}

pub struct ExchangeClient<T> {
    transport: T,
}

impl<T: Transport> ExchangeClient<T> {
    pub fn new(transport: T, signer: Signer, network: Network) -> Self {
        todo!()
    }
    #[must_use]
    pub fn with_vault(self, vault: [u8; 20]) -> Self {
        todo!()
    }
    #[must_use]
    pub fn with_expires_after(self, expires_after: u64) -> Self {
        todo!()
    }
    pub fn transport(&self) -> &T {
        &self.transport
    }
    pub fn send(&mut self, action: &Action, nonce: u64) -> Result<ExchangeResponse, Error> {
        todo!()
    }
}
