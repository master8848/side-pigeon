//! Typed error taxonomy for providers (see `docs/api-contract.md`).

use thiserror::Error;

/// A provider error with a machine-readable variant taxonomy.
///
/// Implemented with [`thiserror`]; the `Display` messages are part of the
/// contract and must not change.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// Bad or missing configuration.
    #[error("configuration error: {0}")]
    Config(String),
    /// Authentication / authorization failed.
    #[error("auth error: {0}")]
    Auth(String),
    /// Network-level failure (DNS, connect, timeout, ...).
    #[error("network error: {0}")]
    Network(String),
    /// Rate limited by the platform.
    #[error("rate limited: {0}")]
    RateLimit(String),
    /// The platform violated or rejected our protocol exchange.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// Anything that does not fit the taxonomy.
    #[error("other: {0}")]
    Other(String),
}

impl ProviderError {
    /// Stable, wire-friendly variant name (used in JSON-RPC error `data.kind`).
    pub fn kind(&self) -> &'static str {
        match self {
            ProviderError::Config(_) => "Config",
            ProviderError::Auth(_) => "Auth",
            ProviderError::Network(_) => "Network",
            ProviderError::RateLimit(_) => "RateLimit",
            ProviderError::Protocol(_) => "Protocol",
            ProviderError::Other(_) => "Other",
        }
    }
}

impl From<&str> for ProviderError {
    fn from(s: &str) -> Self {
        ProviderError::Other(s.to_string())
    }
}

impl From<String> for ProviderError {
    fn from(s: String) -> Self {
        ProviderError::Other(s)
    }
}
