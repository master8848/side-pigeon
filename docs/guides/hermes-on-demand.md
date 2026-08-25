# Hermes on-demand — keep Rust alive, wake the agent per message

> **Problem:** Hermes (the LLM agent) is heavy — hundreds of MB to GB idle when it holds provider connections and model context. The Rust sidecar `pc` is tiny (~10–30 MB idle, `docs/architecture.md:7`, `docs/research/zeroclaw.md:260`) and is the only process that should hold the Telegram offset cursor and the Discord gateway session durably. Let `pc serve` stay alive forever; keep Hermes cold and wake it once per inbound message.

This guide shows the concrete pattern: a small Rust-only watcher holds the connections, and Hermes sleeps until there is work.

---

## Architecture

```
                Telegram long-poll          Discord gateway (WS)
                (offset cursor)             (intents + resume)
                         \                       /
                          v                     v
                    ┌──────────────┐
                    │  pc serve    │  Rust daemon, idle ~10-30 MB
                    │  :8788 HTTP  │  holds offset / gateway session
                    │  :8787 WS    │  fans out event.message + event.error
                    └──────┬───────┘
                           │  SSE  GET /api/events   or  WS  (broadcast, cap 32 — state.rs:43)
                           │  future: --on-message-exec / --on-message-webhook
                           v
                    ┌──────────────┐
                    │   watcher    │  tiny Node/Python (~5-15 MB), no LLM
                    │  (10 lines)  │  subscribes, dedups by message.id,
                    └──────┬───────┘  maps chatKey -> session, spawns Hermes
                           │  spawn on event.message
                           v
                    ┌──────────────┐
                    │   Hermes     │  COLD until woken; runs per message
                    │  (agent)     │  --resume <session> --text <prompt>
                    └──────┬───────┘
                           │  reply via pc
                           v
                 pc send / POST /api/providers/:id/send -> Telegram/Discord
```

**Idle cost when nothing is happening:** `pc` ~10–30 MB. Watcher ~5–15 MB. Hermes 0 MB (not running). Compare to holding Telegram + Discord inside the JS/TS agent process: ~400 MB–1 GB+ (`docs/architecture.md:7`).

**Where this is implemented today:** `pc serve` is the always-on daemon with SSE/WS fan-out (`bin/pc/src/main.rs:999` `run_serve`, `crates/provider-transport/src/http.rs`, `crates/provider-transport/src/ws.rs`, `crates/provider-transport/src/state.rs`). It does not yet have a built-in `exec`/`webhook` wake hook — that is Option 2 below.

> **Note on naming:** Pi (`plugins/pi-plugin/`) is the base that many agents build on. Hermes is a separate agent in this story — same pattern applies to any agent you want to keep cold.

---

## Prerequisites

Build `pc` with the providers you need:

```bash
cargo build -p pc --features telegram,discord
# binary: target/debug/pc  (or target/release/pc with --release)

# Or run directly:
cargo run -p pc --features telegram,discord -- serve --http :8788 --ws :8787
```

Configure providers (any one works — env, config file, or both):

```bash
# env (prefix PC_<PROVIDER>_TOKEN, plus PC_PROVIDERS list):
export PC_PROVIDERS=telegram,discord
export PC_TELEGRAM_TOKEN=123456:ABC...
export PC_DISCORD_TOKEN=MTIz...

# or JSON file (pc.config.json) — same shape provider_config::load expects:
# { "providers": [{ "id": "telegram", "config": { "token": "..." } }] }
pc --config pc.config.json serve --http :8788 --ws :8787

# verify:
curl -s http://localhost:8788/health | jq .
# -> { protocolVersion, methods, notifications: ["event.message","event.error"], transport, providers }
# (shape is AppState::capabilities_value(), crates/provider-transport/src/state.rs:203)
```

See `docs/api-contract.md:54` for the full HTTP surface and `cli/README.md` for the data-loss matrix (why a daemon is required).

---

## Option 1 — Works today (no Rust change)

Run `pc serve` under a supervisor, and add a tiny watcher that subscribes to `GET /api/events` (SSE) or WS and spawns Hermes per message.

### 1a. Keep `pc serve` alive

`pc serve` binds both transports by default (`:8787` WS, `:8788` HTTP) and fans out every `event.message` via `broadcast::Sender<Outbound>` (`state.rs:17` capacity 32 — tuned for chat rate, not 512× memory). Use any supervisor:

