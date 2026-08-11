//! Sidecar configuration: JSON file or environment variables.

use std::env;
use std::fs;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

/// Top-level sidecar config. Loaded from `--config <path>`, `$PC_CONFIG`, or
/// environment variables (`PC_PROVIDERS` + `PC_<UPPER_ID>_TOKEN`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SidecarConfig {
    /// Providers to load, e.g. `[{"id":"telegram","config":{"token":"..."}}]`.
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
}

/// One provider entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider id: `demo` (built-in), `telegram`, `discord` (feature-gated).
    pub id: String,
    /// Provider-specific config (e.g. `{"token": "..."}`).
    #[serde(default)]
    pub config: Value,
}

/// Configuration load errors.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Could not read the config file.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Config file / env JSON is invalid.
    #[error("invalid config: {0}")]
    Json(#[from] serde_json::Error),
}

/// Load configuration: CLI path → `$PC_CONFIG` → environment variables.
pub fn load(cli_path: Option<String>) -> Result<SidecarConfig, ConfigError> {
    if let Some(path) = cli_path {
        return from_file(&path);
    }
    if let Ok(path) = env::var("PC_CONFIG") {
        if !path.trim().is_empty() {
            return from_file(&path);
        }
    }
    from_env()
}

fn from_file(path: &str) -> Result<SidecarConfig, ConfigError> {
    let text = fs::read_to_string(path)?;
    let config: SidecarConfig = serde_json::from_str(&text)?;
    Ok(config)
}

/// Environment fallback:
///   PC_PROVIDERS=demo,telegram            (comma-separated provider ids)
///   PC_TELEGRAM_TOKEN=123:abc             (token per provider)
///   PC_TELEGRAM_CONFIG={"base_url":"..."} (optional extra JSON, merged)
fn from_env() -> Result<SidecarConfig, ConfigError> {
    let mut providers = Vec::new();
    let Some(list) = env::var("PC_PROVIDERS").ok() else {
        return Ok(SidecarConfig { providers });
    };
    for id in list.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let upper = id.to_uppercase();
        let mut config = json!({});
        if let Ok(token) = env::var(format!("PC_{upper}_TOKEN")) {
            config["token"] = Value::String(token);
        }
        if let Ok(extra) = env::var(format!("PC_{upper}_CONFIG")) {
            if !extra.trim().is_empty() {
                let extra: Value = serde_json::from_str(&extra)?;
                merge_into(&mut config, extra);
            }
        }
        providers.push(ProviderConfig {
            id: id.to_string(),
            config,
        });
    }
    Ok(SidecarConfig { providers })
}

/// Merge `extra` (an object) into `base`; `extra` wins per key.
fn merge_into(base: &mut Value, extra: Value) {
    match (base, extra) {
        (Value::Object(base), Value::Object(extra)) => {
            for (key, value) in extra {
                base.insert(key, value);
            }
        }
        (base, extra) => *base = extra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_file_shape() {
        let config: SidecarConfig = serde_json::from_str(
            r#"{"providers":[{"id":"telegram","config":{"token":"abc"}},{"id":"demo"}]}"#,
        )
        .unwrap();
        assert_eq!(config.providers.len(), 2);
        assert_eq!(config.providers[0].config["token"], "abc");
        assert_eq!(config.providers[1].id, "demo");
    }

    #[test]
    fn empty_config_defaults() {
        let config: SidecarConfig = serde_json::from_str(r#"{}"#).unwrap();
        assert!(config.providers.is_empty());
    }
}
