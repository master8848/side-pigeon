# agent-skill — cross-agent lean messaging via provider-connect

A cross-agent **skill** (works for opencode AND prime agent, and any other
agent that can run shell commands) for lean provider messaging through
provider-connect. This is the **out-of-process path**: unlike the plugin
siblings (`plugins/opencode-plugin/`, `plugins/pi-plugin/`) which run inside Node processes,
this skill installs a tiny Python 3 stdlib CLI and every action spawns the
provider-connect binary, does one job, and exits. No plugin, no long-running
in-agent process.

| File | Purpose |
| --- | --- |
| `pc_msg.py` | The skill's one script (Python 3 stdlib only, no deps). |
| `SKILL.md` | The skill definition (generic markdown, both agent types). |
| `sessions.example.json` | Config template: chat ids → agent sessions. |
| `tests/test_pc_msg.py` | Unit tests (JSON parsing, session resolution, mocked subprocess flows). |

The script talks to the **`pc-connect` CLI** (contract: `send --provider <id>
--chat <chat-id> [--text <text>|--text-file -]` prints receipt JSON; `listen
[--providers a,b] [--timeout <secs>] [--once] [--json]` prints
`event.message`/`event.error` JSON lines; `check [--provider <id>]` exits
0/1) and **falls back to the `pc` JSON-RPC sidecar** over stdio (JSON-RPC 2.0,
NDJSON) when `pc-connect` is absent.

## Install

Requirements: Python 3.8+ (stdlib only), plus the provider-connect sidecar
binary (`pc-connect` from cli/, or `pc` from bin/pc — the script also finds
`target/{debug,release}/pc` under the repo root). Provider credentials live in
the sidecar's config (its `--config`/`PC_CONFIG`/`PC_PROVIDERS` +
`PC_<ID>_TOKEN`), which pc_msg passes through.

### opencode agents

opencode reads markdown skills through instructions files:

1. Add the skill to the agent's instructions. In `AGENTS.md`:

   ```md
   @provider-connect/plugins/agent-skill/SKILL.md
   ```

   or in `opencode.json`:

   ```json
   { "instructions": ["/abs/path/to/provider-connect/plugins/agent-skill/SKILL.md"] }
   ```

2. The agent runs `python3 /abs/path/provider-connect/plugins/agent-skill/pc_msg.py …`
   for every messaging action. Optionally symlink `pc_msg.py` into `~/bin`.

### prime agent

1. Install the skill entry from the agent kernel:

   ```python
   await rlm.harness.create_skill(
       name="pc-msg",
       kind="markdown",
       description="Lean provider messaging via provider-connect (shell, no plugin).",
       path="/abs/path/to/provider-connect/plugins/agent-skill/SKILL.md",
   )
   ```

   (or add a prompt note referencing the file). The agent then calls
   `pc_msg …` from the shell.

2. The natural prime receiving path: the agent itself calls
   `pc_msg poll …` as a tool call, sees the JSON lines in its context, and
   replies with `pc_msg send …`. In-family delivery between prime sessions
   goes through the `agent_message` skill / `prime-agent send` (used by
   `pc_msg forward` for prime sessions).

### Verify

```sh
pc_msg check --provider demo        # needs PC_PROVIDERS=demo in env (sidecar config)
pc_msg send --provider demo --chat demo-room --text "hello"
pc_msg poll --providers demo --once
```

The `demo` provider is local (no network) and echoes every send back as an
inbound message — a full send/receive smoke test with zero credentials.

## Usage

```
pc_msg send     --provider X --chat Y [--text T | --text-file -]
pc_msg poll     [--timeout N] [--once] [--providers a,b] [--json]
pc_msg forward  --session ID | --chat Y [--text T | --text-file -]
pc_msg resolve  --chat Y [--provider X] [--autodetect]
pc_msg check    [--provider X]
pc_msg sessions
```

Global flags come before the subcommand: `pc_msg --config path/to/sessions.json poll …`.

### send — one message, print the receipt

```sh
pc_msg send --provider telegram --chat 123456789 --text "build is green"
# stdout: {"message_id": "...", "ts": 1786598277426}
echo "multi\nline" | pc_msg send --provider telegram --chat 123456789 --text-file -
```

### poll — receive for a bounded time

```sh
pc_msg poll --providers telegram --timeout 60
```

stdout is machine-readable, one JSON line per event, normalized to a stable
shape (whatever the backend prints underneath):

```json
{"event": "event.message", "message": {"id": "...", "channel": "telegram", "channel_id": "123456789", "content": [{"Text": "hi"}], "sender": {"id": "...", "name": "..."}, "ts": 1786598277426, ...}}
{"event": "event.error", "error": {"provider": "telegram", "code": -32005, "message": "..."}}
```

stderr carries the human view, including the resolved session and the
**exact one-command handoff**:

```
[pc_msg] message from Alice in telegram chat 123456789: hi
[pc_msg] resolved session opencode-main (agent=opencode) -> one-command handoff:
  pc_msg forward --session 0462509668a54fc6db0a1cf67b97a0f44aa1ab9b --text 'hi'
  (runs: opencode run --session 0462509668a54fc6db0a1cf67b97a0f44aa1ab9b --dir /repo 'hi')
```

