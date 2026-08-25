# examples/app/express — HTTP app using @provider-connect/core

Minimal HTTP server that embeds the Rust sidecar via the **high-level headless client**
(`createProviderClient`), not hand-rolled NDJSON.

```bash
PC_PROVIDERS=demo node server.mjs
# or: PC_TELEGRAM_TOKEN=123:abc node server.mjs
#     PC_BIN=/path/to/pc node server.mjs
#     PORT=3000 node server.mjs
curl -X POST http://localhost:3000/send -d '{"channelId":"my-room","text":"hi"}' -H 'content-type: application/json'
curl http://localhost:3000/health
curl http://localhost:3000/events   # SSE stream of event.message
```

What it demonstrates vs `examples/node` (raw wire):

| | `examples/node` | `examples/app/express` |
|---|---|---|
| Client | hand-rolled `JsonRpcClient` NDJSON | `createProviderClient` + `stdio` transport |
| Dedup | none | `dedup()` plugin |
| Receiving | `on('notification')` switch | `pc.subscribe({}, handler)` + SSE fan-out |
| Sending | `request('send', {message:{channel_id}})` | `pc.send({provider, channelId, text})` |

Install for real apps: `bun add @provider-connect/core` then
`import { createProviderClient } from "@provider-connect/core"`.
The relative `../../../packages/core/src/*` import here is dev-only so the
example runs without publishing. Memory note: Rust sidecar holds provider
connections; Node RSS stays flat — same as `examples/node:120` proof.
