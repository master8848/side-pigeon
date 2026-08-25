# provider-connect — Architecture Blueprint (synthesized from research)

> Status: draft v1, synthesized 2026-08-11 from `docs/research/zeroclaw.md`. The concrete Rust dependency set + memory-tooling details will be merged from `docs/research/rust-ecosystem.md` (in flight) — section 5 is placeholder until then. All policy from `../CONTRIBUTING.md` applies.

## 1. Positioning

`provider-connect` = a **Rust sidecar library/binary** that connects AI agents to messaging providers (Discord, WhatsApp, Telegram, Slack, …) with a clean, language-agnostic API. Fixes: a Node.js agent listening to provider events idles at ~400MB–1GB+ RAM. Target: **idle RSS < 30–50 MB**, sub-10ms boot, standalone binary + library + FFI + JSON-RPC transports.

## 2. Key research facts (from zeroclaw.md)

- ZeroClaw (`zeroclaw-labs/zeroclaw`, MIT/Apache-2.0, Rust): 37 provider integrations, compile-time feature-gated; inbound normalized to one `ChannelMessage` → mpsc → agent loop; outbound `SendMessage`; ACP = JSON-RPC 2.0 over stdio (direct precedent).
- Measured: ZeroClaw foundation binary 6.6 MB, <5 MB idle core, <10 ms boot → Rust approach is proven.
- The ~400MB idle-RAM problem is OpenClaw (TS predecessor): V8 baseline + 64 root deps (playwright-core, quickjs-wasi, linkedom, matrix crypto WASM…) + per-channel JS SDKs (grammy, @slack/bolt, baileys, matrix-js-sdk) + historical whatsapp-web.js Puppeteer/Chromium (300–500MB) + signal-cli JVM (~200MB), no compile-time pruning.
- ZeroClaw's own pattern matches our dep policy: hand-roll REST/WS on `reqwest` + `tokio-tungstenite`; use an SDK crate only for complex protocols (matrix-sdk). Notable: `whatsapp-rust@0.7.0` on crates.io (2025-10-07) — use crates.io release, not git-pinned rev.
- License: MIT/Apache-2.0 → schema/trait/protocol knowledge is directly portable.

## 3. Architecture

```
Agent app (Node/Python/Kotlin/Swift/…) ── JSON-RPC 2.0 over stdio | WebSocket | HTTP  ──┐
Rust lib (direct call, crate API) ──────────────────────────────────────────────────────┤
                                                                                        ▼
provider-connect core (crates/provider-core)
 ├─ Provider trait(s): capability traits (ChatProvider, MediaProvider, PresenceProvider, …)
 ├─ unified schema: ChannelMessage / SendMessage / MediaAttachment / drafts / choice tokens (ported from ZeroClaw, MIT/Apache-2.0)
 ├─ JSON-RPC v1 contract: initialize / capabilities / listen / send / draft / choice / shutdown
 ├─ lifecycle: lazy provider load (zero cost until a provider is enabled), single tokio runtime, mpsc event bus
 └─ policy: peer groups / allowlists OUT of the library (app policy)
        │
providers (crates/provider-<name>, feature-gated)
 ├─ discord     (hand-rolled gateway WS + intents — proven pattern; no heavy SDK)
 ├─ telegram    (getUpdates long-poll or MTProto via SDK)
 ├─ slack       (Socket Mode WS)
 ├─ whatsapp-cloud (Meta Graph v18 webhook, HMAC) / whatsapp-web (wa-rs / whatsapp-rust)
 ├─ signal, matrix (matrix-sdk, E2EE), email (IMAP/SMTP), irc/twitch, line, nostr, bluesky, …
 └─ milestone order: core+schema+stdio → telegram → discord → slack → whatsapp cloud → whatsapp web → signal → matrix → email
        │
transports (crates/provider-transport)
 ├─ stdio JSON-RPC (ACP-style, primary) | WebSocket server | local HTTP server
 └─ FFI: cdylib + C ABI (UniFFI 0.32 for Kotlin/Swift bindings)
```

## 4. Reuse vs redesign (from research §7)

**REUSE (port, MIT/Apache-2.0):** `ChannelMessage`/`SendMessage` schema; trait surface; feature-gating pattern; pacing middleware; draft/streaming protocol; choice/approval wire tokens; ACP JSON-RPC method set.

