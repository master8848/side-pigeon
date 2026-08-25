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
                if !extra.is_object() {
                    return Err(ConfigError::Json(<serde_json::Error as serde::de::Error>::custom(
                        format!("PC_{upper}_CONFIG must be a JSON object, got {extra}"),
                    )));
                }
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
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

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

    #[test]
    fn env_merging_token_and_extra_config() {
        let _guard = env_lock().lock().unwrap();
        // Save and restore env to avoid polluting other tests/process
        let vars = ["PC_PROVIDERS", "PC_TELEGRAM_TOKEN", "PC_TELEGRAM_CONFIG", "PC_CONFIG", "PC_DEMO_CONFIG", "PC_DEMO_TOKEN"];
        let saved: Vec<(String, Option<String>)> = vars.iter().map(|k| (k.to_string(), env::var(k).ok())).collect();

        // Clean slate
        for k in &vars {
            env::remove_var(k);
        }

        env::set_var("PC_PROVIDERS", "demo,telegram");
        env::set_var("PC_TELEGRAM_TOKEN", "123:abc");
        env::set_var("PC_TELEGRAM_CONFIG", r#"{"base_url":"https://example.test","poll_interval_secs":7}"#);
        env::remove_var("PC_CONFIG");

        let cfg = load(None).expect("load from env");
        assert_eq!(cfg.providers.len(), 2);
        assert_eq!(cfg.providers[0].id, "demo");
        assert_eq!(cfg.providers[1].id, "telegram");
        assert_eq!(cfg.providers[1].config["token"], "123:abc");
        assert_eq!(cfg.providers[1].config["base_url"], "https://example.test");
        assert_eq!(cfg.providers[1].config["poll_interval_secs"], 7);

        // Restore
        for (k, v) in saved {
            match v {
                Some(val) => env::set_var(&k, val),
                None => env::remove_var(&k),
            }
        }
    }

    #[test]
    fn env_extra_config_merges_and_overrides_token() {
        let _guard = env_lock().lock().unwrap();
        let vars = ["PC_PROVIDERS", "PC_TELEGRAM_TOKEN", "PC_TELEGRAM_CONFIG", "PC_CONFIG"];
        let saved: Vec<(String, Option<String>)> = vars.iter().map(|k| (k.to_string(), env::var(k).ok())).collect();
        for k in &vars {
            env::remove_var(k);
        }

        env::set_var("PC_PROVIDERS", "telegram");
        env::set_var("PC_TELEGRAM_TOKEN", "old-token");
        // PC_TELEGRAM_CONFIG wins per key over the token env
        env::set_var("PC_TELEGRAM_CONFIG", r#"{"token":"new-token","base_url":"https://override.test"}"#);
        env::remove_var("PC_CONFIG");

        let cfg = load(None).expect("load");
        assert_eq!(cfg.providers[0].config["token"], "new-token");
        assert_eq!(cfg.providers[0].config["base_url"], "https://override.test");

        for (k, v) in saved {
            match v {
                Some(val) => env::set_var(&k, val),
                None => env::remove_var(&k),
            }
        }
    }

    #[test]
    fn invalid_object_error_fails_closed() {
        let _guard = env_lock().lock().unwrap();
        let vars = ["PC_PROVIDERS", "PC_FOO_CONFIG", "PC_CONFIG", "PC_FOO_TOKEN"];
        let saved: Vec<(String, Option<String>)> = vars.iter().map(|k| (k.to_string(), env::var(k).ok())).collect();
        for k in &vars {
            env::remove_var(k);
        }

        // Non-object JSON array
        env::set_var("PC_PROVIDERS", "foo");
        env::set_var("PC_FOO_CONFIG", "[1,2,3]");
        env::remove_var("PC_CONFIG");
        let err = load(None).expect_err("non-object PC_FOO_CONFIG must fail");
        let msg = err.to_string();
        assert!(msg.contains("PC_FOO_CONFIG"), "error should mention env var, got: {msg}");
        assert!(msg.contains("JSON object"), "error should mention JSON object, got: {msg}");

        // Non-object JSON string
        env::set_var("PC_FOO_CONFIG", "\"hello\"");
        let err = load(None).expect_err("string PC_FOO_CONFIG must fail");
        assert!(err.to_string().contains("PC_FOO_CONFIG"));

        // Non-object JSON number
        env::set_var("PC_FOO_CONFIG", "123");
        let err = load(None).expect_err("number PC_FOO_CONFIG must fail");
        assert!(err.to_string().contains("PC_FOO_CONFIG"));

        // Valid object should succeed
        env::set_var("PC_FOO_CONFIG", r#"{"base_url":"https://ok.test"}"#);
        let cfg = load(None).expect("valid object must succeed");
        assert_eq!(cfg.providers[0].config["base_url"], "https://ok.test");

        // Invalid JSON (parse error) also fails
        env::set_var("PC_FOO_CONFIG", "{not json}");
        let err = load(None).expect_err("invalid JSON must fail");
        assert!(err.to_string().contains("invalid config"));

        for (k, v) in saved {
            match v {
                Some(val) => env::set_var(&k, val),
                None => env::remove_var(&k),
            }
        }
    }

    #[test]
    fn empty_extra_config_is_ignored() {
        let _guard = env_lock().lock().unwrap();
        let vars = ["PC_PROVIDERS", "PC_DEMO_CONFIG", "PC_CONFIG", "PC_DEMO_TOKEN"];
        let saved: Vec<(String, Option<String>)> = vars.iter().map(|k| (k.to_string(), env::var(k).ok())).collect();
        for k in &vars {
            env::remove_var(k);
        }
        env::set_var("PC_PROVIDERS", "demo");
        env::set_var("PC_DEMO_CONFIG", "   ");
        env::remove_var("PC_CONFIG");
        let cfg = load(None).expect("whitespace extra config should be ignored");
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(cfg.providers[0].id, "demo");

        for (k, v) in saved {
            match v {
                Some(val) => env::set_var(&k, val),
                None => env::remove_var(&k),
            }
        }
    }
}
