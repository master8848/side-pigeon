# pi-plugin — provider-connect × Prime Agent

Connects [Prime Agent](https://github.com/earendil-works/pi-coding-agent) to
messaging providers (Telegram, Discord, demo) through the provider-connect
`pc` sidecar. Two surfaces, one wire contract:

| Surface | What it is | Files |
|---|---|---|
| **Extension** (real plugin) | TypeScript extension registering LLM tools `pc_check` / `pc_send` / `pc_listen`; auto-discovered by Prime Agent | `extension/provider-connect.ts` |
| **Skill** | `SKILL.md` + a Python stdlib-only script (`check`/`send`/`listen`/`session`/`dispatch`/`bridge`) the agent loads on demand | `SKILL.md`, `pc_connect.py` |

Neither surface reimplements providers: both spawn the Rust `pc` sidecar
(`bin/pc`, JSON-RPC 2.0 over stdio, one JSON document per line) and speak its
wire protocol (`initialize`, `capabilities`, `listen`, `send`, `shutdown`;
notifications `event.message`, `event.error`). Provider logic stays in Rust.

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

### Extension (plugin) — recommended

Copy the extension into Prime Agent's auto-discovery directory. Discovery
takes direct files (`extensions/*.ts`) or `extensions/<dir>/index.ts`, so
install as a direct file:

```bash
cp extension/provider-connect.ts ~/.prime/agent/extensions/provider-connect.ts
```

or, for a quick test without copying:

```bash
prime-agent -e ./extension/provider-connect.ts
```

The extension has **zero npm dependencies**: `@earendil-works/pi-coding-agent`
(types) and `typebox` (tool schemas) are provided by the Prime Agent runtime's
module aliases/virtual modules, so no `npm install`, no lockfile. Restart or
`/reload` Prime Agent; the tools `pc_check`, `pc_send`, `pc_listen` become
available to the model.

When installed outside the repo, point the extension at the sidecar with
`export PC_BIN=/path/to/pc` (repo-relative lookup only applies when the
extension runs from the provider-connect checkout).

### Skill

Copy the whole `plugins/pi-plugin/` directory into a skill location:

```bash
cp -R . ~/.prime/agent/skills/provider-connect/
```

(`~/.prime/agent/skills/<name>/SKILL.md` is auto-discovered; the script is
invoked from the skill instructions.) Verify discovery:

```bash
prime-agent /skill:provider-connect   # or check /commands for skill:provider-connect
```

## Usage

```bash
# status: which providers are compiled in
python3 pc_connect.py check [--provider telegram] [--json]

# send a reply (auto-starts the provider; --text or piped stdin)
python3 pc_connect.py send --provider telegram --chat 123456789 --text "Hello!"
python3 pc_connect.py send --provider telegram --chat 123456789 --reply-to 42 <<< "reply"

# poll for inbound messages (default 30s; --once stops after the first)
python3 pc_connect.py listen --provider telegram --timeout 60 --once --json

# per-chat Prime Agent session file
python3 pc_connect.py session --provider telegram --chat 123456789
# -> ~/.prime/agent/sessions/pc-telegram-123456789.jsonl

# deliver a message into that session and print the agent's reply
python3 pc_connect.py dispatch --session "$(pc_connect.py session --provider telegram --chat 123456789)" --text "incoming: ..."

# full loop: listen -> per-chat session -> send the reply back
python3 pc_connect.py bridge --provider telegram --timeout 300 --once
```

As Prime Agent tools (extension installed), the model can call `pc_check`,
`pc_send {provider, channel_id, text, reply_to?}`, and
`pc_listen {provider?, timeout_secs?, once?}` directly.

### Session routing

One chat = one session file: `<session-dir>/pc-<provider>-<sanitized chat id>.jsonl`
(default `~/.prime/agent/sessions`). `bridge`/`dispatch` open it with
`prime-agent --mode rpc --resume <file>` — Prime Agent creates the file on
first contact and resumes the same conversation afterwards (verified: RPC mode
creates a missing resume path, then reuses it). Interactive handoff:

```bash
prime-agent --resume ~/.prime/agent/sessions/pc-telegram-123456789.jsonl
```

## Tests

Stdlib-only unit tests; no `pc` binary or `prime-agent` needed (subprocess is
scripted):

```bash
python3 -m unittest discover -s tests -v
```

Covers: NDJSON/JSON-RPC framing and id matching, `event.message` unwrapping
and text/media extraction, error-code mapping, request timeout / process-exit
paths, deterministic per-chat session routing and sanitization, `send` param
wiring (incl. `reply_to`), `pc-connect` CLI delegation (argv, receipt/event/
error parsing, stdin for long texts, env/PATH binary resolution), and the
full `bridge` dispatch loop with mocked subprocesses.

Verified live (see commit messages): `tsc --noEmit --strict` clean against
pinned `@earendil-works/pi-coding-agent@0.84.1`; a real agent turn called
`pc_check` against the built sidecar (both `-e` and the auto-discovered
global extension); `dispatch` returned a real agent reply; `bridge` routed a
demo-provider message into a session and sent the reply back with a receipt;
`check`/`send`/`listen` ran against the real `pc-connect` binary
(`cli/target/release/pc-connect`, auto-discovered without `PC_CONNECT_BIN`),
and `send --reply-to` fell back to the `pc` sidecar.

## Config reference

| Variable | Meaning |
|---|---|
| `PC_BIN` | path to the `pc` sidecar (default: `<repo>/target/{release,debug}/pc`, then `PATH`) |
| `PC_CONFIG` | JSON config file for `pc` |
| `PC_PROVIDERS` | comma-separated provider ids when no config file is given |
| `PC_TELEGRAM_TOKEN` | Telegram bot token |
| `PC_DISCORD_TOKEN` | Discord bot token |
| `PC_<ID>_CONFIG` | optional extra JSON merged into the provider config |
| `PRIME_AGENT_BIN` | `prime-agent` executable used by `dispatch`/`bridge` |

## Limitations

- **Not a daemon.** Receiving = polling: messages arrive only while
  `listen`/`bridge`/`pc_listen` is running inside an agent turn. There is no
  background process pushing messages into Prime Agent.
- **Data loss per provider.** Each `listen` window returns whatever arrived
  during it; messages that arrive while nothing is listening are subject to
  each provider's own buffering policy (e.g. Telegram long-poll re-delivers on
  the next poll; other providers may drop). Long idle gaps can miss messages.
