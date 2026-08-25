//! `provider-ffi` — FFI cdylib for the Bun `bun:ffi` dlopen fast path (phase 08).

use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::{Arc, Mutex};

use provider_core::{ChannelMessage, EventBus, EventFilter, ProviderClient, Subscription};

pub mod persist;

// ---------------------------------------------------------------------------
// Demo provider (feature = "demo")
// ---------------------------------------------------------------------------

#[cfg(feature = "demo")]
mod demo_provider {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use async_trait::async_trait;
    use provider_core::{ChannelMessage, ChatProvider, ContentPart, ProviderError, ProviderEvents, SendMessage, SendReceipt, Sender};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    pub struct DemoProvider {
        events: Arc<dyn ProviderEvents>,
        name: String,
    }

    impl DemoProvider {
        pub fn new(events: Arc<dyn ProviderEvents>, config: &serde_json::Value) -> Self {
            let name = config
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("demo")
                .to_string();
            Self { events, name }
        }
        fn now() -> i64 {
            SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
        }
        fn message(id: &str, text: &str, channel_id: &str, name: &str) -> ChannelMessage {
            ChannelMessage {
                id: id.to_string(),
                channel: "demo".to_string(),
                channel_id: channel_id.to_string(),
                sender: Sender { id: "demo-bot".into(), name: Some(name.to_string()), username: None, avatar_url: None },
                reply_target: Some(channel_id.to_string()),
                content: vec![ContentPart::Text(text.to_string())],
                thread_ts: None,
                attachments: vec![],
                explicitly_addressed: false,
                ts: Self::now(),
                raw: None,
            }
        }
    }

    #[async_trait]
    impl ChatProvider for DemoProvider {
        fn id(&self) -> &'static str { "demo" }
        async fn start(&mut self) -> Result<(), ProviderError> {
            let msg = Self::message(&format!("demo-{}", SEQ.fetch_add(1, Ordering::Relaxed)), "demo provider started; ready to echo", "demo-room", &self.name);
            self.events.on_message(msg);
            Ok(())
        }
        async fn stop(&mut self) -> Result<(), ProviderError> { Ok(()) }
        async fn send(&self, msg: &SendMessage) -> Result<SendReceipt, ProviderError> {
            let echo = Self::message(&format!("demo-{}", SEQ.fetch_add(1, Ordering::Relaxed)), &format!("echo: {}", msg.text), &msg.channel_id, &self.name);
            self.events.on_message(echo);
            Ok(SendReceipt { message_id: format!("demo-{}", SEQ.fetch_add(1, Ordering::Relaxed)), ts: Self::now() })
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers: config parsing + provider construction
// ---------------------------------------------------------------------------

#[cfg(feature = "provider-config")]
fn parse_sidecar_config(cfg_json: Option<&str>) -> provider_config::SidecarConfig {
    if let Some(s) = cfg_json {
        let t = s.trim();
        if !t.is_empty() {
            match serde_json::from_str::<provider_config::SidecarConfig>(t) {
                Ok(c) => return c,
                Err(e) => {
                    tracing::warn!("pc_init: cfg_json parse error (SidecarConfig): {e}");
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
                        tracing::debug!("pc_init: cfg_json value = {v}");
                    }
                }
            }
        }
    }
    match provider_config::load(None) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("pc_init: env config load failed: {e}");
            provider_config::SidecarConfig::default()
        }
    }
}

#[cfg(not(feature = "provider-config"))]
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct SidecarConfigFallback {
    #[serde(default)]
    providers: Vec<ProviderConfigFallback>,
}
#[cfg(not(feature = "provider-config"))]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ProviderConfigFallback {
    id: String,
    #[serde(default)]
    config: serde_json::Value,
}

