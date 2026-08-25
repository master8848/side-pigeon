# Persistence

How `pc serve` keeps your events when clients are offline.

## The problem

Providers keep sending messages even when no client is connected to `pc serve`. Without storage, those messages are lost — the next client that connects only sees new ones.

## Two modes

| Where you use us | Storage | You do |
| --- | --- | --- |
| **Daemon** — `pc serve` holds one connection per provider and fans out to many clients | SQLite on disk (WAL) — on by default | Nothing. Replay just works. |
| **Library** — you embed us (`provider-core` / `provider-transport` as a Rust crate, or `provider-ffi` via `bun:ffi`) | None — you own storage | Store `ChannelMessage` however you want. |

## Daemon: SQLite by default

Every `event.message` / `event.error` is appended to a local file. Each gets a cursor — a number that only goes up (`1, 2, 3, ...`).

```
pc serve
# SQLite at ./pc-events.db, WAL mode. Replay via ?since=
```

Change the path:

```sh
pc serve --persist /var/lib/pc/events.db
PC_PERSIST_PATH=/var/lib/pc/events.db pc serve
```

Turn it off (in-memory only, no replay):

```sh
pc serve --no-persist
```

If you built `pc` lean (no SQLite), replay is unavailable:

```sh
cargo install --path bin/pc --no-default-features --features demo
pc serve --persist ./x.db
# -> --persist requires building with --features persist
```

Why SQLite is optional: only the `pc` binary pulls `rusqlite` (feature `persist`, `bundled` — no system SQLite needed). Library crates stay lean and don't pay for it.

## Replay missed events

`GET /api/events?since=CURSOR` returns everything after that cursor.

```sh
# live stream
curl -N http://localhost:8788/api/events

# catch up from cursor 42
curl "http://localhost:8788/api/events?since=42"
# -> { "events": [{ "cursor": 43, "event": { "jsonrpc":"2.0","method":"event.message","params":{...}} }], "latest_cursor": 120 }

# page it
curl "http://localhost:8788/api/events?since=42&limit=100"
```

Typical client logic:

1. Remember the last `cursor` you processed.
2. On reconnect, `GET /api/events?since=<cursor>` to catch up.
3. Then open the live `GET /api/events` SSE stream.

Without `persist` you get `501 built without --features persist` or `500 persistence not enabled`.

## Library: you manage it

No SQLite is linked. Use `EventBus` / `ProviderClient` and persist how you like:

```rust
use provider_core::{EventBus, ProviderClientBuilder};

let bus = EventBus::new();
bus.subscribe(Default::default(), |msg| {
    // write msg to your DB
});

let client = ProviderClientBuilder::with_bus(bus).build()?;
```

For Bun's fast path (`provider-ffi`), `AppState::with_persist(path)` enables the same SQLite log inside your embed if you opt into the `persist` feature.

## What gets persisted

- **Yes:** `event.message` and `event.error` (the same JSON you see on SSE / WS).
- **No:** `capabilities`, `send` receipts, `health`. Provider credentials are never written.

The table is `events(cursor INTEGER PRIMARY KEY AUTOINCREMENT, method TEXT, payload TEXT, ts INTEGER)` with `PRAGMA journal_mode=WAL`.

## FAQ

**Do I need to install SQLite?** No. `rusqlite` bundles it.

**Can I use Postgres / Redis instead?** For the library, yes — you own it. For the daemon, only SQLite is built in today.

**How big does the file get?** One row per event. No auto-pruning yet — rotate or `DELETE` old rows yourself. A limit for `?since=` (`&limit=`) keeps responses bounded.
