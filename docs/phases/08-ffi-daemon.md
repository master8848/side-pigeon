# Phase 08 — FFI + bod daemon

**Lens:** Bun · **Status:** planned

## Why

Phase 06's `pc serve` is a transport-level fan-out (one broadcast); a persistent **bod daemon** is the Rsbuild `dev` / Next `standalone` / Bun `serve()` analog: holds gateway/long-poll connections across agent restarts, hot-reloads `pc.config.ts`, optional persistence for at-least-once (ZeroClaw lesson, fixes `cli/src/main.rs:71` `PROVIDER DATA-LOSS WARNING` "Messages received while not running are LOST").

Stdio JSON-RPC is correct for universality (`README.md:6` ACP precedent) but pays `serde_json` + pipe syscall (`stdio.rs:74` `to_string + write_all + flush` per message). Bun's fast path is `bun:ffi` `dlopen` (`pc` as `cdylib`) — 5 µs vs 50–500 µs.

## Scope

### FFI cdylib

- New `crates/provider-ffi` with `#[no_mangle] extern "C" fn pc_init(cfg:*const c_char)->*mut PcHandle`, `pc_poll`, `pc_send`, `pc_subscribe`.
- `provider-core/Cargo.toml:12` `registry` feature stays lean; `provider-ffi` links `provider-core` + providers behind compile features.
- TS binding via `bun:ffi dlopen("libprovider_connect.so", {pc_init,...})` with stdio fallback for non-Bun runtimes.

### Daemon

- `pc serve --watch` (watch `pc.config.ts`/JSON), prints `ready at ws://localhost:8787 http://localhost:8788`, `event.error` overlay (tracing stderr already at `bin/pc/src/main.rs:187`).
- `pc dev` alias for serve+watch (Rsbuild analog).
- Optional `sqlite` feature for message log (at-least-once replay into `GET /api/events?since=cursor`).

### Any-agent hook

- WS: `ws://.../events` NDJSON over WS (reuses `ws.rs`), HTTP: `POST /send` + `GET /events` (SSE).
- One `AppState` with `transport: vec!["stdio","ws","http"]` mounts same `handle_request` dispatch on all transports.

## Exit criteria

- `bun:ffi` path: host calls provider without child process / JSON; stdio fallback still works for Python/Kotlin/Swift via `ProviderEvents`.
- `pc serve` daemon survives agent connect/disconnect; `GET /api/events?since=<cursor>` replays if persistence enabled.
- Idle RSS / binary size documented per mode (stdio vs daemon vs cdylib).

## Files

- `crates/provider-ffi/src/lib.rs` (new), `Cargo.toml` `cdylib` crate-type
- `bin/pc/src/main.rs:160` tokio runtime shared between FFI and daemon paths
- `crates/provider-transport/src/{state.rs,http.rs,ws.rs}` transport vec wiring

## Risks

- `cdylib` + `tokio` + `reqwest` interaction tricky; separate `provider-ffi` crate isolates it.
