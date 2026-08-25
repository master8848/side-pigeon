# @mbsks/side-pigeon — Headless client for any Node/Bun app (agent or not)

> Thin JS that spawns the `side-pigeon` Rust sidecar (binary `pc`) over stdio JSON-RPC 2.0.
> Rust owns the provider connections (~2–30 MB idle); JS just fans out events.

## Install

```sh
bun add @mbsks/side-pigeon
```

Requires Bun ≥ 1.4 or Node ≥ 20, and the `pc` binary on `PATH` (or pass `pcBin`).

```sh
cargo install --path bin/pc --features telegram,discord   # builds `pc` (Rust 1.97.1)
```

Tooling: oxlint + oxfmt + TypeScript 7 — see repo `package.json`, `.oxlintrc.json`, [repo README](../../README.md).

## 5-line non-agent example

```ts
import { createProviderClient } from "@mbsks/side-pigeon";
const pc = createProviderClient({ providers: [{ id: "demo" }], pcBin: "pc" });
await pc.start();
pc.subscribe({}, (msg) => console.log(msg));
await pc.send({ provider: "demo", channelId: "my-room", text: "hello" });
```

Express `POST /notify`:

```ts
import express from "express";
const app = express();
app.use(express.json());
app.post("/notify", async (req, res) => {
  const receipt = await pc.send({
    provider: "telegram",
    channelId: req.body.chatId,
    text: req.body.text,
  });
  res.json(receipt); // { message_id, ts }
});
```

See [`docs/app-integration.md`](../../docs/app-integration.md) for cron/HTTP/Rust recipes.

## API surface

```ts
import { createProviderClient, createAgentAdapter } from "@mbsks/side-pigeon";
import { stdio } from "@mbsks/side-pigeon/transports/stdio.js";
import { dedup, echoDedup } from "@mbsks/side-pigeon/plugins/dedup.js";
import { PcClient, adaptBunSpawn, RpcError } from "@mbsks/side-pigeon/client.js";
```

| Export                       | One-liner                                                                                                                                                           |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `createProviderClient(opts)` | Headless client — `start()`, `subscribe(filter, cb) => unsubscribe`, `send(SendInput) => SendReceipt`, `use(plugin)`, `createSendMutation`, `[Symbol.asyncDispose]` |
| `Transport` / `stdio(opts)`  | Pluggable transport; default is stdio child process (`pcBin`, `pcArgs`, `env`, `spawnFn`, `requestTimeoutMs`)                                                       |
| `Plugin` / `dedup(opts)`     | Middleware — `onMessage(msg) => suppress?`, `onError(err)`; `dedup` keys on `message.id`, `echoDedup` on `send` receipts                                            |
| `EventFilter`                | `{ provider?, channelId?, explicitlyAddressed? }` or `(msg) => boolean` predicate                                                                                   |
| `PcClient` / `adaptBunSpawn` | Low-level NDJSON stdio client; `adaptBunSpawn(Bun.spawn(...))` wraps `Bun.Subprocess` into `ChildLike` for `spawnFn`                                                |
| `RpcError`                   | JSON-RPC error with `code` (`-32001` config, `-32002` auth, `-32003` rate-limit, `-32004` protocol, `-32005` network)                                               |

## Source layout

```
packages/core/src/
  index.ts              # public re-exports
  client.ts             # PcClient + adaptBunSpawn + RpcError
  provider-client.ts    # createProviderClient + createAgentAdapter
  schema.ts             # ChannelMessage / SendMessage / SendReceipt + helpers
  ndjson.ts             # attachNdjsonReader + parseLine
  transports/stdio.ts   # stdio(opts).connect() -> PcClient
  plugins/dedup.ts      # dedup() + echoDedup()
```

Types mirror `crates/provider-core/src/schema.rs` and [`docs/api-contract.md`](../../docs/api-contract.md).

## Links

- App integration guide — [`docs/app-integration.md`](../../docs/app-integration.md)
- Node stdio example — [`examples/node/`](../../examples/node/)
- Contract — [`docs/api-contract.md`](../../docs/api-contract.md)
- Architecture — [`docs/architecture.md`](../../docs/architecture.md)
