# App integration guide — provider-connect for regular apps

> You don't need an AI agent to use provider-connect. Any Node, Bun, Python, Go, or Rust app that needs to send or receive Telegram/Discord messages can use the same Rust sidecar — and keep it lean.

**Why a sidecar?** Per-channel JS SDKs (`grammy`, `@slack/bolt`, `discord.js`, `matrix-js-sdk`, …) plus the Node runtime push a listening process to **~400 MB–1 GB idle** (see [`docs/architecture.md:6`](architecture.md) — "Fixes: a Node.js agent listening to provider events idles at ~400MB–1GB+ RAM. Target: idle RSS < 30–50 MB"). The Rust sidecar idles at **~2–30 MB** — a measured ZeroClaw foundation binary is 6.6 MB and < 5 MB idle core ([`docs/architecture.md:12`](architecture.md), [`docs/research/zeroclaw.md` §6](research/zeroclaw.md)) — because it multiplexes all providers on one tokio runtime with no V8, no WASM crypto, and no per-SDK caches.

---

## 30-second picker — which surface do I want?

| Need | Use | Reliable receive? | Install |
|---|---|---|---|
| Send-only cron / CLI / script | `pc-connect` one-shots (`send`, `listen --once`) | No — see receiving note | `cargo install --path cli --features telegram,discord` |
| Long-lived Node or Bun service | `@provider-connect/core` + `pc` child process | Yes — sidecar stays alive | `bun add @provider-connect/core` + `cargo install --path bin/pc` |
| Polyglot (Python, Go, Ruby, …) | `pc serve` HTTP + SSE | Yes — `pc serve` is the daemon | `cargo install --path bin/pc --features http,ws,telegram,discord` |
| Pure Rust service | `provider-core` crates directly | Yes — in-process | Add `provider-core` + `provider-telegram`/`provider-discord` as path/crate deps |

Quick rule: if a process is always running, receiving is reliable. If you spawn, do one thing, and exit, receiving has gaps (more below).

---

## Shared setup — config once, every surface reads it

### 1. Generate a config file

```sh
pc init                 # writes pc.config.json if none exists
cat pc.config.json
```

```json
{
  "providers": [
    { "id": "demo" },
    { "id": "telegram", "config": { "token": "123:abc" } },
    { "id": "discord",  "config": { "token": "MTIz..." } }
  ]
}
```

Or typed TS (handy in a Node repo — Rust still reads JSON, but you get autocomplete):

```ts
// pc.config.ts
import { defineConfig } from "@provider-connect/core/config";
export default defineConfig({
  providers: [{ id: "telegram", config: { token: process.env.TG_TOKEN! } }],
});
```

CLI flag overrides: `pc --config path/to/pc.config.json serve`.

### 2. Or configure entirely via environment

No file needed — every surface respects the same env contract:

```sh
PC_PROVIDERS=demo,telegram
PC_TELEGRAM_TOKEN=123:abc
PC_TELEGRAM_CONFIG={"base_url":"https://api.telegram.org","poll_interval_secs":2}
PC_DISCORD_TOKEN=MTIz...
PC_CONFIG=/path/to/pc.config.json   # alternative file pointer
```

`PC_<ID>_CONFIG` must be a JSON object; otherwise startup fails closed.

### 3. Verify

```sh
pc check                # initialize + smoke-check each provider
pc check --provider telegram --json
# {"ok":true,"protocolVersion":"0.1.0","methods":[...],"providers":[{"provider":"telegram","ok":true,...}]}
```

`demo` proves the whole pipeline locally (no network, announces on start, echoes sends). Telegram/Discord checks poll the async error slot for ~6 s — auth failures fail fast, silence means the long-poll/gateway is in flight.

Logging: `RUST_LOG=debug pc serve` (or `pc`, `pc-connect`) — logs go to stderr, stdout stays JSON.

---

## Recipes

### A. Long-lived Node / Bun service — `@provider-connect/core`

Best for Express/Fastify/Hono/Koa, workers, queue consumers — anywhere a Node/Bun process is already running.