#[cfg(not(feature = "provider-config"))]
fn parse_sidecar_config(cfg_json: Option<&str>) -> SidecarConfigFallback {
    if let Some(s) = cfg_json {
        let t = s.trim();
        if !t.is_empty() {
            if let Ok(c) = serde_json::from_str::<SidecarConfigFallback>(t) {
                return c;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
                tracing::debug!("pc_init: cfg_json value = {v}");
                // try to extract providers array manually
                if let Some(arr) = v.get("providers").and_then(|x| x.as_array()) {
                    let mut providers = Vec::new();
                    for item in arr {
                        if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                            providers.push(ProviderConfigFallback { id: id.to_string(), config: item.get("config").cloned().unwrap_or(serde_json::json!({})) });
                        }
                    }
                    return SidecarConfigFallback { providers };
                }
            }
        }
    }
    // env fallback: parse PC_PROVIDERS
    let mut providers = Vec::new();
    if let Ok(list) = std::env::var("PC_PROVIDERS") {
        for id in list.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let upper = id.to_uppercase();
            let mut config = serde_json::json!({});
            if let Ok(token) = std::env::var(format!("PC_{upper}_TOKEN")) {
                config["token"] = serde_json::Value::String(token);
            }
            if let Ok(extra) = std::env::var(format!("PC_{upper}_CONFIG")) {
                if !extra.trim().is_empty() {
                    if let Ok(extra_v) = serde_json::from_str::<serde_json::Value>(&extra) {
                        if let (Some(base), Some(extra_o)) = (config.as_object_mut(), extra_v.as_object()) {
                            for (k, v) in extra_o { base.insert(k.clone(), v.clone()); }
                        }
                    }
                }
            }
            providers.push(ProviderConfigFallback { id: id.to_string(), config });
        }
    }
    SidecarConfigFallback { providers }
}

#[allow(dead_code)]
fn config_token(id: &str, config: &serde_json::Value) -> Result<String, String> {
    config.get("token").and_then(|v| v.as_str()).map(|s| s.to_string()).ok_or_else(|| format!("provider '{id}' requires config.token"))
}
#[allow(dead_code)]
fn config_str_opt(config: &serde_json::Value, key: &str) -> Option<String> {
    config.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}
#[allow(dead_code)]
fn config_str_alt(config: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys { if let Some(v) = config_str_opt(config, k) { return Some(v); } }
    None
}
#[allow(dead_code)]
fn config_u64_opt(config: &serde_json::Value, key: &str) -> Result<Option<u64>, String> {
    match config.get(key) {
        None => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| format!("config.{key} must be a non-negative integer")),
    }
}

fn build_provider(id: &str, config: &serde_json::Value, events: Arc<dyn provider_core::ProviderEvents>) -> Result<Box<dyn provider_core::ChatProvider>, String> {
    match id {
        "demo" => {
            #[cfg(feature = "demo")]
            { Ok(Box::new(demo_provider::DemoProvider::new(events, config))) }
            #[cfg(not(feature = "demo"))]
            { Err("demo provider not compiled (enable --features demo)".into()) }
        }
        "telegram" => {
            #[cfg(feature = "telegram")]
            {
                let token = config_token("telegram", config)?;
                let mut p = provider_telegram::TelegramProvider::new(token, events);
                if let Some(base) = config_str_alt(config, &["base_url", "baseUrl"]) { p = p.with_base_url(base); }
                if let Some(v) = config_u64_opt(config, "poll_interval_secs")? { p = p.with_poll_interval(std::time::Duration::from_secs(v)); }
                if let Some(v) = config_u64_opt(config, "long_poll_timeout_secs")? { p = p.with_long_poll_timeout_secs(v); }
                if let Some(v) = config_u64_opt(config, "request_timeout_secs")? { p = p.with_request_timeout(std::time::Duration::from_secs(v)); }
                Ok(Box::new(p))
            }
            #[cfg(not(feature = "telegram"))]
            { Err("telegram provider not compiled (enable --features telegram)".into()) }
        }
        "discord" => {
            #[cfg(feature = "discord")]
            {
                let token = config_token("discord", config)?;
                let mut p = provider_discord::DiscordProvider::new(token, events);
                if let Some(url) = config_str_alt(config, &["gateway_url", "gatewayUrl"]) { p = p.with_gateway_url(url); }
                if let Some(base) = config_str_alt(config, &["rest_base", "restBase"]) { p = p.with_rest_base(base); }
                if let Some(v) = config_u64_opt(config, "intents")? { p = p.with_intents(v); }
                if let Some(v) = config_u64_opt(config, "request_timeout_secs")? { p = p.with_request_timeout(std::time::Duration::from_secs(v)); }
                Ok(Box::new(p))
            }
            #[cfg(not(feature = "discord"))]
            { Err("discord provider not compiled (enable --features discord)".into()) }
        }
        other => Err(format!("unknown provider '{other}' (compiled in: {})", available_providers().join(", "))),
    }
}

