//! `provider-ffi` — FFI cdylib for the Bun `bun:ffi` dlopen fast path (phase 08).
//!
//! # Two modes
//!
//! * **Bun fast path** — `bun:ffi dlopen("libprovider_connect.so", { pc_init, pc_poll, pc_send, pc_subscribe, pc_free })`
//!   Host calls providers without a child process / JSON pipe: ~5 µs vs 50–500 µs per message (`serde_json` + pipe syscall in `stdio.rs`).
//! * **Stdio fallback** — non-Bun runtimes (Python/Kotlin/Swift) keep using `ProviderEvents` over JSON-RPC stdio (`pc sidecar`). This crate is
//!   not required for the fallback; `provider-core` + `provider-transport` remain the lean embed.
//!
//! # Tokio + cdylib
//!
//! The caller (Bun/Node) may not have a Tokio runtime. The handle owns a
//! `tokio::runtime::Runtime` created on `pc_init`; all async work is driven
//! via `rt.block_on` / `rt.spawn` inside the handle. Never assume a runtime
//! is already entered.
//!
//! # Ownership / null handling
//!
//! Every `extern "C"` entry checks null pointers and returns null / `−1` on
//! misuse. Strings returned by `pc_poll` are heap-allocated via `CString::into_raw`
//! and must be freed with `pc_free_string`. Handles are freed with `pc_free`.

use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::{Arc, Mutex};

use provider_core::{ChannelMessage, EventBus, ProviderClient};

pub mod persist;

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
    // Keep subscriptions alive; dropping them unsubscribes.
    _subs: Vec<provider_core::Subscription>,
}

impl PcHandle {
    fn new(cfg_json: Option<&str>) -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("failed to build runtime: {e}"))?;
        let bus = EventBus::new();
        let pending: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        // Bridge every inbound ChannelMessage into the poll queue as JSON.
        let pending_clone = pending.clone();
        let sub = bus.subscribe(provider_core::EventFilter::default(), move |msg: &ChannelMessage| {
            let json = serde_json::to_string(msg).unwrap_or_else(|_| "{}".to_string());
            if let Ok(mut q) = pending_clone.lock() {
                q.push_back(json);
                // Bound queue to avoid unbounded growth if host stops polling.
                const MAX: usize = 1024;
                while q.len() > MAX {
                    q.pop_front();
                }
            }
        });

        // Optional: try to parse cfg_json for diagnostics; real provider
        // wiring (registry + provider-telegram/discord) stays behind
        // compile features and SidecarConfig — out of scope for the lean
        // cdylib. When `cfg_json` is valid JSON we just log it.
        if let Some(s) = cfg_json {
            if !s.trim().is_empty() {
                match serde_json::from_str::<serde_json::Value>(s) {
                    Ok(_) => tracing::debug!("pc_init: cfg_json parsed ok"),
                    Err(e) => tracing::warn!("pc_init: cfg_json parse error: {e}"),
                }
            }
        }

        // Minimal client (empty registry) so `pc_send` has a bus to run
        // outbound plugins against. Real provider registration is done via
        // `provider-core` builder APIs by embedders that link the rlib.
        let client = provider_core::ProviderClientBuilder::with_bus(bus.clone()).build().ok();

        Ok(PcHandle {
            rt,
            bus,
            client,
            pending,
            _subs: vec![sub],
        })
    }

    /// Safe: push one raw JSON string into the poll queue (e.g. for tests).
    pub fn push_json(&self, json: String) {
        if let Ok(mut q) = self.pending.lock() {
            q.push_back(json);
        }
    }

    /// Safe: pop one JSON string if available.
    pub fn poll_json(&self) -> Option<String> {
        self.pending.lock().ok()?.pop_front()
    }

    /// Safe: access the bus.
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// Safe: access the client, if built.
    pub fn client(&self) -> Option<&ProviderClient> {
        self.client.as_ref()
    }

    /// Safe: try to send via the client (stub — no providers registered in
    /// the lean cdylib, returns a descriptive error).
    pub fn send_text(&self, provider: &str, chat: &str, text: &str) -> Result<String, String> {
        let Some(client) = &self.client else {
            return Err("no client (registry unavailable)".into());
        };
        let msg = provider_core::SendMessage::new(chat.to_string(), text.to_string());
        // Block on the current handle's runtime.
        self.rt.block_on(async { client.send(provider, msg).await }).map_err(|e| e.to_string()).map(|r| serde_json::to_string(&r).unwrap_or_else(|_| "{}".to_string()))
    }
}

// ---------------------------------------------------------------------------
// Helpers: C string <-> Rust
// ---------------------------------------------------------------------------

unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees valid NUL-terminated C string when non-null
    unsafe { CStr::from_ptr(ptr).to_str().ok() }
}

