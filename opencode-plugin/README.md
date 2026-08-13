# provider-connect opencode plugin

Bridges [opencode](https://opencode.ai) to messaging providers (Telegram,
Discord, and the built-in `demo` provider) through the **provider-connect
sidecar** — the Rust `pc` binary speaking JSON-RPC 2.0 over stdio.

The plugin does **not** implement any provider protocol in JavaScript. It
spawns `pc` as a child process, drives it over newline-delimited JSON-RPC
(`initialize` → `capabilities` → `listen` → `send` → `shutdown`), and:

- **inbound**: `event.message` notifications are routed to an opencode
  session per chat (`[provider] <chat-id>`); the mapping chat → session is
  persisted in plugin state so restarts keep the same session per chat.
- **outbound**: agents get a `send_message` tool; `provider_status` reports
  sidecar health and surfaces `event.error` notifications.

```
┌──────────────────────────────┐     JSON-RPC 2.0 (NDJSON)     ┌──────────────────────────┐
│  opencode (Node/Bun process) │ ◄─────────── stdout ─────────► │  pc sidecar (Rust)       │
│  ┌────────────────────────┐  │  initialize / capabilities /  │  ┌────────────────────┐  │
│  │ plugin (this package)  │  │  listen / send / shutdown     │  │ telegram / discord │  │
│  │  └─ spawns pc ────────┼──┼───────────────────────────────►│  │ (long-poll/gateway)│  │
│  │  └─ sessions per chat │  │  ◄── event.message / error ────┤  └────────────────────┘  │
│  └────────────────────────┘  │                              └──────────────────────────┘
└──────────────────────────────┘
```

Heavy listening lives in the sidecar process (Rust, ~tens of MB RSS); the
plugin process only relays events into sessions. See
[`docs/api-contract.md`](../docs/api-contract.md) for the wire contract and
[`crates/provider-transport/src/jsonrpc.rs`](../crates/provider-transport/src/jsonrpc.rs)
for the framing.

## Install

Build the sidecar first (the plugin needs `pc` on `PATH` or a configured path):

```sh
cd provider-connect
cargo build --release -p pc --features telegram,discord   # or just demo
export PATH="$PWD/target/release:$PATH"
```

Then add the plugin to your opencode config (`opencode.json`). From a local
checkout:

```json
{
  "plugin": [
    [
      "file:/absolute/path/to/provider-connect/opencode-plugin",
      {
        "providers": ["telegram"],
        "rooms": { "telegram": ["123456789"] },
        "agent": "build"
      }
    ]
  ]
}
```

For an npm install (once published), `npm i -D @provider-connect/opencode-plugin`
and use `"plugin": ["@provider-connect/opencode-plugin"]`.

## Configuration

Plugin options (second element of the `["spec", {options}]` tuple) with
environment fallbacks:

| Option | Env fallback | Default | Meaning |
| --- | --- | --- | --- |
| `pcBin` | `PC_BIN` | `pc` (on PATH) | Path to the sidecar binary |
| `pcArgs` | — | `[]` | Extra CLI args for `pc` |
| `pcConfigFile` | — | — | Sidecar JSON config file, passed as `pc -c <path>` (see `bin/pc` usage) |
| `providers` | `PC_PROVIDERS` | — | Comma-separated provider ids, e.g. `["telegram","discord"]` |
| `tokens` | `PC_<ID>_TOKEN` | — | Per-provider tokens, e.g. `{"telegram": "123:abc"}` |
| `providerConfig` | `PC_<ID>_CONFIG` | — | Extra per-provider JSON (base_url, poll_interval_secs, intents, …) |
| `rooms` | — | all chats | Allowlist per provider: `{"telegram": ["<chat-id>"]}`; empty = every chat gets a session |
| `agent` | — | default agent | Agent run for inbound messages |
| `model` | — | session default | `{"providerID": "...", "modelID": "..."}` for inbound runs |
| `stateFile` | — | `~/.local/state/opencode/provider-connect/state.json` (XDG) | Chat→session mapping persistence |
| `sessionPrefix` | — | `[<provider>] ` | Session title prefix |
| `awaitReply` | — | `false` | Wait for the agent reply on inbound (`session.prompt`) instead of fire-and-forget (`promptAsync`) |
| `ignoreSenderIds` | — | `[]` | Sender ids never routed to a session (see echo caveat) |

Tokens can live in the opencode config (`tokens`) or the environment
(`PC_TELEGRAM_TOKEN=...`). Either way they only ever reach the `pc` child
process environment — never the opencode provider/auth stores.

Example with env-only config:

```sh
export PC_BIN=/path/to/target/release/pc
export PC_PROVIDERS=telegram,discord
export PC_TELEGRAM_TOKEN=123:abc
export PC_DISCORD_TOKEN=your-bot-token
opencode
```

### Auth

No opencode provider auth is involved: the plugin hands tokens to the
sidecar, which talks to Telegram Bot API / Discord Gateway directly. There is
no `opencode auth login` step for messaging providers.

## Tools

- **`send_message`** — `{ text, provider?, chat?, replyTo? }`. Provider and
  chat default to the chat the calling session is bridged to (so an agent in
  the `[telegram] 123` session just calls `send_message` with text). `replyTo`
  threads the reply to a provider message id. Returns the provider receipt
  (`message_id`, `ts`).
