# Rust ecosystem research — what provider-connect actually used

> Status: written by orchestrator 2026-08-11 after implementation (the dedicated
> research agent errored). This documents the real, verified dependency set.

## Implementation reality (final Cargo.toml, workspace)

| Area | Choice | Why (verified) |
|---|---|---|
| Runtime | `tokio` (rt-multi-thread, macros, sync, io-std, io-util, time, net) | minimal feature set; `pc` binary uses `current_thread` runtime |
| HTTP client | `reqwest` 0.12 (default-features off, rustls-tls) | hand-rolled provider clients (ZeroClaw pattern); rustls avoids OpenSSL |
| WebSocket | `tokio-tungstenite` 0.24 | Discord Gateway v10 + optional ws transport |
| JSON-RPC | hand-rolled on serde_json (newline-delimited stdio) | smaller than jsonrpsee; exact control over framing/errors |
| Errors | `thiserror` 2 | contract ProviderError variants |
| Logging | `tracing` + `tracing-subscriber` (fmt, env-filter) | stderr logs; stdout reserved for RPC |
| Async traits | `async-trait` | Box<dyn ChatProvider> in the registry |

## Memory results (measured 2026-08-11)

- `pc` release binary: 1.1 MB stripped (opt-level 3, lto thin, codegen-units 1, panic abort, strip)
- Idle RSS via `/usr/bin/time -l`: **1.64 MB** (--help) / **2.2 MB** running-idle — target was < 50 MB
- Node example hosting the sidecar: ~42 MB Node RSS total (agent + sidecar)

## Memory tooling notes

- `/usr/bin/time -l` — quick RSS; `samply` / `heaptrack` for heap profiling (documented, not required at current footprint)
- Keep the runtime single-threaded (current_thread) for the sidecar; no extra threads per provider

## Supply-chain (from scripts/check-supply-chain.sh)

- 159 registry crates in the full closure audited via crates.io `created_at`; all >= 14 days (youngest 236d at audit time)
- Script: `cargo metadata --all-features` -> curl crates.io API -> 14-day gate

## Provider notes

- Discord: hand-rolled Gateway v10 on tokio-tungstenite (no serenity) — matches ZeroClaw pattern
- Telegram: getUpdates long-poll on reqwest (no teloxide needed)
- WhatsApp (roadmap): `whatsapp-rust` 0.7.0 on crates.io (2025-10-07) — use crates.io release, not git-pin
- Complex protocols later (Matrix E2EE): matrix-sdk is the documented exception where an SDK crate is justified