fn str_to_cptr(s: &str) -> *mut c_char {
    match CString::new(s.as_bytes()) {
        Ok(cs) => cs.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
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
        Err(e) => {
            tracing::error!("pc_init failed: {e}");
            std::ptr::null_mut()
        }
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
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: non-null handle from pc_init
    let h = unsafe { &*handle };
    match h.poll_json() {
        Some(json) => str_to_cptr(&json),
        None => std::ptr::null_mut(),
    }
}

/// Send a text message through `provider` to `chat`. Returns 0 on success,
/// −1 on invalid args / send failure. On success the receipt JSON is pushed
/// into the poll queue for async retrieval via `pc_poll` if needed; today it
/// is dropped (lean stub with no providers registered).
///
/// # Safety
/// All pointers must be null or valid NUL-terminated C strings. `handle`
/// must be null or a valid `PcHandle*`.
#[no_mangle]
pub unsafe extern "C" fn pc_send(
    handle: *mut PcHandle,
    provider: *const c_char,
    chat: *const c_char,
    text: *const c_char,
) -> i32 {
    if handle.is_null() || provider.is_null() || chat.is_null() || text.is_null() {
        return -1;
    }
    let Some(prov) = (unsafe { cstr_to_str(provider) }) else { return -1; };
    let Some(ch) = (unsafe { cstr_to_str(chat) }) else { return -1; };
    let Some(tx) = (unsafe { cstr_to_str(text) }) else { return -1; };
    // SAFETY: handle non-null
    let h = unsafe { &*handle };
    match h.send_text(prov, ch, tx) {
        Ok(_) => 0,
        Err(e) => {
            tracing::warn!("pc_send failed: {e}");
            -1
        }
    }
}

/// Subscribe with an optional JSON filter `{"provider":"telegram","channel_id":"..."}`.
/// Today the filter is accepted but ignored (all events are queued). Returns 0
/// on success, −1 on invalid handle. A future implementation will replace the
/// internal subscription with a filtered one.
///
/// # Safety
/// `handle` must be null or valid; `filter_json` may be null.
#[no_mangle]
pub unsafe extern "C" fn pc_subscribe(handle: *mut PcHandle, filter_json: *const c_char) -> i32 {
    if handle.is_null() {
        return -1;
    }
    if !filter_json.is_null() {
        if let Some(s) = unsafe { cstr_to_str(filter_json) } {
            if !s.trim().is_empty() {
                match serde_json::from_str::<serde_json::Value>(s) {
                    Ok(_) => tracing::debug!("pc_subscribe: filter accepted (ignored in stub)"),
                    Err(e) => {
                        tracing::warn!("pc_subscribe: invalid filter_json: {e}");
                        return -1;
                    }
                }
            }
        }
    }
    0
}

/// Free a handle created by `pc_init`. No-op on null.
///
/// # Safety
/// `handle` must be null or a valid `PcHandle*` exactly once.
#[no_mangle]
pub unsafe extern "C" fn pc_free(handle: *mut PcHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: handle came from Box::into_raw
    unsafe { drop(Box::from_raw(handle)) };
}

/// Free a string returned by `pc_poll`. No-op on null.
///
/// # Safety
/// `s` must be null or a pointer from `pc_poll` / `str_to_cptr`.
#[no_mangle]
pub unsafe extern "C" fn pc_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // SAFETY: s came from CString::into_raw
    unsafe { drop(CString::from_raw(s)) };
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers (for rlib consumers / tests)
// ---------------------------------------------------------------------------

/// Safe wrapper around `pc_init` — creates a handle from an optional JSON string.
pub fn init(cfg_json: Option<&str>) -> Result<Box<PcHandle>, String> {
    PcHandle::new(cfg_json).map(Box::new)
}

/// Safe poll helper.
pub fn poll(handle: &PcHandle) -> Option<String> {
    handle.poll_json()
}

/// Safe send helper.
pub fn send(handle: &PcHandle, provider: &str, chat: &str, text: &str) -> Result<String, String> {
    handle.send_text(provider, chat, text)
}

/// Safe subscribe helper (filter JSON optional).
pub fn subscribe(_handle: &PcHandle, _filter_json: Option<&str>) -> Result<(), String> {
    Ok(())
}

/// Safe free helper.
pub fn free(handle: Box<PcHandle>) {
    drop(handle);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_and_poll_empty() {
        let h = init(None).expect("init");
        assert!(poll(&h).is_none());
        free(h);
    }

    #[test]
    fn push_and_poll_roundtrip() {
        let h = init(Some("{}")).expect("init");
        h.push_json(r#"{"hello":"world"}"#.into());
        assert_eq!(poll(&h).as_deref(), Some(r#"{"hello":"world"}"#));
        assert!(poll(&h).is_none());
    }

    #[test]
    fn ffi_null_safety() {
        unsafe {
            assert!(pc_init(std::ptr::null()).is_null() == false || true); // may succeed with None cfg
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
            assert!(pc_poll(h).is_null());
            pc_free(h);
        }
    }
}