- **`provider_status`** — sidecar health: running, pid, started providers,
  sessions mapped, last `event.error` notifications (also logged).

## How it behaves

- On plugin load: load the persisted chat→session map, spawn `pc`, run
  `initialize`, then `listen` with the configured providers.
- On `event.message`: skip ignored senders / duplicate ids / own echoes, check
  the `rooms` allowlist, then create (or reuse) the session for
  `provider:chat` and hand the message text over via `session.promptAsync`
  (or `session.prompt` with `awaitReply`). If the mapped session was deleted,
  it is recreated once and the message redelivered.
- On `event.error`: logged and kept in the `provider_status` ring.
- On dispose (opencode shutdown): `shutdown` request, stdin close, and a
  SIGTERM fallback if the sidecar does not exit in 2 s. A `process.on("exit")`
  hook force-kills the child if opencode dies without dispose.

## Development

```sh
npm ci            # exact-pinned deps (see package-lock.json)
npm run typecheck # tsc --noEmit
npm run lint      # eslint + prettier --check
npm test          # build + node --test (mocked pc child process)
```

The unit tests run the plugin against a **mocked `pc` child** (NDJSON
JSON-RPC over PassThrough streams, `test/helpers.mjs`), plus a config suite.
An end-to-end run against the real binary with the `demo` provider:

```sh
cargo build -p pc --features demo
PC_BIN=../../target/debug/pc node --input-type=module -e "
import { ProviderConnectRuntime } from './dist/runtime.js';
import { resolveConfig } from './dist/config.js';
// ... see test/runtime.test.mjs for the fake-client pattern
"
```

### Dependencies

| Package | Version | Why |
| --- | --- | --- |
| `@opencode-ai/plugin` | `1.18.9` (exact) | Plugin API types + `tool()` helper (zod args, shared with the opencode runtime) |
| `typescript` (dev) | `5.9.3` (exact) | Build/typecheck |
| `@types/node` (dev) | `24.13.3` (exact) | Node types for `child_process`/`readline` |
| `eslint`/`typescript-eslint`/`prettier` (dev) | exact | Lint/format gates per CONTRIBUTING |

No runtime deps beyond `@opencode-ai/plugin`; the JSON-RPC client is
hand-rolled on `node:child_process` + `node:readline` (zero extra deps, same
pattern as `examples/node/index.mjs`). All versions pinned exactly; the
lockfile is committed.

## LIMITATIONS — read before relying on this

- **In-process = best-effort.** The plugin runs inside opencode's Node/Bun
  process. If opencode is closed, crashes, or the machine sleeps, inbound
  messages stop being routed. Providers keep consuming from their platform
  while the sidecar is up, so messages can be **read and dropped** while no
  session is listening — Telegram `getUpdates` offsets advance and Discord
  gateway events are not replayed. This is a real data-loss window.
- **For reliable background receiving, use the out-of-process siblings**:
  - [`cli/`](../cli/) — the `pc-connect` CLI (spawn, send, bounded poll).
  - [`agent-skill/`](../agent-skill/) — the `pc-msg` skill/CLI for agents:
    zero-dependency Python, spawns the sidecar per command, maps chat ids to
    agent sessions, and can hand messages to `opencode run --session` /
    `prime-agent send`.
  - The sidecar itself (`pc`) is the always-on option; run it under a process
    supervisor (systemd/launchd) and point the CLI/skill at it.
- **Echo loops**: the plugin suppresses a provider's own sent messages by
  matching inbound ids against recent send receipts (works for Telegram —
  `update_id/message_id` suffix — and Discord — snowflake ids). The `demo`
  provider generates unrelated ids for echoes, so testing with `demo` requires
  `ignoreSenderIds: ["demo-bot"]` or you will see your sends echoed into the
  session.
- **Per-provider caveats** (v0.1):
  - Telegram: long-polling (1 s idle poll); messages without text/caption/media
    are skipped; attachments are media refs only (no file download); `send`
    is text-only (attachments ignored with a warning).
  - Discord: gateway v10 WebSocket; `send` is text-only in v0.1 (attachments
    ignored); no thread/channel creation.
  - `explicitly_addressed` is always `false` in v0.1 — every message in an
    allowlisted chat is routed.
- **One session per chat**: all inbound messages of a chat go to one session;
    concurrent chats each get their own session. Messages arriving while the
    session is busy are queued by opencode (or fail with a logged
    `event.error`-style entry in `provider_status` if the session rejects).
- **No media round-trip**: inbound media becomes `[media]` markers; the agent
  cannot receive files into the session (v0.1 contract).
- **No outbound history**: the plugin does not write agent replies back as
  chat history; the agent's reply is whatever it sends via `send_message`.

## Layout

```
opencode-plugin/
├── src/
│   ├── index.ts          # plugin entrypoint ({ id, server } module)
│   ├── client-adapter.ts # SDK client → minimal runtime client
│   ├── config.ts         # options + env resolution, child env
│   ├── pc-client.ts      # JSON-RPC 2.0 client over the child stdio
│   ├── runtime.ts        # lifecycle, session mapping, inbound routing, tools
│   ├── session-map.ts    # persisted chat→session mapping
│   └── format.ts         # wire content-part rendering
└── test/                 # node --test suites with a mocked pc child
```
