//! # provider-transport
//!
//! JSON-RPC 2.0 transports for the `provider-connect` sidecar.
//!
//! Primary transport: **newline-delimited JSON-RPC 2.0 over stdio**
//! (ACP-style, [`stdio::serve_stdio`]). Optional feature-gated transports:
//! `ws` (tokio-tungstenite server) and `http` (minimal hyper server).
//!
//! ## Method surface (requests)
//!
//! | method         | params                        | result                         |
//! |----------------|-------------------------------|--------------------------------|
//! | `initialize`   | `{}`                          | capabilities object            |
//! | `capabilities` | `{}`                          | capabilities object            |
//! | `listen`       | `{ "providers"?: [id] }`      | `{ "started": [id] }`          |
//! | `send`         | `{ "provider": id, "message": SendMessage }` | `SendReceipt`  |
//! | `shutdown`     | `{}`                          | `null`                         |
//!
//! ## Notifications (server → client)
//!
//! `event.message` ([`ChannelMessage`](provider_core::ChannelMessage)),
//! `event.draft` ([`events::DraftEvent`]), `event.choice`
//! ([`events::ChoiceEvent`]), `event.error` ([`events::ErrorEvent`]).
//!
//! ## Errors
//!
//! Standard JSON-RPC 2.0 codes (`-32700` parse, `-32600` invalid request,
//! `-32601` method not found, `-32602` invalid params, `-32603` internal) plus
//! `-32000..-32005` mapping [`ProviderError`](provider_core::ProviderError)
//! variants (see [`state::provider_error`]).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod events;
pub mod jsonrpc;
pub mod persist;
pub mod state;
pub mod stdio;

#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "ws")]
pub mod ws;

pub use error::TransportError;
pub use state::{AppState, DispatchOutcome, Outbound};
