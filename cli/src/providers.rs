//! Feature-gated provider construction for pc-connect (embed mode).
//!
//! NOTE: this module mirrors `bin/pc/src/main.rs::build_provider` from the
//! provider-connect sidecar so pc-connect embeds the identical provider
//! wiring in-process (single binary, no sidecar spawn). The sidecar remains
//! the single source of truth; keep the two in sync.
//!
//! Real providers are compile-time feature-gated (`telegram`, `discord`),
//! exactly like `bin/pc` — unused providers are pruned at compile time.

use std::sync::Arc;

use provider_core::{ChatProvider, ProviderEvents};

/// Feature-gated provider construction. Each branch depends only on
/// `provider-core` types plus the provider crate's own builder.
///
/// The full merged config blob (`PC_*_CONFIG` / JSON file) is applied — not
/// just `token` — so `base_url`, `poll_interval`, `intents` and timeouts
/// actually reach the providers.
// TODO(P2): extract to provider-config::factory - see docs/POLISH.md P2
pub fn build_provider(
    id: &str,
    config: &serde_json::Value,
    events: Arc<dyn ProviderEvents>,
) -> Result<Box<dyn ChatProvider>, String> {
    let kind = provider_core::alias::provider_kind(id, config);
    let alias_static = provider_core::alias::leak_alias(id);
    let events = provider_core::alias::AliasEvents::wrap(alias_static, events);
    match kind {
        #[cfg(feature = "demo")]
        "demo" => Ok(Box::new(provider_core::alias::AliasedProvider::new(
            alias_static,
            Box::new(crate::demo::DemoProvider::new(events, config)),
        ))),
        #[cfg(feature = "telegram")]
        "telegram" => {
            let p = build_telegram(config, events)?;
            Ok(Box::new(provider_core::alias::AliasedProvider::new(
                alias_static,
                Box::new(p),
            )))
        }
        #[cfg(feature = "discord")]
        "discord" => {
            let p = build_discord(config, events)?;
            Ok(Box::new(provider_core::alias::AliasedProvider::new(
                alias_static,
                Box::new(p),
            )))
        }
        other => Err(format!(
            "unknown provider kind '{other}' for alias '{id}' (compiled in: {})",
            available_providers().join(", ")
        )),
    }
}

/// Concrete `TelegramProvider` builder (kept concrete so `check` can reach
/// `take_last_error`). Same config keys as the sidecar.
#[cfg(feature = "telegram")]
pub fn build_telegram(
    config: &serde_json::Value,
    events: Arc<dyn ProviderEvents>,
) -> Result<provider_telegram::TelegramProvider, String> {
    let token = config_token("telegram", config)?;
    let mut provider = provider_telegram::TelegramProvider::new(token, events);
    if let Some(base) = config_str_alt("telegram", config, &["base_url", "baseUrl"])? {
        provider = provider.with_base_url(base);
    }
    if let Some(secs) = config_u64("telegram", config, "poll_interval_secs")? {
        provider = provider.with_poll_interval(std::time::Duration::from_secs(secs));
    }
    if let Some(secs) = config_u64("telegram", config, "long_poll_timeout_secs")? {
        provider = provider.with_long_poll_timeout_secs(secs);
    }
    if let Some(secs) = config_u64("telegram", config, "request_timeout_secs")? {
        provider = provider.with_request_timeout(std::time::Duration::from_secs(secs));
    }
    Ok(provider)
}

/// Concrete `DiscordProvider` builder (kept concrete so `check` can reach
/// `take_last_error`). Same config keys as the sidecar.
#[cfg(feature = "discord")]
pub fn build_discord(
    config: &serde_json::Value,
    events: Arc<dyn ProviderEvents>,
) -> Result<provider_discord::DiscordProvider, String> {
    let token = config_token("discord", config)?;
    let mut provider = provider_discord::DiscordProvider::new(token, events);
    if let Some(url) = config_str_alt("discord", config, &["gateway_url", "gatewayUrl"])? {
        provider = provider.with_gateway_url(url);
    }
    if let Some(base) = config_str_alt("discord", config, &["rest_base", "restBase"])? {
        provider = provider.with_rest_base(base);
    }
    if let Some(intents) = config_u64("discord", config, "intents")? {
        provider = provider.with_intents(intents);
    }
    if let Some(secs) = config_u64("discord", config, "request_timeout_secs")? {
        provider = provider.with_request_timeout(std::time::Duration::from_secs(secs));
    }
    Ok(provider)
}

#[cfg(any(feature = "telegram", feature = "discord"))]
fn config_token(id: &str, config: &serde_json::Value) -> Result<String, String> {
    config
        .get("token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("provider '{id}' requires config.token"))
}

#[cfg(any(feature = "telegram", feature = "discord"))]
#[allow(dead_code)]
fn config_str(id: &str, config: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
    match config.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| format!("provider '{id}' config.{key} must be a string")),
    }
}

#[cfg(any(feature = "telegram", feature = "discord"))]
fn config_str_alt(
    id: &str,
    config: &serde_json::Value,
    keys: &[&str],
) -> Result<Option<String>, String> {
    for key in keys {
        match config.get(*key) {
            None => continue,
            Some(v) => {
                return v
                    .as_str()
                    .map(|s| Some(s.to_string()))
                    .ok_or_else(|| format!("provider '{id}' config.{key} must be a string"));
            }
        }
    }
    Ok(None)
}

#[cfg(any(feature = "telegram", feature = "discord"))]
fn config_u64(id: &str, config: &serde_json::Value, key: &str) -> Result<Option<u64>, String> {
    match config.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("provider '{id}' config.{key} must be a non-negative integer")),
    }
}

/// Provider ids compiled into this binary.
pub fn available_providers() -> Vec<&'static str> {
    let mut ids = Vec::new();
    #[cfg(feature = "demo")]
    ids.extend(["demo"]);
    #[cfg(feature = "telegram")]
    ids.extend(["telegram"]);
    #[cfg(feature = "discord")]
    ids.extend(["discord"]);
    ids
}
