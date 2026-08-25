# Provider-Connect Phases

> Vision: **Rust tiny listener** (idle <30 MB) with **language-agnostic JSON-RPC** — usable as a **headless lib** (`bun add provider-connect`) for any AI agent/app, or as a **standalone bod server** (`pc serve`) that holds provider connections and fans out to any hook.

Read in order. Each phase is one small vertical slice (1–3 commits, <1 day), additive and non-breaking on the wire (`jsonrpc.rs:21` `Id/Request/Response` stable).

| Phase | Title | Lens | Status |
|---|---|---|---|
| 01 | Foundation fixes | review P0/P1 | **done** (`e04ec4a`, `430a8ca`, `a20911b`) |
| 02 | Runtime trim | Bun | **done** (`e9278cc`) |
| 03 | Config crate | Rspack/Next.js | planned |
| 04 | Single binary (`pc`) | Rspack/Bun | planned |
| 05 | EventBus + Plugin | TankStack | planned |
| 06 | HTTP transport (`pc serve`) | Next.js | planned |
| 07 | Headless TS core | TankStack/Bun | planned |
| 08 | FFI + bod daemon | Bun | planned |
| 09 | Polish & release | all | planned |

## How the lenses map

- **TankStack**: headless core + adapters, typed `createProviderClient`, `subscribe(filter) => unsubscribe`, `Plugin` middleware, `Mutation`-like `send`.
- **Rspack/Rsbuild**: unified toolchain, `pc.config.ts` via `defineConfig`, compile-time pruning stays but runtime composition via `Plugin::apply`, `JoinSet` parallel start, single config crate.
- **Next.js**: conventions over config, file `pc.config.ts`, Route Handlers (`POST /api/providers/:id/send`, `GET /api/events` SSE, `GET /health` via `capabilities_value` at `state.rs:142`), `middleware.ts` pipeline, cache/revalidation.
- **Bun**: single binary with subcommands, `Bun.spawn` fast path, `postinstall` prebuilt fetch, `OUTBOUND 32` (not 512 at `state.rs:15`), `rt` not `rt-multi-thread` at `Cargo.toml:19`, optional `mimalloc`, `bun:ffi` cdylib fallback to stdio.

## The two surfaces (always)

1. **As lib** — `provider-core` headless + transports as plugins. Host constructs `ProviderClient::builder().provider(telegram(...)).transport(stdio()).plugin(Retry).build()` (Rust) or `createProviderClient({providers:[telegram(opts)], transports:[stdio()], plugins:[retry()]})` (TS). Any agent hooks via `client.subscribe`.
2. **As bod server** — `pc serve --ws :8787 --http :8788` holds provider connections once (`broadcast::Sender<Outbound>` at `state.rs:43`), multiplexes `event.message` to any number of stdio/WS/HTTP clients. Agents connect/disconnect without provider reconnect; optional sqlite at-least-once later.

## Commit discipline

- One commit per phase file, then 1–2 implementation commits per phase (small).
- No breaking wire change; `capabilities_value` only grows.
- `bin/pc` remains backward-compatible (`pc` with no subcommand = sidecar stdio).