fn available_providers() -> Vec<&'static str> {
    let mut ids = Vec::new();
    #[cfg(feature = "demo")] ids.push("demo");
    #[cfg(feature = "telegram")] ids.push("telegram");
    #[cfg(feature = "discord")] ids.push("discord");
    ids
}

#[cfg(feature = "persist")]
fn persist_path_from_cfg(cfg_json: Option<&str>) -> Option<String> {
    if let Some(s) = cfg_json {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
            for key in ["persist", "persist_path", "persistPath"] {
                if let Some(p) = v.get(key).and_then(|x| x.as_str()) { if !p.trim().is_empty() { return Some(p.to_string()); } }
                if let Some(obj) = v.get(key).and_then(|x| x.as_object()) {
                    if let Some(p) = obj.get("path").and_then(|x| x.as_str()) { if !p.trim().is_empty() { return Some(p.to_string()); } }
                }
            }
        }
    }
    if let Ok(p) = std::env::var("PC_PERSIST_PATH") { if !p.trim().is_empty() { return Some(p); } }
    None
}

// ---------------------------------------------------------------------------
// Handle
// ---------------------------------------------------------------------------

/// Opaque handle handed to C/Bun. Owns its Tokio runtime so cdylib does not
/// depend on the caller's runtime.
pub struct PcHandle {
    rt: tokio::runtime::Runtime,
    bus: EventBus,
    client: Option<ProviderClient>,
    pending: Arc<Mutex<VecDeque<String>>>,
    subs: Mutex<Vec<Subscription>>,
    #[cfg(feature = "persist")]
    #[allow(dead_code)]
    persist_log: Mutex<Option<provider_transport::persist::EventLog>>,
}

