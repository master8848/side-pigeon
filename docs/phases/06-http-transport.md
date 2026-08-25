# Phase 06 — HTTP transport + `pc serve` (Route Handlers)

**Lens:** Next.js · **Status:** planned

## Why

`README.md:6` promises WS/HTTP optional, but `bin/pc/src/main.rs:170` only calls `serve_stdio`; `state.rs:48` documents `ws: one per connection; http: none — notifications dropped` as aspirational. Stdio is single-parent (one `opencode-plugin` _or_ `pi-plugin`, not both). Next.js solved this with standalone server + Route Handlers — any client `fetch("/api/send")` hooks a single origin. The **bod server** is that origin for providers.

## Scope

- New `crates/provider-transport/src/http.rs` (opt-in `http` feature, `hyper`/`axum` minimal):
  ```
  GET  /health                     -> capabilities_value() at state.rs:142 (for k8s / pc check)
  POST /api/providers/:id/send     -> AppState::send (typed SendMessage, JSON)
  GET  /api/events                 -> SSE subscription to broadcast::Sender<Outbound> (fan-out)
  POST /rpc                        -> NDJSON/JSON-RPC batch (same dispatch as stdio/WS)
  ```
  Keeps stdio primary; enabled via `pc serve --http :3000 --ws :3001`.
- `bin/pc/src/main.rs:132` `AppState::new` takes `Vec<String>` transports (`state.rs:51`) not single `&str`; `capabilities_value` reflects `["stdio","http","ws"]`.
- `provider-transport/src/ws.rs` `OUT_QUEUE_CAPACITY 1024` already in place (Phase 01); HTTP reuses `handle_request` dispatch at `state.rs:95`.
- `Registry::start_all` parallelized with `JoinSet` (`registry.rs:121` sequential loop -> `FuturesUnordered`); per-provider `poll_interval` jitter already added in Phase 01, add startup jitter.

## Exit criteria

- `pc serve --ws :8787 --http :8788` holds provider connections once, fans out `event.message` to any number of stdio/WS/SSE clients; agents connect/disconnect without provider reconnect.
- `GET /health` probes without JSON-RPC handshake; `POST /api/providers/:id/send` is a Next-like Route Handler (typed, middleware-interceptable hook in Phase 05).
- `--help` and `docs/api-contract.md` updated with HTTP surface; stdio path unchanged.

## Files

- `crates/provider-transport/src/http.rs` (new), `state.rs:15,51,95,142,194`, `jsonrpc.rs:1`
- `bin/pc/src/main.rs:132,160` serve selection + `AppState` transport vec
- `Cargo.toml:25` `hyper`/`axum` behind `http` feature

## Notes

- Rspack/Bun concern: keep HTTP behind feature (`http = ["dep:hyper", ...]`) so idle sidecar stays lean; `pc serve` is the daemon mode, `pc` (no args) stays stdio-only.
- Follow-up (Phase 08): sqlite at-least-once persistence for messages dropped during serve downtime (addresses `cli/src/main.rs:71` `PROVIDER DATA-LOSS WARNING`).
