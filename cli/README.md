# pc-connect — thin connection CLI for provider-connect

`pc-connect` is the simple, scriptable command-line interface to
[provider-connect](../README.md): one self-contained binary that **sends**
messages, **listens** for inbound messages, and **checks** connectivity —
using the exact same providers, config contract and JSON-RPC error vocabulary
as the `pc` sidecar, but embedded in-process (no sidecar to spawn or manage).

```text
pc-connect send --provider <id> --chat <chat-id> [--text <text> | --text-file -] [--json]
pc-connect listen [--providers a,b] [--timeout <secs>] [--once] [--json]
pc-connect check [--provider <id>] [--json]
```

---

## ⚠️ DATA-LOSS WARNING — READ THIS FIRST

**`pc-connect` is NOT a continuous background service.** Each invocation is a
short-lived process: it connects to the provider, does its job, and exits.
This makes it excellent for **sending** and for ad-hoc/scripted **receiving**,
but **receiving has data loss windows** whenever nothing is listening:

| Provider | Receiving behavior | Data loss while `listen` is stopped |
|---|---|---|
| `telegram` | getUpdates long-poll — receives **only while `listen` runs** | ✅ LOST (Telegram only queues updates for a short window / while the bot polls; no delivery while the process is down) |
| `discord` | requires a **continuous gateway connection** — `listen` holds it only for its own lifetime | ✅ LOST (no gateway = no events, and gateway state/session is not persisted across invocations) |
| `demo` | local-only echo; announce on start | ✅ LOST (per-process; nothing is ever delivered cross-process) |

**If you need reliable receiving, do NOT rely on `pc-connect listen` as a
daemon.** Run the [`pc` sidecar](../bin/pc/) (a long-lived process
that streams events over stdio JSON-RPC), or run `pc-connect listen` inside a
supervised loop that restarts it (e.g. systemd `Restart=always`,
`while true; do pc-connect listen ...; sleep 1; done`). Even then, events
that arrive between a crash and the restart are lost — the sidecar is the
only receiver that never has gaps while it is alive.

`pc-connect` is best suited for **sending**, and for **receiving when the
listener is only needed transiently** (tests, demos, one-shot polls).

---

## Contract (stable — other agents depend on this)

### `pc-connect send --provider <id> --chat <chat-id> [--text <text> | --text-file -] [--json]`

Sends one message and prints the **SendReceipt** JSON on stdout:

```json
{"message_id":"12345","ts":1710000000000}
```

* Exit `0` on success. On failure: non-zero exit **and** an error JSON object
  on stdout, using the JSON-RPC error vocabulary of the sidecar
  (`-32001` config, `-32002` auth, `-32003` rate-limit, `-32004` protocol,
  `-32005` network, `-32603` internal):
  `{"error":{"code":-32004,"message":"unknown provider 'nope' (...)"}}`.
* `--text` and `--text-file` are mutually exclusive; exactly one is required.
  `--text-file -` reads the body from stdin (a single trailing newline is
  stripped); `--text-file <path>` reads a file.
* `--json` is accepted and is also the default: stdout is JSON either way.

### `pc-connect listen [--providers a,b] [--timeout <secs>] [--once] [--json]`

Starts the providers (all configured, or the `--providers` subset) and prints
**one JSON object per line** on stdout:

```json
{"event":"message","message":{"id":"...","channel":"telegram","channel_id":"...","sender":{...},"content":[{"Text":"hi"}],"ts":...}}
{"event":"error","error":{"provider":"telegram","code":-32005,"message":"...","data":{...}}}
```