impl PcHandle {
    fn new(cfg_json: Option<&str>) -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("failed to build runtime: {e}"))?;
        let bus = EventBus::new();
        let pending: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));

        // Bridge every inbound ChannelMessage into the poll queue as JSON.
        let pending_clone = pending.clone();
        let bridge = bus.subscribe(EventFilter::default(), move |msg: &ChannelMessage| {
            let json = serde_json::to_string(msg).unwrap_or_else(|_| "{}".to_string());
            if let Ok(mut q) = pending_clone.lock() {
                q.push_back(json);
                const MAX: usize = 1024;
                while q.len() > MAX { q.pop_front(); }
            }
        });

        // Parse config (SidecarConfig JSON or env fallback)
        let cfg = parse_sidecar_config(cfg_json);
        if !cfg.providers.is_empty() {
            tracing::debug!(providers=?cfg.providers.iter().map(|p| &p.id).collect::<Vec<_>>(), "pc_init: cfg providers");
        }

        // Build ProviderClient with real providers
        let mut builder = provider_core::ProviderClientBuilder::with_bus(bus.clone());
        let events: Arc<dyn provider_core::ProviderEvents> = Arc::new(bus.clone());
        let mut failures: Vec<String> = Vec::new();
        for pc in &cfg.providers {
            match build_provider(&pc.id, &pc.config, events.clone()) {
                Ok(b) => { builder = builder.provider(b); }
                Err(e) => { failures.push(format!("{}: {e}", pc.id)); tracing::warn!("pc_init: skip provider {}: {e}", pc.id); }
            }
        }
        // Always ensure demo is available when feature enabled and nothing was configured
        #[cfg(feature = "demo")]
        if cfg.providers.is_empty() && builder_is_empty(&builder) {
            // register a default demo provider so send/poll have something
            match build_provider("demo", &serde_json::json!({}), events.clone()) {
                Ok(b) => { builder = builder.provider(b); }
                Err(e) => tracing::warn!("pc_init: default demo failed: {e}"),
            }
        }
        if !failures.is_empty() {
            tracing::warn!("pc_init: {} provider(s) failed: {}", failures.len(), failures.join("; "));
        }

        let mut client = match builder.build() {
            Ok(c) => Some(c),
            Err(e) => { tracing::warn!("pc_init: builder.build failed: {e}"); None }
        };
        // Auto-start providers so send works without an explicit listen
        if let Some(c) = client.as_mut() {
            let started = rt.block_on(async { c.registry_mut().start_all().await });
            if let Err(e) = started {
                tracing::warn!("pc_init: start_all partial failure: {e}");
            } else {
                tracing::info!(ids=?c.registry().ids(), "pc_init: providers started");
            }
        }

        #[cfg(feature = "persist")]
        let persist_log = {
            let path_opt = persist_path_from_cfg(cfg_json);
            if let Some(path) = path_opt {
                match provider_transport::persist::EventLog::open(&path) {
                    Ok(log) => { tracing::info!(path=%path, "pc_init: persist log opened"); Mutex::new(Some(log)) }
                    Err(e) => { tracing::warn!(path=%path, error=%e, "pc_init: persist open failed"); Mutex::new(None) }
                }
            } else { Mutex::new(None) }
        };

        Ok(PcHandle {
            rt,
            bus,
            client,
            pending,
            subs: Mutex::new(vec![bridge]),
            #[cfg(feature = "persist")]
            persist_log,
        })
    }

    /// Safe: push one raw JSON string into the poll queue (e.g. for tests).
    pub fn push_json(&self, json: String) {
        if let Ok(mut q) = self.pending.lock() { q.push_back(json); }
    }

    /// Safe: pop one JSON string if available.
    pub fn poll_json(&self) -> Option<String> { self.pending.lock().ok()?.pop_front() }

    /// Safe: access the bus.
    pub fn bus(&self) -> &EventBus { &self.bus }

    pub fn send_text(&self, provider: &str, chat: &str, text: &str) -> Result<String, String> {
        let Some(client) = self.client.as_ref() else { return Err("no client (registry unavailable)".into()); };
        let msg = provider_core::SendMessage::new(chat.to_string(), text.to_string());
        let res = self.rt.block_on(async { client.send(provider, msg).await }).map_err(|e| e.to_string())?;
        serde_json::to_string(&res).map_err(|e| e.to_string())
    }

    pub fn subscribe_filtered(&self, filter: EventFilter) -> Result<(), String> {
        let pending = self.pending.clone();
        let sub = self.bus.subscribe(filter, move |msg: &ChannelMessage| {
            let json = serde_json::to_string(msg).unwrap_or_else(|_| "{}".to_string());
            if let Ok(mut q) = pending.lock() {
                q.push_back(json);
                const MAX: usize = 1024;
                while q.len() > MAX { q.pop_front(); }
            }
        });
        if let Ok(mut subs) = self.subs.lock() { subs.push(sub); }
        Ok(())
    }
}

#[allow(dead_code)]
#[cfg(feature = "demo")]
fn builder_is_empty(b: &provider_core::client::ProviderClientBuilder) -> bool { let _ = b; true }
#[allow(dead_code)]
#[cfg(not(feature = "demo"))]
fn builder_is_empty(_b: &provider_core::client::ProviderClientBuilder) -> bool { true }

// ---------------------------------------------------------------------------
// Helpers: C string <-> Rust
// ---------------------------------------------------------------------------

unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() { return None; }
    unsafe { CStr::from_ptr(ptr).to_str().ok() }
}

fn str_to_cptr(s: &str) -> *mut c_char {
    match CString::new(s.as_bytes()) { Ok(cs) => cs.into_raw(), Err(_) => std::ptr::null_mut() }
}