```ts
import { createProviderClient } from "@provider-connect/core";
import { dedup } from "@provider-connect/core/plugins/dedup.js";
import { RpcError } from "@provider-connect/core/client.js";

const pc = createProviderClient({
  providers: [
    { id: "telegram", token: process.env.TG_TOKEN! },
    { id: "discord",  token: process.env.DISCORD_TOKEN! },
  ],
  plugins: [dedup({ windowMs: 5 * 60_000 })], // dedupe by message.id
  pcBin: "pc",                                // or "/usr/local/bin/pc"
  requestTimeoutMs: 10_000,
});

await pc.start();

// Subscribe — filter is { provider, channelId } or a predicate
const unsubscribe = pc.subscribe({ provider: "telegram" }, (msg) => {
  const text = msg.content.map((p: any) => p.Text ?? "[media]").join(" ");
  console.log(`[${msg.channel}:${msg.channel_id}] ${msg.sender.name}: ${text}`);
});

// Send — replies thread via replyTo
await pc.send({ provider: "telegram", channelId: "123456789", text: "hello" });
await pc.send({ provider: "telegram", channelId: "123456789", text: "reply", replyTo: "orig-msg-id" });

// Express example — POST /notify triggers a Telegram message
// app.post("/notify", async (req, res) => {
//   const receipt = await pc.send({ provider: "telegram", channelId: req.body.chatId, text: req.body.text });
//   res.json(receipt); // { message_id, ts }
// });

// Error handling — JSON-RPC codes shared by every surface
try {
  await pc.send({ provider: "telegram", channelId: "bad", text: "hi" });
} catch (err) {
  if (err instanceof RpcError) {
    // -32001 config, -32002 auth, -32003 rate-limit, -32004 protocol, -32005 network
    if (err.code === -32002) console.error("bad token", err.message);
    if (err.code === -32003) console.error("rate limited, retry after", err.data);
  }
}

// Graceful shutdown — drains pending requests, ends child stdin, SIGTERM fallback
process.on("SIGTERM", async () => {
  unsubscribe();
  await pc.shutdown();
});

// Bun alternative — adapt Bun.spawn into the same client
// import { adaptBunSpawn } from "@provider-connect/core/client.js";
// const pc = createProviderClient({ providers: [...], spawnFn: (bin, args, opts) =>
//   adaptBunSpawn(Bun.spawn([bin, ...args], { stdin: "pipe", stdout: "pipe", stderr: "inherit", env: opts.env })) });
```

Other helpers on the same client: `pc.use(plugin)`, `pc.createSendMutation({ onSuccess })`, `pc.subscribe((msg) => msg.explicitly_addressed, cb)` (predicate form), `await using pc = createProviderClient(...)` via `[Symbol.asyncDispose]`. See `packages/core/src/provider-client.ts` and `packages/core/src/plugins/dedup.ts`.

### B. Cron / CLI / one-shot — `pc-connect` via `execFile`

Best for nightly jobs, deploy hooks, or scripts that only need to **send**.

```js
// cron.mjs — Node sends one Telegram alert without a daemon
import { execFile } from "node:child_process";
import { promisify } from "node:util";
const exec = promisify(execFile);

const receipt = await exec("pc-connect", ["send", "--provider", "telegram", "--chat", "123456789", "--text", "build is green"], {
  env: { ...process.env, PC_PROVIDERS: "telegram", PC_TELEGRAM_TOKEN: process.env.TG_TOKEN },
});
console.log(JSON.parse(receipt.stdout)); // { message_id, ts }

// One-shot receive (ad-hoc poll) — exits after first event or --timeout
const polled = await exec("pc-connect", ["listen", "--providers", "telegram", "--once", "--json", "--timeout", "30"], {
  env: { ...process.env, PC_PROVIDERS: "telegram", PC_TELEGRAM_TOKEN: process.env.TG_TOKEN },
});
for (const line of polled.stdout.trim().split("\n")) {
  const evt = JSON.parse(line); // { event:"message", message:{ id, channel, channel_id, content, sender, ts } }
  if (evt.event === "error") console.error(evt.error); // { provider, code, message }
}
```

Shell equivalent:

```sh
pc-connect send --provider telegram --chat 123456789 --text "build is green"
pc-connect listen --providers telegram --once --json --timeout 30
echo "long body" | pc-connect send --provider telegram --chat 123456789 --text-file -
PC_PROVIDERS=demo pc-connect check --json   # smoke test with no credentials
```

On failure `pc-connect send` exits non-zero and prints `{"error":{"code":-32002,"message":"..."}}` on stdout (same `-3200x` codes as the sidecar).

