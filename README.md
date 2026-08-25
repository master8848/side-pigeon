# side-pigeon

> **side-pigeon** — Rust sidecar + headless JS that connects any Node/Bun app or AI agent to messaging providers (Telegram, Discord, Slack, ...) over a clean JSON-RPC 2.0 API.

- **Rust sidecar** (`pc` binary) owns the provider connections (Telegram long-poll, Discord Gateway WS, ...). Idle RSS target **< 30–50 MB** (fixes the ~400 MB idle-RAM trap of JS SDKs).
- **Headless TS** (`@mbsks/side-pigeon`) spawns that sidecar over **stdio NDJSON**, speaks `initialize → listen → send → shutdown` (`event.message`/`event.error`).
- **Transports**: stdio (default, lean), WebSocket and HTTP (`pc serve`).
- **Plugins**: TypeScript extensions for **Opencode** and **Pi** that reuse the same sidecar.

Prerelease `0.1.0` — see [`docs/architecture.md`](docs/architecture.md).

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
- `pc.config.ts` `defineConfig` is a type stub; Rust loader reads JSON.
- SQLite replay is daemon-only (`pc serve`). Stdio sidecar stays in-memory.

## Architecture

See [docs/architecture.md](docs/architecture.md) · [docs/api-contract.md](docs/api-contract.md) · [docs/supply-chain.md](docs/supply-chain.md) · [docs/phases/README.md](docs/phases/README.md).

For regular web apps/CLIs without an AI agent, see [docs/app-integration.md](docs/app-integration.md).

Release profile is `LTO thin`, `panic=abort`, `strip` at `Cargo.toml:39` for binary size.

## License

MIT — see [`LICENSE`](LICENSE).
