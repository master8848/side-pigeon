# side-pigeon

> **side-pigeon** — single Rust binary `pc` that holds Telegram/Discord connections lean (~12-28 MB idle), wakes your agent only on message.

- **Rust sidecar** (`pc` binary) owns the provider connections (Telegram long-poll `crates/provider-telegram/src/lib.rs`, Discord Gateway WS `crates/provider-discord/src/gateway.rs`). Fixes the ~400 MB idle-RAM trap of JS SDKs `docs/architecture.md:5`.
- **Transports**: `stdio` NDJSON `crates/provider-transport/src/stdio.rs:18` (default, lean), `WebSocket`+`HTTP` `pc serve` `crates/provider-transport/src/http.rs:11` — any language via `POST /api/providers/:id/send` + `GET /api/events` SSE.
- **Headless TS** (`@mbsks/side-pigeon`) spawns sidecar over stdio, `initialize→listen→send→shutdown` (`event.message`/`event.error` `crates/provider-transport/src/jsonrpc.rs:194`).
- **Plugins**: `Opencode`/`Pi` reuse the same sidecar `plugins/`.

Prerelease `0.1.0`.

## What can you do — start here

| Need | How | Doc |
|---|---|---|
| Send/receive from any app (Node/Bun/Python/Go/Dart) | `pc serve --http 127.0.0.1:8788` + `fetch`/`curl` | [docs/app-integration.md](docs/app-integration.md), [docs/guides/polyglot.md](docs/guides/polyglot.md) |
| Two bots same platform (tg-main + tg-ops) | `pc.config.json` alias `id` + `config.kind` | [docs/guides/multi-bot-routing.md](docs/guides/multi-bot-routing.md) |
| Wake any script on message (no Rust) | `curl -N /api/events` SSE -> `Popen hermes` | [docs/guides/spawn-script.md](docs/guides/spawn-script.md) |
| Hermes 0 MB idle, spin on message, kill after 5m | `pc serve` stays, watcher spawns, `--idle-kill 300` | [docs/guides/hermes-on-demand.md](docs/guides/hermes-on-demand.md), [docs/guides/idle-autokill.md](docs/guides/idle-autokill.md) |
| Human-like 2m delay, coalesce 3 msgs, `/now` instant | `DebouncePlugin delay 120s` | [docs/guides/human-delay.md](docs/guides/human-delay.md) |
| Config JSON / JSONC / TOML / Lua | `pc.config.{json,jsonc,toml,lua}` + `PC_*` env | [docs/guides/config-formats.md](docs/guides/config-formats.md) |
| Repair/docs & security/quality | Features vs fixes vs polish split | [docs/IMPROVEMENTS.md](docs/IMPROVEMENTS.md), [docs/SECURITY.md](docs/SECURITY.md), [docs/POLISH.md](docs/POLISH.md) |

## Install

```sh
# sidecar — daemon includes durable SQLite log by default
cargo install --path bin/pc
cargo install --path bin/pc --features telegram,discord,http,ws

# lean build with no SQLite (library embed or minimal install)
cargo install --path bin/pc --no-default-features --features demo

# headless JS
bun add @mbsks/side-pigeon
```

Requires **Rust 1.97.1** and **Bun ≥ 1.4 / Node ≥ 20**. `bun` is the package manager.

## Quick start

```sh
pc --help
pc init                 # writes pc.config.json if none exists
pc check                # initialize + smoke-check configured providers
pc serve                # ws :8787 + http :8788 + stdio fan-out
pc                      # stdio sidecar (default, no subcommand)
```

One-shot (no daemon):

```sh
pc send --provider demo --chat my-room --text "hello"
pc listen --once --timeout 10
```

Headless TS (5 lines):

```ts
import { createProviderClient } from "@mbsks/side-pigeon";
const pc = createProviderClient({ providers: [{ id: "demo" }], pcBin: "pc" });
await pc.start();
pc.subscribe({}, (msg) => console.log(msg));
await pc.send({ provider: "demo", channelId: "my-room", text: "hello" });
```

## Config

File (`pc.config.json` or `pc.config.ts`) or env:

```json
{ "providers": [{ "id": "demo" }, { "id": "telegram", "config": { "token": "123:abc" } }] }
```

```ts
// pc.config.ts (typed helper)
export default {
  providers: [{ id: "telegram", config: { token: process.env.TG_TOKEN! } }],
};
```

CLI flag `pc --config path/to/pc.config.json` overrides. Env fallback:

```
PC_PROVIDERS=demo,telegram
PC_TELEGRAM_TOKEN=123:abc
PC_TELEGRAM_CONFIG={"base_url":"https://api.telegram.org"}
PC_CONFIG=/path/to/pc.config.json
```