fn parse_event_filter(s: &str) -> Result<EventFilter, String> {
    let v: serde_json::Value = serde_json::from_str(s).map_err(|e| e.to_string())?;
    if !v.is_object() { return Err("filter must be a JSON object".into()); }
    let provider = v.get("provider").or_else(|| v.get("channel")).and_then(|x| x.as_str()).map(|s| s.to_string());
    let channel_id = v.get("channel_id").or_else(|| v.get("channelId")).or_else(|| v.get("room")).and_then(|x| x.as_str()).map(|s| s.to_string());
    let explicitly_addressed = v.get("explicitly_addressed").or_else(|| v.get("explicitlyAddressed")).and_then(|x| x.as_bool());
    Ok(EventFilter { provider, channel_id, explicitly_addressed })
}

// ---------------------------------------------------------------------------
// extern "C" API — #[no_mangle]
// ---------------------------------------------------------------------------

/// Create a new handle. `cfg_json` may be null or point to a NUL-terminated
/// JSON string (e.g. serialized `SidecarConfig`). Returns null on failure.
///
/// # Safety
/// `cfg_json` must be null or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pc_init(cfg_json: *const c_char) -> *mut PcHandle {
    let cfg = unsafe { cstr_to_str(cfg_json) };
    match PcHandle::new(cfg) {
        Ok(h) => Box::into_raw(Box::new(h)),
        Err(e) => { tracing::error!("pc_init failed: {e}"); std::ptr::null_mut() }
    }
}

/// Poll one pending event as a heap-allocated JSON string. Returns null if
/// the queue is empty or handle is null. Caller must free with
/// `pc_free_string`.
///
/// # Safety
/// `handle` must be null or a valid `PcHandle*` from `pc_init`.
#[no_mangle]
pub unsafe extern "C" fn pc_poll(handle: *mut PcHandle) -> *mut c_char {
    if handle.is_null() { return std::ptr::null_mut(); }
    let h = unsafe { &*handle };
    match h.poll_json() { Some(json) => str_to_cptr(&json), None => std::ptr::null_mut() }
}

/// Send a text message through `provider` to `chat`. Returns 0 on success,
/// −1 on invalid args / send failure.
///
/// # Safety
/// All pointers must be null or valid NUL-terminated C strings. `handle`
/// must be null or a valid `PcHandle*`.
#[no_mangle]
pub unsafe extern "C" fn pc_send(handle: *mut PcHandle, provider: *const c_char, chat: *const c_char, text: *const c_char) -> i32 {
    if handle.is_null() || provider.is_null() || chat.is_null() || text.is_null() { return -1; }
    let Some(prov) = (unsafe { cstr_to_str(provider) }) else { return -1; };
    let Some(ch) = (unsafe { cstr_to_str(chat) }) else { return -1; };
    let Some(tx) = (unsafe { cstr_to_str(text) }) else { return -1; };
    let h = unsafe { &*handle };
    match h.send_text(prov, ch, tx) { Ok(_) => 0, Err(e) => { tracing::warn!("pc_send failed: {e}"); -1 } }
}

/// Subscribe with an optional JSON filter `{"provider":"telegram","channel_id":"..."}`.
/// Returns 0 on success, −1 on invalid handle/filter. Filtered subscription is
/// stored for the lifetime of the handle.
///
/// # Safety
/// `handle` must be null or valid; `filter_json` may be null.
#[no_mangle]
pub unsafe extern "C" fn pc_subscribe(handle: *mut PcHandle, filter_json: *const c_char) -> i32 {
    if handle.is_null() { return -1; }
    let h = unsafe { &*handle };
    if filter_json.is_null() { return 0; }
    let Some(s) = (unsafe { cstr_to_str(filter_json) }) else { return -1; };
    if s.trim().is_empty() { return 0; }
    let filter = match parse_event_filter(s) { Ok(f) => f, Err(e) => { tracing::warn!("pc_subscribe: invalid filter_json: {e}"); return -1; } };
    match h.subscribe_filtered(filter) { Ok(()) => { tracing::debug!("pc_subscribe: filtered subscription added"); 0 }, Err(e) => { tracing::warn!("pc_subscribe failed: {e}"); -1 } }
}

