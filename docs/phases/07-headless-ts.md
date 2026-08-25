# Phase 07 — Headless TS core (`@provider-connect/core`)

**Lens:** TankStack + Bun · **Status:** planned

## Why

`plugins/opencode-plugin/src/pc-client.ts:82` `PcClient` (spawn + `readline` NDJSON + id-matched `pending` + `EventEmitter`) and `plugins/opencode-plugin/src/runtime.ts:68` `ProviderConnectRuntime` (dedup at `runtime.ts:77` `recentIds/recentSent`, `SessionMap` at `session-map.ts:28`, `rooms` allowlist) are welded to Opencode's `ClientLike` at `runtime.ts:21`. There are three duplicated NDJSON clients (`pc-client.ts:64`, `examples/node/index.mjs:64`, `pi-plugin` style). The promise is `bun add provider-connect` works for *any* agent (Opencode / Pi / Eliza / custom Node script) in 5 lines.

## Scope

- New `packages/core/src/` (headless, no `@opencode-ai/plugin` dep):
  ```ts
  export function createProviderClient(opts: {
    providers: ProviderDef[]; // telegram(opts), discord(opts)
    transports?: Transport[];  // stdio({bin}), websocket({port}), http({url})
    plugins?: Plugin[];        // retry(), dedup({windowMs}), logger()
    defaultSendOptions?: { timeoutMs: number; retryOn: ("Network"|"RateLimit")[] };
  }): ProviderClient;
  // ProviderClient: subscribe(filter: EventFilter, cb) => unsubscribe
  //               .createSendMutation({onMutate, onSuccess, onError})
  //               .send({provider, channelId, text}) -> SendReceipt
  //               .use(adapter)
  ```
- `createAgentAdapter({ onMessage: (msg)=>Promise<string|void> })` → subscribes + session mapping, replaces `runtime.ts:221` `deliver()`.
- Extract shared `PcClient`/NDJSON parser from `pc-client.ts:82` + `examples/node/index.mjs:64` into `packages/core`; `opencode-plugin` re-exports/ wraps it.
- Schema types generated once from `schema.rs:15` via `ts-rs`/`schemars` → `packages/core/src/schema.generated.ts`, replacing hand-rolled `WireMessage` at `pc-client.ts:32` (drift: `ContentPart::Text(String)` at `schema.rs:54` vs wire `string`).
- `SpawnFn` generalized to `Bun.spawn | child_process.spawn`.

### Opencode as adapter

- `plugins/opencode-plugin/src/index.ts:18` `ProviderConnectServer` becomes:
  ```ts
  import { createProviderClient } from '@provider-connect/core';
  import { opencodeAdapter } from '@provider-connect/opencode';
  export const ProviderConnectServer = async (input, opts) => {
    const pc = createProviderClient({ providers: resolveProviders(opts), transports:[stdio({bin: opts.pcBin})] });
    return opencodeAdapter(pc, input);
  };
  ```
- `runtime.ts:68` `ProviderConnectRuntime` shrinks to ~80 LOC `packages/opencode/src/adapter.ts`.

### Bun `postinstall`

- `packages/core/package.json` `postinstall` fetches prebuilt `pc-{os}-{arch}` from GitHub Releases, fallback to `cargo build --release -p pc`.

## Exit criteria

- Any Bun/Node agent: `bun add provider-connect` then `createProviderClient` + `subscribe` in 5 lines, no cargo required.
- `opencode-plugin` thin adapter still passes `bun run --cwd opencode-plugin typecheck` + existing `test/*.test.ts`.
- No duplicated JSON-RPC parser; one `schema.generated.ts` source of truth.

## Files

- `packages/core/src/{client.ts,index.ts,schema.generated.ts}` (new)
- `plugins/opencode-plugin/src/{pc-client.ts,runtime.ts,index.ts}` refactor to import from `@provider-connect/core`
- `examples/node/index.mjs:64` use shared client