```ini
# /etc/systemd/system/pc.service
[Unit]
Description=provider-connect sidecar
After=network.target

[Service]
ExecStart=/usr/local/bin/pc serve --http :8788 --ws :8787 --config /etc/pc/pc.config.json
Restart=always
RestartSec=1
Environment=RUST_LOG=info
# tokens via EnvironmentFile= or PC_* env — never commit them

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload && sudo systemctl enable --now pc
curl -s http://localhost:8788/health | jq .providers
```

On macOS use `launchd` (`~/Library/LaunchAgents/ai.pc.plist` with `KeepAlive: true`), same `ProgramArguments`.

### 1b. Watcher — Node (SSE)

Copy-paste `watcher.mjs`. Pattern mirrors `examples/node/index.mjs:64` `JsonRpcClient` but over SSE (simpler than WS for a wake hook):

```js
// watcher.mjs — Node 18+, zero deps (uses global fetch)
const SSE_URL = process.env.PC_SSE_URL || "http://localhost:8788/api/events";
const seen = new Set(); // dedup by message.id (reconnects may replay last frame)

function chatKey(provider, chatId) { return `${provider}:${chatId}`; }
// same key as plugins/opencode-plugin/src/session-map.ts:24
// Pi's Python equivalent: sanitize_component() in plugins/pi-plugin/pc_connect.py:129

import { spawn } from "node:child_process";

async function handleMessage(msg) {
  if (seen.has(msg.id)) return;
  seen.add(msg.id);
  const key = chatKey(msg.channel, msg.channel_id);
  const text = (msg.content || []).map(p => p.Text ?? "").join(" ").trim();
  const session = `sessions/${key.replace(/[^A-Za-z0-9._-]/g, "_")}.jsonl`;
  console.log(`[watcher] ${key} id=${msg.id} -> hermes --resume ${session}`);

  // Replace this spawn with your Hermes entrypoint:
  spawn("hermes", ["run", "--session", session, "--text", text], { stdio: "inherit" });
}

async function watch() {
  for (;;) {
    try {
      const res = await fetch(SSE_URL, { headers: { Accept: "text/event-stream" } });
      if (!res.ok) throw new Error(`SSE ${res.status}`);
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        let idx;
        while ((idx = buf.indexOf("\n\n")) !== -1) {
          const chunk = buf.slice(0, idx); buf = buf.slice(idx + 2);
          for (const line of chunk.split("\n")) {
            if (!line.startsWith("data: ")) continue;
            const evt = JSON.parse(line.slice(6));
            if (evt.method === "event.message") await handleMessage(evt.params.message);
            if (evt.method === "event.error") console.error("[watcher] event.error", evt.params);
          }
        }
      }
    } catch (e) {
      console.error("[watcher] SSE error, retrying in 1s:", e.message);
      await new Promise(r => setTimeout(r, 1000));
    }
  }
}
watch();
```

Run: `node watcher.mjs` (or `systemd`/`pm2` it alongside `pc`).

### 1c. Watcher — Python (SSE)

```python
# watcher.py — Python 3.10+, stdlib only (urllib)
import json, os, re, subprocess, time, urllib.request

SSE_URL = os.environ.get("PC_SSE_URL", "http://localhost:8788/api/events")
seen = set()

def sanitize(s, max_len=80):
    # same as plugins/pi-plugin/pc_connect.py:129 sanitize_component
    safe = re.sub(r"[^A-Za-z0-9._-]", "_", str(s)).strip("._")
    return (safe[:max_len] or "default")

def chat_key(provider, chat_id): return f"{provider}:{chat_id}"

def handle(msg):
    mid = msg.get("id")
    if mid in seen: return
    seen.add(mid)
    key = chat_key(msg.get("channel"), msg.get("channel_id"))
    text = " ".join(p.get("Text","") for p in (msg.get("content") or []) if isinstance(p, dict))
    session = f"sessions/pc-{sanitize(msg.get('channel'))}-{sanitize(msg.get('channel_id'))}.jsonl"
    print(f"[watcher] {key} id={mid} -> hermes --resume {session}")
    subprocess.Popen(["hermes", "run", "--session", session, "--text", text])

while True:
    try:
        with urllib.request.urlopen(SSE_URL) as r:
            buf = ""
            while True:
                chunk = r.read(1024).decode("utf-8", errors="replace")
                if not chunk: break
                buf += chunk
                while "\n\n" in buf:
                    frame, buf = buf.split("\n\n", 1)
                    for line in frame.splitlines():
                        if not line.startswith("data: "): continue
                        evt = json.loads(line[6:])
                        if evt.get("method") == "event.message":
                            handle(evt["params"]["message"])
    except Exception as e:
        print(f"[watcher] SSE error, retrying: {e}")
        time.sleep(1)
```