### C. Polyglot / Python / Go / any HTTP client — `pc serve`

Best when the app is not Node — run `pc serve` once and talk HTTP/SSE from any language.

```sh
pc serve --http :8788 --ws :8787
# GET  /health                    -> capabilities (+ transport list)
# POST /api/providers/:id/send    -> SendReceipt
# GET  /api/events               -> SSE fan-out (event.message / event.error)
# POST /rpc                       -> JSON-RPC batch (same as stdio/WS)
```

```sh
# Send
curl -s -X POST http://localhost:8788/api/providers/telegram/send \
  -H 'content-type: application/json' \
  -d '{"channel_id":"123456789","text":"hello from cron","reply_to":null,"attachments":[]}'

# Health (k8s / pc check equivalent)
curl -s http://localhost:8788/health | jq .

# Stream events via SSE (one line per event)
curl -N http://localhost:8788/api/events
# event: message
# data: {"id":"...","channel":"telegram","channel_id":"...","content":[{"Text":"hi"}],"sender":{...},"ts":...}
```

```js
// Node/Bun fetch variant — no SDK needed
const receipt = await fetch("http://localhost:8788/api/providers/telegram/send", {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ channel_id: "123456789", text: "hello" }),
}).then((r) => r.json());

// SSE consume
const res = await fetch("http://localhost:8788/api/events");
for await (const chunk of res.body!) { /* parse SSE frames */ }
```

```python
# Python variant
import requests, json, sseclient  # or plain http.client
requests.post("http://localhost:8788/api/providers/telegram/send",
              json={"channel_id": "123456789", "text": "hello"})
```

`pc serve` holds provider connections once and fans out to any number of HTTP/stdio/WS clients — agents can connect/disconnect without provider reconnect (broadcast at `crates/provider-transport/src/state.rs:43`).

### D. Pure Rust — `provider-core` crates

Best when the app is already Rust and you want in-process providers (no sidecar binary).

```rust
use provider_core::{ChatProvider, SendMessage, SendReceipt, ProviderError, ChannelMessage};
use async_trait::async_trait;

struct MyTelegram { token: String }

#[async_trait]
impl ChatProvider for MyTelegram {
    fn id(&self) -> &'static str { "telegram" }
    async fn start(&mut self) -> Result<(), ProviderError> { /* connect long-poll */ Ok(()) }
    async fn stop(&mut self) -> Result<(), ProviderError> { Ok(()) }
    async fn send(&self, msg: &SendMessage) -> Result<SendReceipt, ProviderError> {
        // POST /bot{token}/sendMessage — see crates/provider-telegram for the real impl
        Ok(SendReceipt { message_id: "123".into(), ts: 0 })
    }
}
```

Wire schema and error taxonomy are the contract — see [`docs/api-contract.md`](api-contract.md) and `crates/provider-core/src/schema.rs`. Providers hand inbound `ChannelMessage`s to the transport via `ProviderEvents::on_message`. Feature-gate providers at compile time (`telegram`, `discord`, `http`, `ws`, `demo`) so a Telegram-only binary stays ~3.3 MB stripped (demo-only is ~1.1 MB; see `cli/README.md`).

---

## Receiving semantics — when messages can be lost

`pc-connect` and `pc_msg.py --poll` are **short-lived processes**: they connect, do one job, and exit. Receiving only works **while a listener is running**. If you need reliable receiving, keep a single `pc` or `pc serve` process alive.

| Provider | Mechanism | While nothing listens | Short `pc-connect listen` runs | Reliable receiving |
|---|---|---|---|---|
| `demo` | local echo (announces on start) | n/a — per-process fixture | full — safe for tests | local only |
| `telegram` | Bot API `getUpdates` long-poll (in-memory offset cursor) | queued ~24 h by Telegram | catch-up delivers queued messages, **but** every fresh process resets its cursor → recent messages can be re-delivered — **dedupe by `message.id`** | keep one sidecar alive so the cursor advances continuously |
| `discord` | Gateway v10 WebSocket | **lost** — gateway only delivers while connected; no replay | receives only while running; anything sent while down is gone | sidecar must run **continuously** (gateway + reconnect) |
| `pc serve` SSE | `broadcast::Sender<Outbound>` fan-out | in-memory only (no sqlite replay yet) | SSE is live — no backlog on connect | consumers must stay connected |

Practical dedupe:

