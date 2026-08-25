# provider-connect

Rust sidecar library + binary that connects AI agents to messaging providers
(Discord, Telegram, Slack, ...) with a clean, language-agnostic API:
JSON-RPC 2.0 over stdio (primary), WebSocket and HTTP (optional), plus direct
Rust library calls. Target: idle RSS < 30-50 MB (fixes the ~400 MB idle-RAM
problem of JS agent SDKs).

Status: implementation in progress (see [docs/architecture.md](docs/architecture.md)).

## Install

Rust sidecar:

```sh
cargo install --path bin/pc          # binary `pc` (add --features telegram,discord,http,ws)
cargo install --path bin/pc --features telegram,discord
```

Headless TS core (any Bun/Node agent):

```sh
bun add @provider-connect/core
# or: npm add @provider-connect/core
```

Requires Rust ≥1.80, Node ≥20 / Bun ≥1.0.

## Quick start

```sh
pc --help
pc init                 # writes pc.config.json if none exists
pc check                # initialize + smoke-check configured providers
pc serve                # bod server: ws :8787 + http :8788 + stdio fan-out
pc                      # stdio sidecar (default, no subcommand)
```

One-shot ops without a daemon:

```sh
pc send --provider demo --chat my-room --text "hello"
pc listen --once --timeout 10
```

Headless TS (any agent, 5 lines):

```ts
import { createProviderClient } from "@provider-connect/core";
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
import { defineConfig } from "@provider-connect/core/config"; // if exported, else plain JSON
export default defineConfig({
  providers: [{ id: "telegram", config: { token: process.env.TG_TOKEN! } }],
});
```

CLI flag `pc --config path/to/pc.config.json` overrides. Env fallback:

```
PC_PROVIDERS=demo,telegram
PC_TELEGRAM_TOKEN=123:abc
PC_TELEGRAM_CONFIG={"base_url":"https://api.telegram.org","poll_interval_secs":2}
```

`PC_<ID>_CONFIG` must be a JSON object; otherwise startup fails closed.

Also `PC_CONFIG=/path/to/pc.config.json`.

## Provider matrix

| Provider | Feature flag | `id` | Transport | Needs |
|---|---|---|---|---|
| demo | `demo` (default) | `demo` | echo, no network | nothing |
| telegram | `telegram` | `telegram` | long-poll (`getUpdates`) | `token` |
| discord | `discord` | `discord` | gateway WS + REST | `token` |

HTTP/WS serving is behind `http`/`ws` features (`pc serve`). Idle sidecar can stay stdio-only and lean.

## `pc serve` (bod server)

```
pc serve [--ws :8787] [--http :8788]
GET  /health                  -> capabilities (+ transport list)
POST /api/providers/:id/send  -> {channel_id, text, reply_to?, attachments?} -> receipt
GET  /api/events              -> SSE fan-out of event.message / event.error
POST /rpc                     -> JSON-RPC batch (same dispatch as stdio/WS)
GET  /ws                     -> WS JSON-RPC (feature `ws`)
```

Agents connect/disconnect without provider reconnect.

## Limitations

- WhatsApp/Slack/Signal/Matrix not yet implemented (hand-rolled on `reqwest` + `tokio-tungstenite` per architecture).
- `pc serve` persistence is in-memory only (no sqlite at-least-once replay yet; see `docs/phases/08-ffi-daemon.md`).
- `pc.config.ts` `defineConfig` helper is a type stub; Rust loader reads JSON.

## Architecture

See [docs/architecture.md](docs/architecture.md) · [docs/api-contract.md](docs/api-contract.md) · [docs/supply-chain.md](docs/supply-chain.md) · [docs/phases/README.md](docs/phases/README.md).

Release profile is `LTO thin`, `panic=abort`, `strip` at `Cargo.toml:31` for binary size.
