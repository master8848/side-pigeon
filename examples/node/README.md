# examples/node — Node.js sidecar example

Demonstrates the **low-RAM sidecar pattern**: a tiny Node process that spawns
the Rust `pc` sidecar binary and drives it over **stdio JSON-RPC 2.0**
(newline-delimited JSON). The Rust process owns the provider connections
(Telegram long-poll / Discord Gateway WebSocket); Node just relays messages —
so Node's memory stays at baseline (`process.memoryUsage()` is printed after
startup and on every event to prove it).

Zero dependencies — plain Node built-ins only (`child_process`, `readline`,
`fs`, `path`).

## Requirements

- Node.js ≥ 18
- Rust toolchain (only to build the sidecar the first time)
- A bot token + a chat/channel id for Telegram or Discord
  - Discord: `MESSAGE_CONTENT` intent enabled in the developer portal

## Usage

```bash
# Telegram
PROVIDER=telegram TOKEN=123456:ABC... CHANNEL_ID=-1001234567890 node index.mjs

# Discord
PROVIDER=discord TOKEN=MTIz... CHANNEL_ID=991234567890123456 node index.mjs

# optional knobs
PC_BIN=/path/to/pc        node index.mjs   # skip the cargo build step
RUN_SECONDS=60            node index.mjs   # listen duration (default 30)
```

What the example does:

1. Resolves the sidecar binary — `$PC_BIN`, then `target/release/pc` relative
   to the repo root (`../bin/pc` is the crate source), building it with
   `cargo build --release -p pc --features telegram,discord` if missing.
2. Spawns `pc` with `PC_PROVIDERS=<provider>` and `PC_<PROVIDER>_TOKEN=<token>`
   (the sidecar's env-config schema); its stderr (tracing logs) is inherited.
3. Speaks the JSON-RPC protocol: `initialize` → `capabilities` → `listen`
   → `send` → `shutdown`, logging responses and `event.message` notifications
   as they arrive.
4. Prints `process.memoryUsage()` after startup and on every inbound event —
   the RSS delta shows the sidecar absorbs all platform SDK cost.

## Protocol cheat-sheet (what the example speaks)

```
request:  {"jsonrpc":"2.0","id":1,"method":"initialize"}
request:  {"jsonrpc":"2.0","id":2,"method":"listen","params":{"providers":["telegram"]}}
request:  {"jsonrpc":"2.0","id":3,"method":"send",
           "params":{"provider":"telegram","message":{"channel_id":"...","text":"hi"}}}
notify:   {"jsonrpc":"2.0","method":"event.message","params":{"message":{...}}}
request:  {"jsonrpc":"2.0","id":4,"method":"shutdown"}
```

See `docs/api-contract.md` for the full method surface.
