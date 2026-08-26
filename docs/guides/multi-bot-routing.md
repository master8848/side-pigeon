# Multi-bot routing — two Telegram / two Discord bots on one `pc`

One `pc` can hold many bots of the *same* platform. Today `id` is both alias and driver `bin/pc/src/main.rs:977` + `crates/provider-core/src/registry.rs:51` duplicate error, so two `telegram` entries fail. This guide uses the **alias fix** `F1`.

## Config

```json
// pc.config.json — F1 alias `id` vs `config.kind`
{
  "providers": [
    { "id": "tg-main", "config": { "kind": "telegram", "token": "123:aaa", "poll_interval_secs": 1 } },
    { "id": "tg-ops",  "config": { "kind": "telegram", "token": "123:bbb" } },
    { "id": "dc-main", "config": { "kind": "discord",  "token": "MTIz..." } }
  ]
}
```
Env alternative: `PC_PROVIDERS=tg-main,tg-ops,dc-main` + `PC_TG_MAIN_TOKEN=...` (upper `id`).

> Without F1 patch, use two `pc serve` processes on different ports.

## Routing

`ChannelMessage` `crates/provider-core/src/schema.rs:15` carries `channel` (=alias `id`) and `channel_id` (chat/room). Filter per bot:

```rust
use provider_core::{EventFilter, EventBus};
bus.subscribe(EventFilter{provider:Some("tg-ops"),..Default::default()}, |m| spawn_ops(m));
bus.subscribe(EventFilter{provider:Some("tg-main"), channel_id:Some("123"),..Default::default()}, |m| spawn_main(m));
```

HTTP: `POST /api/providers/tg-ops/send` `crates/provider-transport/src/http.rs:287` — `id` is alias. `GET /api/events` SSE `http.rs:182` fans out all bots; filter client-side by `msg.channel`.

## `chatKey`

Stable per chat `plugins/opencode-plugin/src/session-map.ts:24` `chatKey="${provider}:${chatId}"` and `plugins/pi-plugin/pc_connect.py:129` `sanitize_component` — include alias so `tg-main:123` vs `tg-ops:123` map to different `hermes --resume` sessions.

## Verify

```sh
pc check --json | jq .providers   # lists tg-main,tg-ops,dc-main
curl -s http://127.0.0.1:8788/health | jq .providers
```