- **Sends go through Rust.** One-shot `check`/`send`/`listen` prefer the
  `pc-connect` CLI (`cli/target/{release,debug}/pc-connect`, `$PC_CONNECT_BIN`,
  or PATH); `session`/`dispatch`/`bridge` and any send with `--reply-to` use
  the `pc` sidecar. Neither is reimplemented in Python or TypeScript.
- `pc-connect send` lacks `--reply-to` (in-thread replies require the `pc`
  sidecar); the script handles the fallback automatically.
- `event.draft` / `event.choice` are reserved vocabulary in the sidecar, not
  yet implemented; only `event.message` / `event.error` are consumed.
- `dispatch`/`bridge` need a configured LLM provider for `prime-agent`
  (`--mode rpc` runs a real agent turn per inbound message).
- Media: inbound attachments are reported with kind/caption; sending
  attachments via `pc_connect.py` is not exposed (the wire supports it — send
  `message.attachments` through the JSON-RPC client if needed).

## Supply chain

- **Python: stdlib only** (`subprocess`, `threading`, `queue`, `json`,
  `argparse`, ...). No pip dependencies, no lockfile.
- **TypeScript extension: zero runtime dependencies.** Types used for
  verification only, pinned in a throwaway `/tmp` check (not shipped):
  `typescript@5.9.3`, `@earendil-works/pi-coding-agent@0.84.1`,
  `@types/node@24`; `typebox` resolved from the installed prime-agent runtime
  (all published well over 14 days before use).