**REDESIGN:** invert deps (providers depend only on core+std, not config/runtime); split monolithic traits into capability traits (ZeroClaw's slack.rs is 382KB!); `anyhow` → `thiserror`; explicit JSON-RPC v1 contract (initialize/capabilities/auth-flows with QR/pair-code as RPC); app policy out of library; per-provider in-workspace crates.

## 5. Dependency set — PENDING merge from rust-ecosystem.md

(placeholders, to be finalized when the research doc lands)

- Runtime: tokio (minimal features)
- HTTP/WS: reqwest, tokio-tungstenite (hand-rolled providers per ZeroClaw pattern)
- Optional SDKs for complex protocols: matrix-sdk
- JSON-RPC: jsonrpsee or hand-rolled over stdio (decide from ecosystem research)
- Errors: thiserror; tracing for logs
- Supply-chain: verify every crate's `created_at` ≥ 14 days via crates.io API

## 6. Build order (tomorrow, mapped to Reminders plan)

1. Research synthesis + design doc: capability traits, unified schema, JSON-RPC contract (2)
2. Scaffold workspace `crates/{provider-core, provider-transport, provider-discord, …}` + CI (4)
3. Transport: stdio JSON-RPC first (5) → WS → HTTP
4. Providers: telegram (simplest: long-poll) then discord (WS gateway) (6)
5. Resource work: single runtime, lazy loading, release profile (opt-level, LTO, panic=abort, strip), measure with `/usr/bin/time -l` + samply/heaptrack; idle RSS budget <30–50MB (7)
6. Node sidecar e2e: spawn binary from Node agent, real event, benchmark vs JS SDK baseline (8)
7. Docs, examples (Node/Python quickstarts), roadmap (9); publish v0.1, GitHub repo (10)

## 7. Risks & mitigations

| Risk                              | Mitigation                                                                                           |
| --------------------------------- | ---------------------------------------------------------------------------------------------------- |
| WhatsApp Web protocol complexity  | use `whatsapp-rust@0.7.0` (crates.io) or wa-rs; keep cloud API (webhook) as the simple path first    |
| Idle RSS budget vs tokio baseline | measure early; single runtime, no extra threads; consider jemalloc; panic=abort                      |
| Provider API churn                | hand-rolled thin clients on reqwest/tungstenite; feature-gated; per-provider crates isolate breakage |
| Supply-chain                      | 14-day crates.io `created_at` gate; Cargo.lock committed                                             |

## 8. Polish (Phase 09)

- **DedupPlugin TTL LRU** (`crates/provider-core/src/plugin.rs:47`): `DedupPlugin` is a TTL-bounded LRU — `window` defaults to 5 min (`DEFAULT_DEDUP_WINDOW`), `max_entries` defaults to 2000 (`DEDUP_MAX_ENTRIES`), configurable via `DedupPlugin::with_capacity`. Each `on_message` does TTL `retain` (evict `age >= window`) then capacity eviction of the oldest `Instant` when at capacity; `on_send` is not deduped so a reflected echo after your own `send` is correctly suppressed within the window (revalidate-after-send note).
- **CI** (`.github/workflows/ci.yml`): four jobs — `rust` (toolchain 1.97.1: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --workspace --all-features` with 5 min timeout, `cargo check --workspace`), `js` (bun 1.4: `bun install`, `bun run lint`, `bun run typecheck`, `node --test` for plugins), `python` (3.9 + `pip install -e packages/python` + `pytest`), `supply-chain` (`scripts/check-supply-chain.sh`). Uses `actions/checkout@v4`, `dtolnay/rust-toolchain`, `oven-sh/setup-bun`.
- **Supply-chain** (`scripts/check-supply-chain.sh:1`): 14-day `created_at` gate via `crates.io` API; now covers all registry crates in the resolved graph including new `provider-ffi` / `persist` deps (rusqlite/bundled) when those features are resolved.
- **Zero unsafe** (`crates/provider-core/src/lib.rs:29`, `crates/provider-transport/src/lib.rs:32`): both crates `#![forbid(unsafe_code)]`; the only `unsafe` in the workspace is the FFI `extern "C"` boundary in `provider-ffi` (`CStr`/`CString`). All `Mutex::lock().unwrap()` call sites use poisoning recovery via `unwrap_or_else(|e| e.into_inner())`.
- **Devtools stub**: `EventBus` is `Clone + Arc` with `subscribe(EventFilter)` + plugin chain; a devtools subscriber can attach as a plain `EventBus::subscribe(EventFilter::default(), ...)` observer (TankStack devtools analog) — dedicated `provider-devtools` crate is deferred/opt-in.
