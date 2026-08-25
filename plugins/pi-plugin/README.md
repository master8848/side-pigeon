# pi-plugin — provider-connect × Prime Agent

Connects [Prime Agent](https://github.com/earendil-works/pi-coding-agent) to
messaging providers (Telegram, Discord, demo) through the provider-connect
`pc` sidecar. This plugin is a **TypeScript extension only**
(`plugins/pi-plugin/extension/provider-connect.ts`) registering LLM tools
`pc_check` / `pc_send` / `pc_listen`; auto-discovered by Prime Agent. It
spawns the Rust `pc` sidecar (`bin/pc`, JSON-RPC 2.0 over stdio, one JSON
document per line) and speaks its wire protocol (`initialize`, `capabilities`,
`listen`, `send`, `shutdown`; notifications `event.message`, `event.error`).
Provider logic stays in Rust.

For shell/Python usage, see `plugins/agent-skill/` (`pc_msg.py`).

## Prerequisites

Build the sidecar (once):

```bash
cd provider-connect   # repo root
cargo build --release -p pc --features telegram,discord
# binary: target/release/pc  (demo provider is built in by default)
```

Configure credentials — env vars (preferred) or a JSON config file:

```bash
export PC_PROVIDERS=demo,telegram        # comma list when no config file
export PC_TELEGRAM_TOKEN=123:abc         # per-provider token
export PC_DISCORD_TOKEN=discord-bot-token
# optional: export PC_CONFIG=/path/to/config.json
# optional: export PC_BIN=/path/to/pc     (default: repo target/, then PATH)
```

Config file form: `{"providers": [{"id": "telegram", "config": {"token": "..."}}]}`.

## Install

Copy the extension into Prime Agent's auto-discovery directory. Discovery
takes direct files (`extensions/*.ts`) or `extensions/<dir>/index.ts`.

Preferred (multi-file):

```bash
cp -R plugins/pi-plugin/extension/provider-connect ~/.prime/agent/extensions/provider-connect
```

Legacy single-file shim (still works):

```bash
cp plugins/pi-plugin/extension/provider-connect.ts ~/.prime/agent/extensions/provider-connect.ts
```

Quick test without copying:

```bash
prime-agent -e ./plugins/pi-plugin/extension/provider-connect/index.ts
# legacy shim also accepts: prime-agent -e ./plugins/pi-plugin/extension/provider-connect.ts
```

The extension has **zero npm dependencies**: `@earendil-works/pi-coding-agent`
(types) and `typebox` (tool schemas) are provided by the Prime Agent runtime's
module aliases/virtual modules, so no `npm install`, no lockfile. Restart or
`/reload` Prime Agent; the tools `pc_check`, `pc_send`, `pc_listen` become
available to the model.

When installed outside the repo, point the extension at the sidecar with
`export PC_BIN=/path/to/pc` (repo-relative lookup only applies when the
extension runs from the provider-connect checkout).

## Usage

As Prime Agent tools (extension installed), the model can call `pc_check`,
`pc_send {provider, channel_id, text, reply_to?}`, and
`pc_listen {provider?, timeout_secs?, once?}` directly.

Example tool calls (from the model):

- `pc_check` — capabilities: providers, methods, features.
- `pc_send {provider: "telegram", channel_id: "123456789", text: "Hello!"}` — send (returns receipt).
- `pc_listen {provider: "telegram", timeout_secs: 60, once: true}` — bounded poll, returns formatted inbound messages.

## Config reference

| Variable | Meaning |
|---|---|
| `PC_BIN` | path to the `pc` sidecar (default: `<repo>/target/{release,debug}/pc`, then `PATH`) |
| `PC_CONFIG` | JSON config file for `pc` |
| `PC_PROVIDERS` | comma-separated provider ids when no config file is given |
| `PC_TELEGRAM_TOKEN` | Telegram bot token |
| `PC_DISCORD_TOKEN` | Discord bot token |
| `PC_<ID>_CONFIG` | optional extra JSON merged into the provider config |

## Limitations

- **Not a daemon.** Receiving = polling: messages arrive only while
  `pc_listen` is running inside an agent turn. There is no background
  process pushing messages into Prime Agent.
- **Data loss per provider.** Each `listen` window returns whatever arrived
  during it; messages that arrive while nothing is listening are subject to
  each provider's own buffering policy (e.g. Telegram long-poll re-delivers on
  the next poll; other providers may drop). Long idle gaps can miss messages.
- **Sends go through Rust.** `check`/`send`/`listen` use the `pc` sidecar.
  Neither is reimplemented in TypeScript.
- `event.draft` / `event.choice` are reserved vocabulary in the sidecar, not
  yet implemented; only `event.message` / `event.error` are consumed.
- Media: inbound attachments are reported with kind/caption; sending
  attachments via the extension is not exposed (the wire supports it — send
  `message.attachments` through the JSON-RPC client if needed).

## Supply chain

- **TypeScript extension: zero runtime dependencies.** Types used for
  verification only, pinned in a throwaway `/tmp` check (not shipped):
  `typescript@5.9.3`, `@earendil-works/pi-coding-agent@0.84.1`,
  `@types/node@24`; `typebox` resolved from the installed prime-agent runtime
  (all published well over 14 days before use).
