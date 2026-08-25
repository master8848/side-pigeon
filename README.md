# side-pigeon

> **side-pigeon** — Rust sidecar + headless JS that connects any Node/Bun app or AI agent to messaging providers (Telegram, Discord, Slack, ...) over a clean JSON-RPC 2.0 API.

- **Rust sidecar** (`pc`/`side-pigeon` binary) owns the provider connections (Telegram long-poll, Discord Gateway WS, ...). Idle RSS target **< 30–50 MB** (fixes the ~400 MB idle-RAM trap of JS SDKs).
- **Headless TS** (`@mbsks/side-pigeon`) spawns that sidecar over **stdio NDJSON**, speaks `initialize → listen → send → shutdown` (`event.message`/`event.error`).
- **Transports**: stdio (primary, default lean), WebSocket and HTTP (feature-gated `pc serve`).
- **Plugins**: local TypeScript extensions for **Opencode** and **Pi** that reuse the same sidecar.

Status: implementation in progress (see [`docs/architecture.md`](docs/architecture.md)). Prerelease `0.1.0`.

## Install

```sh
# sidecar (lean stdio by default; add providers as features)
cargo install --path bin/pc          # binary `pc`
cargo install --path bin/pc --features telegram,discord,http,ws

# headless JS (any agent or plain app)
bun add @mbsks/side-pigeon
```

Requires **Rust 1.97.1** (see `rust-toolchain.toml` / `mise.toml`) and **Bun ≥ 1.4 / Node ≥ 20**.

`bun` is the package manager — `npm` also works but we commit `bun.lockb`.

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

Headless TS (any Node/Bun process, 5 lines):

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
PC_TELEGRAM_CONFIG={"base_url":"https://api.telegram.org","poll_interval_secs":2}
```

`PC_<ID>_CONFIG` must be a JSON object; otherwise startup fails closed.

Also `PC_CONFIG=/path/to/pc.config.json`.

## Provider matrix

| Provider | Feature flag     | `id`       | Transport                | Needs   |
| -------- | ---------------- | ---------- | ------------------------ | ------- |
| demo     | `demo` (default) | `demo`     | echo, no network         | nothing |
| telegram | `telegram`       | `telegram` | long-poll (`getUpdates`) | `token` |
| discord  | `discord`        | `discord`  | gateway WS + REST        | `token` |

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

## Tooling

| Tool            | Version     | How pinned                     |
| --------------- | ----------- | ------------------------------ |
| Rust            | `1.97.1`    | `rust-toolchain.toml`          |
| Bun             | `1.4.0`     | `packageManager` + `mise.toml` |
| Node (fallback) | `22.22.2`   | `mise.toml` + `engines.node`   |
| TypeScript      | `7.0.2`     | `package.json#devDependencies` |
| oxlint / oxfmt  | `1.80/0.65` | `package.json#devDependencies` |

```sh
mise install         # installs Rust + Bun + Node
bun install          # installs JS deps (writes bun.lockb)
bun run lint         # oxlint --type-aware .
bun run format       # oxfmt --check .
bun run typecheck    # tsc -p <each tsconfig> --noEmit
cargo test           # Rust tests
```

Config files: [`.oxlintrc.json`](.oxlintrc.json), [`.oxfmtrc.json`](.oxfmtrc.json).

## Limitations

- WhatsApp/Slack/Signal/Matrix not yet implemented (hand-rolled on `reqwest` + `tokio-tungstenite` per architecture).
- `pc serve` persistence is in-memory only (no sqlite at-least-once replay yet; see `docs/phases/08-ffi-daemon.md`).
- `pc.config.ts` `defineConfig` helper is a type stub; Rust loader reads JSON.

## Architecture

See [docs/architecture.md](docs/architecture.md) · [docs/api-contract.md](docs/api-contract.md) · [docs/supply-chain.md](docs/supply-chain.md) · [docs/phases/README.md](docs/phases/README.md).

For regular web apps/CLIs without an AI agent, see [docs/app-integration.md](docs/app-integration.md).

Release profile is `LTO thin`, `panic=abort`, `strip` at `Cargo.toml:39` for binary size.

## License

MIT — see [`LICENSE`](LICENSE).
