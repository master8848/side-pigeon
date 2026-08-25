//! # provider-core
//!
//! Unified schema, capability traits, error types and the feature-gated
//! provider registry for `provider-connect`.
//!
//! This crate implements the public API surface of `docs/api-contract.md` v0.1
//! verbatim: the message schema (`ChannelMessage`, `Sender`, `ContentPart`,
//! `MediaAttachment`, `MediaKind`, `SendMessage`, `SendReceipt`), the
//! capability traits (`ChatProvider`, `ProviderEvents`), and the typed
//! [`ProviderError`]. Every schema type derives `serde::Serialize` /
//! `serde::Deserialize` so the same structs are the JSON-RPC wire types.
//!
//! The [`ProviderRegistry`] (feature `registry`, on by default) is the
//! compile-time feature-gated home for providers: provider crates depend only
//! on this crate + `std` (see `docs/architecture.md` §4), and hosts register
//! them behind cargo features so unused providers are pruned at compile time.
//!
//! ## Notes for implementors
//!
//! [`ChatProvider`] uses [`async_trait`] (rather than native `async fn`) so
//! providers can be held as `Box<dyn ChatProvider>` in the registry. The
//! method signatures match `docs/api-contract.md` exactly; implementors apply
//! `#[async_trait]` to their `impl ChatProvider for X` blocks.
//!
//! Providers receive an `Arc<dyn ProviderEvents>` at construction time (from
//! the host/transport) and call `events.on_message(..)` for every inbound
//! message — exactly the ZeroClaw pattern ported to a library.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod schema;
pub mod traits;

#[cfg(feature = "registry")]
pub mod registry;

pub use error::ProviderError;
pub use schema::{
    ChannelMessage, ContentPart, MediaAttachment, MediaKind, SendMessage, SendReceipt, Sender,
};
pub use traits::{ChatProvider, ProviderEvents, TRANSIENT_ERROR_EVENT_THRESHOLD};

#[cfg(feature = "registry")]
pub use registry::ProviderRegistry;