* Exits `0` after `--timeout` seconds, after the first event with `--once`,
  or when the event stream closes. Without `--timeout`/`--once` it runs until
  Ctrl-C (exit code is then the shell's, 130).
* **Documented deviation:** `--once` exits on the first `event.message` **or**
  the first `event.error` — an async provider error means the listen is dead,
  and hanging forever after one would be worse. The contract's promise
  ("exits after the first message") is preserved.
* Logs (set `RUST_LOG=debug|info|trace`) go to **stderr**; stdout carries
  only event lines.

### `pc-connect check [--provider <id>] [--json]`

Connectivity check: initialize + capabilities + a **listen smoke** per
provider. Exit `0` when every checked provider is healthy, `1` otherwise.

* `demo`: `start()` must push its start announcement through the transport
  within 6 s — proves the whole pipeline.
* `telegram`/`discord`: the provider connects asynchronously; `check` polls
  its async error slot for 6 s. Auth failures (Telegram 401, Discord gateway
  close 4004) and network failures fail the check; silence passes it
  (long-poll / gateway in flight). A Telegram `getMe` call is not used — the
  crate does not expose one, so this is the documented "initialize+listen
  smoke" per the contract.
* `--json` prints `{"ok":true,"protocolVersion":"0.1.0","methods":[...],"providers":[{"provider":"demo","ok":true,"detail":"..."}]}`.

## Config (same env contract as `pc`)

`PC_PROVIDERS=demo,telegram` · `PC_TELEGRAM_TOKEN=123:abc` ·
`PC_TELEGRAM_CONFIG={"base_url":"..."}` (optional extra JSON, merged) — plus
`PC_CONFIG` / `-c, --config <path>` for a JSON file with the same shape as the
sidecar. The config module is copied verbatim from `bin/pc/src/config.rs`
(see [Design](#design)); the sidecar remains the single source of truth for
the env contract.

Provider ids: `demo` (built-in, default feature), `telegram` and `discord`
(compile-time feature-gated, same as `pc`). Config keys: see
[`bin/pc/src/main.rs`](../bin/pc/src/main.rs) `USAGE`.

## Build, test, verify

```bash
cargo build --release            # default: demo only
cargo build --release --features telegram,discord   # all providers
cargo test                       # unit + integration (spawns the real binary)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Release binaries (stripped, LTO): **1.1 MB** (demo), **3.3 MB**
(demo+telegram+discord) — comfortably under the 10 MB note in the parent
project's design docs. Idle RSS is the provider-connect baseline (< 50 MB).

## Design

* **Embed, don't spawn.** `pc-connect` links `provider-core` +
  `provider-transport` + the feature-gated provider crates directly and drives
  an `AppState` in-process — the same code path the `pc` sidecar serves over
  stdio. One binary, nothing to spawn, no protocol hop. (A `PC_BIN`/spawn mode
  was considered and rejected: it would add a process to manage and a second
  binary to ship for zero benefit at this size.)
* **Standalone workspace.** `cli/` has its own empty `[workspace]` table and
  its own committed `Cargo.lock`; it is **not** a member of the root
  `provider-connect` workspace, so siblings' builds never collide with ours
  (and our lockfile never drags new versions into theirs). It depends on the
  provider crates by path (`../crates/...`).
* **Copied modules, attributed.** `src/config.rs`, `src/demo.rs` and the
  provider builders in `src/providers.rs` are copied verbatim from
  `bin/pc/` with attribution comments — the sidecar stays the single source
  of truth, and `pc-connect` stays buildable without touching `bin/`.
  (Refactoring them into `provider-core` was considered and rejected: it
  would churn files siblings are actively editing.)
* **Per-invocation providers.** Each `pc-connect` process builds its own
  provider instances; there is no shared daemon state. Consequence: the
  `demo` provider's echo is observable only inside the sending process — a
  cross-process `send` → `listen` round-trip requires a real provider
  (telegram/discord deliver through the platform) or the `pc` sidecar. The
  in-process round-trip is covered by unit tests in `src/ops.rs`.
* **Runtime.** One current-thread tokio runtime per invocation (like `pc`);
  stdout is reserved for JSON, logs go to stderr (default level `warn`;
  `RUST_LOG=debug` for detail).

## Supply chain

Every dependency is already in the audited provider-connect closure
([`docs/supply-chain.md`](../docs/supply-chain.md)) — **no new external
crates**. Version ranges mirror the root workspace; the committed
`Cargo.lock` is the exact pin. Verified 2026-08-13 against crates.io
`created_at`: all 168 registry packages in `cli/Cargo.lock` are ≥ 14 days old
(newest: `zmij`, 238 days; `http-body-util` is pinned to `0.1.4` — `0.1.5`
was newer than the policy window). Re-run the gate with the repo script after
any dependency change.

## Related tooling

Sibling agents in this repo build on the same provider-connect contract:

* [`plugins/opencode-plugin/`](../plugins/opencode-plugin/) — OpenCode plugin
* [`plugins/pi-plugin/`](../plugins/pi-plugin/) — Prime Intellect agent plugin
* [`plugins/agent-skill/`](../plugins/agent-skill/) — agent skill

They are owned by their respective agents; this CLI only documents the
interface they share (`send` / `listen` / `check` above).