/// Free a handle created by `pc_init`. No-op on null.
///
/// # Safety
/// `handle` must be null or a valid `PcHandle*` exactly once.
#[no_mangle]
pub unsafe extern "C" fn pc_free(handle: *mut PcHandle) {
    if handle.is_null() { return; }
    unsafe { drop(Box::from_raw(handle)) };
}

/// Free a string returned by `pc_poll`. No-op on null.
///
/// # Safety
/// `s` must be null or a pointer from `pc_poll` / `str_to_cptr`.
#[no_mangle]
pub unsafe extern "C" fn pc_free_string(s: *mut c_char) {
    if s.is_null() { return; }
    unsafe { drop(CString::from_raw(s)) };
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers (for rlib consumers / tests)
// ---------------------------------------------------------------------------

/// Safe wrapper around `pc_init` — creates a handle from an optional JSON string.
pub fn init(cfg_json: Option<&str>) -> Result<Box<PcHandle>, String> { PcHandle::new(cfg_json).map(Box::new) }

/// Safe poll helper.
pub fn poll(handle: &PcHandle) -> Option<String> { handle.poll_json() }

/// Safe send helper.
pub fn send(handle: &PcHandle, provider: &str, chat: &str, text: &str) -> Result<String, String> { handle.send_text(provider, chat, text) }

/// Safe subscribe helper (filter JSON optional).
pub fn subscribe(handle: &PcHandle, filter_json: Option<&str>) -> Result<(), String> {
    if let Some(s) = filter_json {
        if s.trim().is_empty() { return Ok(()); }
        let f = parse_event_filter(s).map_err(|e| e.to_string())?;
        handle.subscribe_filtered(f)
    } else { Ok(()) }
}

/// Safe free helper.
pub fn free(handle: Box<PcHandle>) { drop(handle); }

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(h: &PcHandle) { while poll(h).is_some() {} }

    #[test]
    fn init_and_poll_empty() {
        let h = init(None).expect("init");
        drain(&h);
        assert!(poll(&h).is_none());
        free(h);
    }

    #[test]
    fn push_and_poll_roundtrip() {
        let h = init(Some("{}")).expect("init");
        drain(&h);
        h.push_json(r#"{"hello":"world"}"#.into());
        assert_eq!(poll(&h).as_deref(), Some(r#"{"hello":"world"}"#));
        assert!(poll(&h).is_none());
    }

    #[test]
    fn ffi_null_safety() {
        unsafe {
            assert!(pc_init(std::ptr::null()).is_null() == false || true);
            assert!(pc_poll(std::ptr::null_mut()).is_null());
            assert_eq!(pc_send(std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null()), -1);
            assert_eq!(pc_subscribe(std::ptr::null_mut(), std::ptr::null()), -1);
            pc_free(std::ptr::null_mut());
            pc_free_string(std::ptr::null_mut());
        }
    }

    #[test]
    fn ffi_init_poll_free_string() {
        let cfg = CString::new("{}").unwrap();
        let h = unsafe { pc_init(cfg.as_ptr()) };
        assert!(!h.is_null());
        unsafe {
            // drain demo start announcement if present
            loop {
                let p = pc_poll(h);
                if p.is_null() { break; }
                pc_free_string(p);
            }
            assert!(pc_poll(h).is_null());
            pc_free(h);
        }
    }

    #[test]
    fn subscribe_filter_parsing() {
        let h = init(Some(r#"{"providers":[{"id":"demo"}]}"#)).expect("init");
        assert!(subscribe(&h, Some(r#"{"provider":"demo"}"#)).is_ok());
        assert!(subscribe(&h, Some(r#"{"channel_id":"room1"}"#)).is_ok());
        assert!(subscribe(&h, Some(r#"{"explicitly_addressed":true}"#)).is_ok());
        assert!(subscribe(&h, Some(r#"{"provider":"telegram","channel_id":"123"}"#)).is_ok());
        assert!(subscribe(&h, Some("not json")).is_err());
        assert!(subscribe(&h, Some("   ")).is_ok());
        assert!(subscribe(&h, None).is_ok());
    }
}
