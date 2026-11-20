//! The network this crate may target: testnet, and only testnet.
//!
//! Mainnet is deliberately not representable: there is no mainnet variant, no
//! mainnet URL and no way to produce the mainnet phantom-agent source `"a"`.
//! Ahmed has approved testnet only; adding mainnet needs his explicit approval.

/// Testnet REST base URL (`hyperliquid/utils/constants.py::TESTNET_API_URL`).
pub const TESTNET_API_URL: &str = "https://api.hyperliquid-testnet.xyz";

/// Path of the signed-action endpoint.
pub const EXCHANGE_PATH: &str = "/exchange";

/// The network an action is signed for and posted to.
///
/// Only testnet exists:
///
/// ```compile_fail
/// let _ = hl_order::Network::Mainnet;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Network {
    /// Hyperliquid testnet: phantom-agent source `"b"`.
    Testnet,
}

impl Network {
    /// The phantom agent's `source` (`construct_phantom_agent`: `"a"` on mainnet,
    /// `"b"` otherwise).
    #[must_use]
    pub const fn source(self) -> &'static str {
        match self {
            Self::Testnet => "b",
        }
    }

    /// REST base URL.
    #[must_use]
    pub const fn base_url(self) -> &'static str {
        match self {
            Self::Testnet => TESTNET_API_URL,
        }
    }

    /// Full URL of the `/exchange` endpoint.
    #[must_use]
    pub const fn exchange_url(self) -> &'static str {
        match self {
            Self::Testnet => "https://api.hyperliquid-testnet.xyz/exchange",
        }
    }
}