`--once` stops after the first message; `--json` suppresses the stderr hints.
`--timeout 0` (default) runs until `--once`/Ctrl-C.

### forward — hand a message to an agent session in ONE command

```sh
pc_msg forward --session 0462509668a54fc6db0a1cf67b97a0f44aa1ab9b --text "hi"
pc_msg forward --chat 123456789 --text-file - < msg.txt
```

Resolves the session (by session id or by chat id) and executes the handoff:

| agent | handoff command |
| --- | --- |
| `opencode` | `opencode run --session <id> [--dir <project>] "<text>"` |
| `prime` | `prime-agent send <agent-name> "<text>"` |
| custom | your `handoff` template (placeholders `{session} {chat} {provider} {text}`) |

### resolve / sessions / check

```sh
pc_msg resolve --chat 123456789 --provider telegram --autodetect
pc_msg sessions
pc_msg check --provider telegram; echo $?    # 0 available, 1 not
```

## sessions.json — chat → session mapping

```json
{
  "sessions": [
    {"id": "opencode-main", "provider": "telegram", "chat": "123456789",
     "agent": "opencode", "session": "<opencode session id>", "project": "/path/to/repo"},
    {"id": "prime-research", "provider": "telegram", "chat": "987654321",
     "agent": "prime", "session": "<agent name from `prime-agent list`>"},
    {"id": "custom", "provider": "demo", "chat": "demo-room",
     "agent": "opencode", "session": "any-id",
     "handoff": ["mycmd", "--session", "{session}", "{text}"]}
  ]
}
```

Lookup order: `--config PATH` → `$PC_MSG_CONFIG` → `./sessions.json` →
`~/.config/pc-msg/sessions.json`. `chat` may be a list via `chats` (aliases);
an entry without `provider` matches any provider.

**Resolving sessions without a config entry:**

- opencode: `opencode session list`, or the newest directory under
  `~/.local/share/opencode/storage/session/<session-id>/`. `pc_msg resolve
  --chat <id> --autodetect` prints the heuristic result.
- prime: session files live under `~/.prime/agent/session-artifacts/<uuid>/`,
  but the handoff handle is the agent **name** from `prime-agent list`
  (`prime-agent send <name> <text>`), which is not derivable from the
  artifacts directory — configure prime sessions in sessions.json.

## WARNINGS — receiving is NOT a background service

> `pc_msg poll` spawns the sidecar, listens for a bounded time, prints, exits.
> Messages are only received **while a listener is running**. If you need
> dependable receiving, run the sidecar continuously
> (`pc-connect listen --providers telegram,discord` or `pc` as a service) and
> have it deliver into the agent by other means. For SENDING, poll-less
> `pc_msg send` is always reliable.

### Receive matrix per provider

| provider | mechanism | while nothing listens | short `pc_msg poll` runs | reliable receiving |
| --- | --- | --- | --- | --- |
| `demo` | in-process echo | n/a (local test fixture; announces on start, echoes sends) | full — safe for tests | n/a (local) |
| `telegram` | Bot API `getUpdates` long-poll (in-memory offset cursor) | no loss for ~24h: the bot API queues updates; they are delivered to the next poll that starts from a low offset | catch-up delivers queued messages, **but** every `pc_msg poll` starts a fresh sidecar process whose offset cursor restarts → recent updates can be re-delivered (dedupe by `message.id`) | keep one sidecar process alive so the cursor advances continuously |
| `discord` | Gateway v10 WebSocket | **data loss**: gateway delivers only while connected; missed messages are not replayed | receives only while running; anything sent while it was down is gone | sidecar must be running **continuously** (gateway connection, with reconnect) |
| future providers | varies | assume queue-or-lose per provider | per provider | sidecar |

Bottom line: **this skill is best for SENDING and for on-demand receive
checks.** For a dependable always-on inbox, run the sidecar continuously.

## Backend selection

1. `pc-connect` when found (`PC_CONNECT_BIN` overrides PATH lookup).
2. else `pc` (`PC_BIN`, else `target/{debug,release}/pc` under the repo root,
   else PATH). The script implements the JSON-RPC 2.0 stdio client itself
   (`initialize` → `listen`/`send` → `shutdown`, `event.message`/
   `event.error` notifications).
3. `--backend auto|pc-connect|pc` forces a choice.

Differences worth knowing: with `pc-connect`, `--text-file -` is passed
through (the CLI reads your stdin); with the `pc` fallback, pc_msg reads
stdin itself. `send` against the `pc` fallback issues `listen` first because
the sidecar starts providers lazily.

## Tests

```sh
python3 -m unittest discover -s plugins/agent-skill/tests -v
# 44 tests: JSON/event parsing, session resolution, handoff construction,
# mocked subprocess flows for both backends, binary discovery.
```

## Layout / ownership

This directory is owned by the agent-skill agent. Do not modify
`plugins/opencode-plugin/`, `plugins/pi-plugin/`, or `cli/` (owned by their respective
agents). The `pc-connect` contract this skill targets lives in
`docs/api-contract.md` + `crates/provider-transport/src/{jsonrpc,events}.rs`.
