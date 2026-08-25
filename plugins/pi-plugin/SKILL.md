---
name: provider-connect
description: >-
  Connect Prime Agent to messaging providers (Telegram, Discord, demo) through the
  provider-connect `pc` sidecar. Use to check provider status (pc_check), send
  replies (pc_send), poll for inbound messages (pc_listen / pc_connect.py listen),
  and route each chat to its own Prime Agent session (pc_connect.py session /
  dispatch / bridge). Always drive the Rust `pc` sidecar as a subprocess — never
  reimplement provider APIs in Python or JS.
---

# provider-connect

Bridges Prime Agent and messaging platforms (Telegram, Discord, ...) via the
provider-connect `pc` sidecar: a small Rust process speaking JSON-RPC 2.0 over
stdio (one JSON document per line). Providers run inside `pc`; this skill only
spawns it, parses NDJSON, and routes messages.

Two surfaces:

1. **Extension tools** (if the `provider-connect` extension is installed):
   `pc_check`, `pc_send`, `pc_listen` — call them directly like any tool.
2. **`pc_connect.py`** (this skill's script, Python stdlib only):
   `check | send | listen | session | dispatch | bridge`.

Backends: the one-shot commands (`check`/`send`/`listen`) **prefer the
`pc-connect` CLI** (`cli/target/release/pc-connect`, or `$PC_CONNECT_BIN`) when
it is available — it embeds the same provider logic in one process. Otherwise
they drive the JSON-RPC `pc` sidecar (`target/debug/pc` / `$PC_BIN`).
`session`/`dispatch`/`bridge` always use the `pc` sidecar (they need one long-
lived process with `reply_to` threading).

Credentials come from the environment (`PC_TELEGRAM_TOKEN`,
`PC_DISCORD_TOKEN`, ...) or a config file (`-c path` / `$PC_CONFIG`); see
README for build instructions.

## Quick start

```bash
# provider status / which providers are compiled in
python3 <skill-dir>/pc_connect.py check --provider telegram

# send a reply (auto-starts the provider; --text or piped stdin)
python3 <skill-dir>/pc_connect.py send --provider telegram --chat 123456789 --text "Hello!"

# poll for inbound messages (30s window, stop after the first)
python3 <skill-dir>/pc_connect.py listen --provider telegram --timeout 30 --once --json

# which Prime Agent session belongs to a chat
python3 <skill-dir>/pc_connect.py session --provider telegram --chat 123456789
# -> ~/.prime/agent/sessions/pc-telegram-123456789.jsonl
```

## Sending replies

Prefer `pc_send` (extension) or `pc_connect.py send`. The sidecar requires the
provider to be started before sending; the script starts it automatically and
retries. Set `reply_to` to the inbound message id to reply in-thread.

## Receiving messages (poll, not push)

The plugin is **not a daemon**. To check for new messages you must run
`pc_listen` / `pc_connect.py listen` while the agent is running (e.g. as a
scheduled/bridged step). `listen` starts the providers and returns every
`event.message` received within the timeout:

- `--once`: stop after the first message.
- `--timeout N`: stop after N seconds (default 30).
- `--json`: machine-readable `{started, messages, errors}`.

Inbound message shape (provider-core ChannelMessage):

```json
{
  "id": "provider message id",
  "channel": "telegram",
  "channel_id": "chat id",
  "sender": {"id": "...", "name": "...", "username": "..."},
  "content": [{"Text": "hello"}, {"Media": {"kind": "Image", "caption": "..."}}],
  "reply_target": "...", "thread_ts": null,
  "explicitly_addressed": false, "ts": 1700000000000
}
```

## Which session to open per chat id

Every chat gets **one stable Prime Agent session file**:

```
<session-dir>/pc-<provider>-<sanitized chat id>.jsonl
```

(`session-dir` defaults to `~/.prime/agent/sessions`.) First contact creates
the file; later messages resume the same conversation — use
`pc_connect.py session --provider <p> --chat <id>` to print it, then open it
with:

```bash
prime-agent --resume "$(pc_connect.py session --provider telegram --chat 123456789)"
```

## Automated loop: listen -> session -> reply (`bridge`)

`pc_connect.py bridge` does the full loop in one process: listen on a
provider, for each inbound message dispatch it into the chat's session via
`prime-agent --mode rpc --resume <session-file>`, wait for the `agent_end`
event, and send the assistant text back with `reply_to` set:

```bash
python3 <skill-dir>/pc_connect.py bridge --provider telegram --timeout 300 --once
```

Single-message handoff without the loop:

```bash
python3 <skill-dir>/pc_connect.py dispatch --session "$(pc_connect.py session --provider telegram --chat 123456789)" --text "incoming: ..."
```

## Extension tools

If `extension/provider-connect.ts` is installed (`~/.prime/agent/extensions/`),
the agent can also call:

- `pc_check` — capabilities: providers, methods, features.
- `pc_send {provider, channel_id, text, reply_to?}` — send (returns receipt).
- `pc_listen {provider?, timeout_secs?, once?}` — bounded poll, returns
  formatted inbound messages.

Both surfaces share config: `$PC_BIN` (binary path), `$PC_CONFIG` (JSON config
file), `$PC_PROVIDERS` (comma list), `PC_<PROVIDER>_TOKEN`.

## Environment / config

| Variable | Meaning |
|---|---|
| `PC_BIN` | path to the `pc` sidecar binary (default: repo `target/{release,debug}/pc`, then `PATH`) |
| `PC_CONNECT_BIN` | path to the `pc-connect` CLI (default: repo `cli/target/{release,debug}/pc-connect`, `~/.local/bin`, `~/.cargo/bin`, `PATH`); preferred backend for one-shot check/send/listen |
| `PC_CONFIG` | JSON config file, e.g. `{"providers":[{"id":"telegram","config":{"token":"..."}}]}` |
| `PC_PROVIDERS` | comma-separated provider ids when no config file is given |
| `PC_TELEGRAM_TOKEN` | Telegram bot token |
| `PC_DISCORD_TOKEN` | Discord bot token |
| `PC_<ID>_CONFIG` | optional extra JSON merged into the provider config |
| `PRIME_AGENT_BIN` | path to the `prime-agent` executable used by dispatch/bridge (default `prime-agent`) |

## Limitations

- Not a daemon: messages arrive only while `listen`/`bridge` is running.
- No read receipts/seen state; each `listen` returns whatever arrived in the
  window. Long gaps can miss messages (providers buffer per their own policy).
- Sending is done by `pc`/`pc-connect` (Rust), never reimplemented here.
- `pc-connect send` has no `--reply-to`; when `reply_to` is set the script
  automatically uses the `pc` sidecar instead.
- `event.draft`/`event.choice` are reserved vocabulary, not implemented by pc.
- `dispatch`/`bridge` require a configured LLM provider for `prime-agent`.