```ts
import { dedup } from "@provider-connect/core/plugins/dedup.js";
pc.use(dedup({ windowMs: 5 * 60_000, maxRecent: 2000 }));
// or manual: const seen = new Set<string>(); if (seen.has(msg.id)) return;
```

Also watch `event.error` — e.g. Telegram 401 or Discord gateway close `4004` means the stream is dead; don't sit waiting forever. `cli/README.md` and `plugins/agent-skill/README.md` document the same matrix — this section is the consolidated version.

---

## Config, deployment, and ops

- **Config file** — `pc.config.json` (or `pc.config.ts` with `defineConfig` for DX — Rust still reads JSON; TS helper is in `@provider-connect/core/config`). Env always wins for secrets.
- **Daemon** — run `pc serve` under systemd/launchd/supervisord so it restarts on crash. Persistence is currently in-memory only; sqlite at-least-once replay is planned (see `docs/phases/08-ffi-daemon.md`).
- **Logging** — `RUST_LOG=info pc serve` (or `debug`/`trace`); logs go to stderr, stdout stays JSON/SSE. Same for `pc` and `pc-connect`.
- **Binary size** — stripped release: ~1.1 MB (`demo` only), ~3.3 MB (`demo+telegram+discord`) per `cli/README.md`; ZeroClaw foundation measures 6.6 MB — comfortably under the 30–50 MB idle RSS budget.
- **Transports** — stdio is default (`pc` with no subcommand); `http`/`ws` are feature-gated and only built when you pass `--features http,ws` so the idle sidecar stays lean.
- **Security** — the sidecar never does policy/allowlists — that's the app's job. `explicitly_addressed` and `passive_context` on `ChannelMessage` are hints the app can use.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `pc check` fails, `code -32002 auth` | bad/expired token, Discord `MESSAGE_CONTENT` intent off | verify `PC_<ID>_TOKEN`, re-check provider portal intents |
| `code -32003 rate limited` | Telegram 429 / Discord 429 | respect `retry_after` in `error.data`; back off |
| `code -32001 config` | `PC_<ID>_CONFIG` not a JSON object, or `PC_PROVIDERS` mismatch | `echo $PC_TELEGRAM_CONFIG | jq .` — must be an object |
| `event.error` on `pc-connect listen` / SSE | provider async error (401, gateway close 4004, network) | treat as fatal for that stream — restart the listener; with `@provider-connect/core` the `provider-error` event fires and `Plugin.onError` sees it |
| Telegram re-delivers old messages | cursor reset on fresh process | dedupe by `message.id` (`dedup` plugin, or a `Set`) |
| Discord messages missing | nothing was listening | keep `pc`/`pc serve` running continuously |

Wire codes are shared across every surface (`pc` stdio, `pc-connect`, `pc serve` HTTP/SSE) — see [`docs/api-contract.md`](api-contract.md) for the full method/error contract.

---

## Links

- Contract — [`docs/api-contract.md`](api-contract.md) (schema, `ChatProvider` trait, JSON-RPC + HTTP surfaces)
- Architecture — [`docs/architecture.md`](architecture.md) (sidecar rationale, memory budget, crate layout)
- Research — [`docs/research/zeroclaw.md`](research/zeroclaw.md) (ZeroClaw blueprint, ~400 MB analysis)
- Supply chain — [`docs/supply-chain.md`](supply-chain.md) (14-day `created_at` gate, audited closure)
- Phases — [`docs/phases/README.md`](phases/README.md) (two surfaces: headless lib vs `pc serve` bod server; build order)
- CLI contract — [`cli/README.md`](../cli/README.md) (one-shot `send`/`listen`/`check`, data-loss warning)
- Agent skill matrix — [`plugins/agent-skill/README.md`](../plugins/agent-skill/README.md) (receive matrix, backend selection)
- Node example — [`examples/node/README.md`](../examples/node/README.md) + [`examples/node/index.mjs`](../examples/node/index.mjs) (raw stdio JSON-RPC, memory demo)
- Core source — [`packages/core/src/index.ts`](../packages/core/src/index.ts), [`packages/core/src/client.ts`](../packages/core/src/client.ts), [`packages/core/src/provider-client.ts`](../packages/core/src/provider-client.ts), [`packages/core/src/transports/stdio.ts`](../packages/core/src/transports/stdio.ts)