`PC_<ID>_CONFIG` must be a JSON object; otherwise startup fails.

## Provider matrix

| Provider | Feature   | `id`       | Transport                | Needs   |
| -------- | --------- | ---------- | ------------------------ | ------- |
| demo     | `demo`    | `demo`     | echo, no network         | nothing |
| telegram | `telegram`| `telegram` | long-poll (`getUpdates`) | `token` |
| discord  | `discord` | `discord`  | gateway WS + REST        | `token` |

HTTP/WS serving is behind `http`/`ws` features. Idle sidecar can stay stdio-only.

## `pc serve` — daemon

Holds provider connections once and fans out to many clients. Clients can disconnect and reconnect without losing data when persistence is on.

```
pc serve [--ws :8787] [--http :8788] [--persist ./pc-events.db] [--watch]

GET  /health                     -> capabilities
POST /api/providers/:id/send     -> {channel_id, text} -> receipt
GET  /api/events                 -> SSE live stream
GET  /api/events?since=CURSOR    -> replay missed events (needs --persist)
POST /rpc                        -> JSON-RPC (same as stdio/WS)
GET  /ws                         -> WS JSON-RPC (needs --features ws)
```

### Persistence (at-least-once)

Two modes:

- **Daemon** (`pc serve`): durable by default. Every `event.message` / `event.error` is appended to a local SQLite file (WAL mode) and can be replayed.
- **Library** (`provider-core` / `provider-ffi` / `provider-transport` as a dependency): no SQLite by default — you own storage.

```sh
# daemon with SQLite (default)
pc serve
# -> sqlite at ./pc-events.db, replay via ?since=

# custom path
pc serve --persist /var/lib/pc/events.db
PC_PERSIST_PATH=/var/lib/pc/events.db pc serve

# in-memory only (no replay)
pc serve --no-persist
cargo install --path bin/pc --no-default-features --features demo

# replay missed events
curl "http://localhost:8788/api/events?since=42"
curl "http://localhost:8788/api/events?since=42&limit=100"
# -> { "events": [{ "cursor": 43, "event": { "jsonrpc":"2.0","method":"event.message", ... }}], "latest_cursor": 120 }
```

SQLite is an **optional dependency** — only the `pc` binary pulls it (feature `persist`). Library crates stay lean. No system SQLite needed: the crate bundles it (`rusqlite` with `bundled`).

## Tooling

| Tool | Version | Pinned by |
| ---- | ------- | --------- |
| Rust | `1.97.1` | `rust-toolchain.toml` |
| Bun | `1.4.0` | `packageManager` + `mise.toml` |
| TypeScript | `7.0.2` | `package.json` |
| oxlint / oxfmt | `1.80/0.65` | `package.json` |

```sh
mise install         # Rust + Bun + Node
bun install
bun run lint && bun run format && bun run typecheck
cargo test
```

## Limitations

- WhatsApp / Slack / Signal / Matrix not yet implemented (planned: hand-rolled on `reqwest` + `tokio-tungstenite`).
- `pc.config.ts` `defineConfig` is a type stub; Rust loader reads JSON `crates/provider-config/src/lib.rs:53` — see [docs/guides/config-formats.md](docs/guides/config-formats.md) for TOML/JSONC/Lua.
- SQLite replay is daemon-only (`pc serve`). Stdio sidecar stays in-memory `crates/provider-transport/src/persist.rs:25`.

## Architecture

See [docs/architecture.md](docs/architecture.md) · [docs/api-contract.md](docs/api-contract.md) · [docs/persistence.md](docs/persistence.md) · [docs/supply-chain.md](docs/supply-chain.md) · [docs/IMPROVEMENTS.md](docs/IMPROVEMENTS.md) · [docs/SECURITY.md](docs/SECURITY.md) · [docs/POLISH.md](docs/POLISH.md) · [docs/phases/README.md](docs/phases/README.md).

For regular web apps/CLIs without an AI agent, see [docs/app-integration.md](docs/app-integration.md).

Guides: [hermes-on-demand](docs/guides/hermes-on-demand.md) · [multi-bot](docs/guides/multi-bot-routing.md) · [spawn-script](docs/guides/spawn-script.md) · [human-delay](docs/guides/human-delay.md) · [idle-autokill](docs/guides/idle-autokill.md) · [polyglot](docs/guides/polyglot.md) · [config-formats](docs/guides/config-formats.md).

Release profile is `LTO thin`, `panic=abort`, `strip` at `Cargo.toml:39` for binary size.

## License

MIT — see [`LICENSE`](LICENSE).
