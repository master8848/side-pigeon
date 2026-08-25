---
name: pc-msg
description: Lean provider messaging for agents via provider-connect. Send and receive chat messages (telegram, discord, demo) from the shell — no plugin needed. Use when a message arrives from a provider chat or must be sent to one, when an agent needs to hand a message to another agent's session (opencode run --session / prime-agent send), or when mapping chat ids to agent sessions.
---

# pc-msg — out-of-process lean messaging

`pc_msg.py` is a zero-dependency (Python 3 stdlib) CLI that gives ANY agent
with a shell access to provider messaging through
[provider-connect](https://github.com/your-org/provider-connect) — the Rust
sidecar + `pc-connect` CLI. It is the out-of-process path: nothing runs inside
the agent process; every command spawns the binary, does one job, and exits.

- **Sending** is reliable: spawn, send, print the receipt, exit.
- **Receiving** is on-demand only: `poll` runs for a bounded time, prints
  events as JSON lines, and exits. It is NOT a continuous background service.
  For dependable receiving run the sidecar continuously (see README WARNINGS).

## Install

The skill is one folder: `agent-skill/` (this repo). The only moving parts are
`pc_msg.py` (the script) and `sessions.json` (your chat→session map).

### As an opencode agent skill

opencode agents read markdown through instructions files:

1. Add this file to the agent's instructions. In `AGENTS.md` (repo root or
   `~/.config/opencode/AGENTS.md`):

   ```md
   @provider-connect/agent-skill/SKILL.md
   ```

   (opencode resolves `@path` imports relative to the AGENTS.md location.)
   Alternatively add the absolute path to the `"instructions"` array in
   `opencode.json`:

   ```json
   { "instructions": ["/path/to/provider-connect/agent-skill/SKILL.md"] }
   ```

2. The agent calls `python3 /path/to/provider-connect/agent-skill/pc_msg.py …`
   for every messaging action.

### As a prime agent skill

1. Install as a skill entry (from the agent kernel):

   ```python
   await rlm.harness.create_skill(
       name="pc-msg",
       kind="markdown",
       description="Lean provider messaging via provider-connect (shell, no plugin).",
       path="/path/to/provider-connect/agent-skill/SKILL.md",
   )
   ```

   or add a prompt note pointing at the file. The skill's `pc_msg.py` runs
   from the shell: `python3 /path/to/provider-connect/agent-skill/pc_msg.py …`.

2. The prime in-kernel alternative for receiving: the agent itself calls
   `pc_msg poll …` as a tool, reads the JSON lines in its context, and replies
   with `pc_msg send …`. No plugin, no daemon message API needed.

### Both

- Put `pc_msg.py` on `PATH` (symlink into `~/bin` or similar) to shorten
  invocations, or keep using the full path.
- `sessions.json` lives next to the script, or in `~/.config/pc-msg/`,
  or point at it with `--config`. Copy `sessions.example.json`.

## Commands

```
pc_msg send     --provider X --chat Y [--text T | --text-file -]
pc_msg poll     [--timeout N] [--once] [--providers a,b] [--json]
pc_msg forward  --session ID | --chat Y [--text T | --text-file -]
pc_msg resolve  --chat Y [--provider X] [--autodetect]
pc_msg check    [--provider X]
pc_msg sessions
```

- `send` prints the receipt JSON (`{"message_id": ..., "ts": ...}`).
- `poll` prints one JSON line per event, normalized to
  `{"event":"event.message","message":{…}}` / `{"event":"event.error","error":{…}}`.
  Human-readable hints — including the resolved session and the exact
  one-command handoff — go to **stderr** (suppress with `--json`).
- `forward` is the ONE-COMMAND handoff: it resolves the mapped session and
  runs the agent handoff itself:
  - opencode → `opencode run --session <id> [--dir <project>] "<text>"`
  - prime → `prime-agent send <agent-name> "<text>"`
  - custom → your `handoff` template in sessions.json.

## Backends (automatic)

1. `pc-connect` (the cli/ binary) when found — env `PC_CONNECT_BIN` overrides.
2. else the `pc` JSON-RPC sidecar — env `PC_BIN`, else
   `target/{debug,release}/pc` under the repo root, else PATH. The script
   speaks JSON-RPC 2.0 (NDJSON) over the sidecar's stdio directly.
3. `--backend pc-connect|pc|auto` forces a choice.

Provider credentials/config are the sidecar's job (its config file or
`PC_PROVIDERS` / `PC_<ID>_TOKEN` env); pc_msg passes the environment through.

## Config: sessions.json

Map provider chats to agent sessions so incoming messages can be handed off:

```json
{
  "sessions": [
    { "id": "opencode-main", "provider": "telegram", "chat": "123456789",
      "agent": "opencode", "session": "<opencode session id>",
      "project": "/path/to/repo" },
    { "id": "prime-research", "provider": "telegram", "chat": "987654321",
      "agent": "prime", "session": "<agent name from `prime-agent list`>" },
    { "id": "custom", "provider": "demo", "chat": "demo-room",
      "agent": "opencode", "session": "any-id",
      "handoff": ["mycmd", "--session", "{session}", "{text}"] }
  ]
}
```

Fields: `id` (local label, optional), `provider`, `chat` (or `chats` list for
aliases), `agent` (`opencode` | `prime` | anything with a custom `handoff`),
`session` (opencode session id / prime agent name), `project` (opencode
`--dir`), `handoff` (command template; placeholders `{session} {chat}
{provider} {text}`; string form is shlex-split).

Resolution order: `--config` → `$PC_MSG_CONFIG` → `./sessions.json` →
`~/.config/pc-msg/sessions.json`.

Session ids:

- **opencode**: `opencode session list`, or the newest dir under
  `~/.local/share/opencode/storage/session/<id>/` (`pc_msg resolve
  --chat <id> --autodetect` suggests one).
- **prime**: the session lives under
  `~/.prime/agent/session-artifacts/<session-uuid>/`; the handoff handle is
  the agent NAME from `prime-agent list` (delivered via
  `prime-agent send <name> <text>`). Prime sessions MUST be configured in
  sessions.json — there is no reliable name↔chat auto-detection.

## Workflow

```sh
# send
pc_msg send --provider telegram --chat 123456789 --text "build is green"

# receive for 60s, hand off mapped messages with one command
pc_msg poll --providers telegram --timeout 60
#   stdout: {"event":"event.message","message":{...}}
#   stderr: [pc_msg] resolved session opencode-main -> one-command handoff:
#           pc_msg forward --session <id> --text '...'

# hand one message to a session (the printed one-command form)
pc_msg forward --session <id> --text "build is green"
pc_msg forward --chat 123456789 --text-file - < msg.txt

# health
pc_msg check --provider telegram
```

## WARNINGS (short form — full matrix in README)

- `poll` is NOT a background service. Messages are only received while a poll
  (or a continuously running sidecar) is active.
- **telegram**: bot API queues updates ~24h and long-poll only receives while
  a process polls; each `pc_msg poll` spawns a fresh sidecar whose in-memory
  offset restarts, so catch-up redelivers recent updates (dedupe by
  `message.id`).
- **discord**: gateway delivers only while connected; missed messages are NOT
  replayed. Do not rely on `poll` for discord receiving — run the sidecar
  continuously.
- **demo**: local-only echo provider; safe for tests.
- For dependable receiving run `pc-connect listen` / `pc` as a persistent
  sidecar and have it deliver into the agent by other means.