### Tips

- **Session mapping:** reuse `chatKey` as `${provider}:${chatId}` (`plugins/opencode-plugin/src/session-map.ts:24`) or `sanitize_component` (`plugins/pi-plugin/pc_connect.py:129`). Store each chat's Hermes session at `sessions/<sanitized-key>.jsonl` and resume it.
- **Dedup:** always gate on `message.id` — SSE/WS reconnects can re-deliver the fan-out's last frame; the transport is in-memory only (see Limitations).
- **WS variant:** swap `fetch(SSE_URL)` for a `WebSocket("ws://localhost:8787")` and reuse the WS dispatch loop in `examples/node/index.mjs:140` (`event.message` / `event.error`). Either transport works — SSE is simpler for “just wake me”.

---

## Option 2 — Future built-in hook (spec, not yet implemented)

Goal: remove the external watcher entirely. `pc serve` would wake Hermes itself.

```bash
# Spec — not implemented yet (tracked via docs/phases/08-ffi-daemon.md):
pc serve --http :8788 --ws :8787 \
  --on-message-exec "hermes handle --stdin" \
  --on-message-webhook http://127.0.0.1:8789/hook

# With persistence (at-least-once replay):
pc serve --http :8788 --ws :8787 \
  --on-message-exec "hermes handle --stdin" \
  --sqlite /var/lib/pc/events.db
# then: GET /api/events?since=<cursor>  replays missed frames
```

**Why this matters per provider:**

- **Telegram:** `getUpdates` long-poll needs a durable offset cursor. Only the daemon holds it. If nothing holds it, updates are lost after Telegram's short queue window (`cli/README.md` data-loss matrix). A built-in exec hook means no gap between `pc` receiving and Hermes waking.
- **Discord:** gateway WS must stay connected — `pc` holds the resume session. No daemon = no events, and session state is not persisted across invocations (`cli/README.md`). Exec/webhook inside `pc` closes the race between “daemon got it” and “watcher saw it”.

**Backpressure note:** `state.rs:43` caps the broadcast at 32 frames (deliberately small — 512 would be ~512 KB idle overhead per connection). When a consumer lags, `pc` emits `event.error` with `dropped_frames_notification` (`state.rs:365`, code `-32006`) instead of growing memory. With `sqlite` persistence (`docs/phases/08-ffi-daemon.md`) the daemon can replay `?since=cursor`; without it, a lag means honest loss — another reason to keep the watcher tiny and fast.

---

## Choosing between the options

| Approach | Latency | Reliability | Ops | When to use |
|---|---|---|---|---|
| **Option 1 — external SSE watcher** (today) | ~10–50 ms added (HTTP + spawn) | At-most-once (in-memory fan-out; gaps if watcher down) | Two units: `pc` + watcher | Default. No Rust change. Works now. |
| **Option 1 — external WS watcher** | similar, slightly lower overhead | At-most-once (same) | Two units | You already speak WS (see `examples/node/index.mjs`) |
| **Option 2 — `--on-message-exec`** (future) | lowest (no HTTP hop) | At-least-once with `--sqlite` queue | One unit: `pc` wakes Hermes | You need no-gap wake + single supervisor |
| **Option 2 — `--on-message-webhook`** (future) | + HTTP RTT to your hook server | At-least-once with sqlite | `pc` + hook server | Hermes already has an HTTP hook endpoint |
| **Socket activation** (systemd `Accept=yes` / launchd) | similar to exec | At-most-once | OS-managed | You want the OS to spawn Hermes on demand |

Rule of thumb: start with Option 1 SSE watcher (10 lines), move to Option 2 when the `sqlite` replay lands.

---

## Hermes session routing

Give every `(provider, chat_id)` pair a stable session so Hermes resumes the same conversation:

```bash
# deterministic path per chat (sanitize like pi-plugin does):
#   sanitize_component(provider) + sanitize_component(chat_id)
#   -> sessions/pc-telegram-123456.jsonl
ls sessions/
# pc-telegram-123456.jsonl
# pc-discord-987654321.jsonl
```

Resume pattern (adapt to your Hermes CLI):

```bash
hermes --resume sessions/pc-telegram-123456.jsonl --text "hello from Telegram chat 123456"
hermes --resume sessions/pc-discord-987654321.jsonl --text "hello from Discord channel 987654321"
```

Reference implementations:

- **TypeScript:** `chatKey()` + `SessionMap` (`plugins/opencode-plugin/src/session-map.ts:24`) — `chatKey = \`${provider}:${chatId}\``, persisted as `{ version: 1, chats: { [key]: { sessionID, title, ... }}}`.
- **Python:** `sanitize_component()` + `session_file_for()` (`plugins/pi-plugin/pc_connect.py:129`, `:135`) — `pc-<sanitized-provider>-<sanitized-channel>.jsonl`.
- **Skill routing:** `plugins/agent-skill/pc_msg.py` `forward`/`resolve` + `sessions.json` schema — maps chat ids to agent sessions for any agent.

Hermes can adopt any of these; the only contract is: **same `chatKey` -> same file/dir -> `--resume` that file**.

---

## Sending replies

Hermes replies through `pc` so the daemon reuses the same provider connections (and rate-limit / retry state).

**CLI one-shot (simplest from Hermes):**

```bash
pc send --provider telegram --chat 123456 --text "hello back" --reply-to 789
# alternative: pipe long text
echo "long reply..." | pc send --provider telegram --chat 123456 --text-file -
```

Or via the transport core (`provider_core::send`) if Hermes links `pc` as a library.

**HTTP (from any language, next to `pc serve`):**

```bash
# POST /api/providers/:id/send  (docs/api-contract.md:54)
curl -s http://localhost:8788/api/providers/telegram/send \
  -H 'content-type: application/json' \
  -d '{"channel_id":"123456","text":"hello back","reply_to":"789"}' | jq .
# -> { "message_id": "...", "ts": 1710000000000 }
```

---

## Deployment

### Health check

```bash
curl -s http://localhost:8788/health | jq .
# capabilities_value() — providers, methods, transport, features
# Use for k8s liveness/readiness, systemd ExecStartPost, or pc check
```

### Systemd / launchd

- `pc serve` is the long-lived unit (`Restart=always`). Watcher is a second unit with `After=pc.service` + `Requires=pc.service` (or co-located in the same process manager).
- Logs: `pc` logs to stderr; control verbosity with `RUST_LOG=info|debug|trace` (`bin/pc/src/main.rs:245` `init_tracing`).

### `pc serve --watch` / `pc dev` (stub today)

`--watch` currently polls `pc.config.*` mtimes every 1 s (`bin/pc/src/main.rs:1111`) and logs `watch: ... changed — restart pc to apply` — it does **not** hot-reload providers yet. `pc dev` is an alias for `serve --watch`. For now, restart `pc` to pick up config changes.

---

## Limitations today

- **In-memory fan-out only:** `GET /api/events` SSE replays nothing after a disconnect; missed frames become `event.error` (`state.rs:365` `dropped_frames_notification`, code `-32006`). No `?since=cursor` replay until the `sqlite` queue lands (`docs/phases/08-ffi-daemon.md`, `README.md:109`).
- **`--watch` is a stub:** mtime poll, no hot-reload; restart `pc` to apply `pc.config.*` edits.
- **One-shot `pc send` / `pc listen` gap windows:** `pc send` and `pc listen` (`cli/README.md` data-loss matrix) each start their own provider connections and lose events while not running — that is why this guide insists on `pc serve` as the durable holder.
- **Backpressure is honest but lossy:** broadcast cap 32 (`state.rs:17`) and per-WS outbound queue 1024 (`ws.rs:67`) bound memory but close slow consumers instead of growing. Keep the watcher fast; defer heavy work to Hermes.

---

## TL;DR — copy-paste

```bash
# 1) start the Rust daemon (holds Telegram offset + Discord gateway)
cargo build -p pc --features telegram,discord
PC_PROVIDERS=telegram PC_TELEGRAM_TOKEN=... pc serve --http :8788 --ws :8787 &

# 2) start a 10-line watcher that wakes Hermes per message
node watcher.mjs &
# or: python3 watcher.py &

# 3) Hermes replies through pc (inside hermes run):
pc send --provider telegram --chat "$CHAT_ID" --text "$REPLY" --reply-to "$MSG_ID"
# or: curl -s http://localhost:8788/api/providers/telegram/send -H 'content-type: application/json' \
#       -d "{\"channel_id\":\"$CHAT_ID\",\"text\":$(jq -Rs . <<<"$REPLY"),\"reply_to\":\"$MSG_ID\"}"

# health
curl -s http://localhost:8788/health | jq .
```

Questions? See `docs/architecture.md`, `docs/api-contract.md`, `examples/node/index.mjs`, `plugins/pi-plugin/pc_connect.py:135`, and `plugins/opencode-plugin/src/session-map.ts:24`.
