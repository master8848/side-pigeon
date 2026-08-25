# provider-discord

Discord provider for [`provider-connect`](../../README.md): Gateway v10
WebSocket inbound, REST outbound. Hand-rolled on `tokio-tungstenite` +
`reqwest` — **no `serenity` SDK** (ZeroClaw pattern).

## Features

- **Inbound (Gateway v10, JSON encoding)** — `start()` spawns the gateway task:
  connect `wss://gateway.discord.gg/?v=10&encoding=json`, IDENTIFY with intents
  `GUILDS | GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT`, heartbeat every
  `heartbeat_interval` (immediate first beat, per spec; ACK tracking, reconnect
  after >3 unanswered beats), and reconnect/resume:
  - `RECONNECT` / `INVALID_SESSION` / unexpected close → reconnect using
    `RESUME` with the cached session id + sequence (re-IDENTIFY when the
    session is not resumable)
  - `READY` caches the session id, resume URL, and the bot's own user id;
    `GUILD_CREATE` caches guild id → name (minimal state)
  - `MESSAGE_CREATE` is normalized into a
    [`ChannelMessage`](https://docs.rs/provider-core):
    `channel = "discord"`, `sender` from `author{id, username, global_name,
avatar}`, `reply_target`/`thread_ts` from `message_reference`, `ts` from the
    message snowflake, attachments → `MediaAttachment`s (CDN URLs), and
    `explicitly_addressed` when the bot is mentioned. `raw` = full payload.
- **Outbound** — `send()` → REST `POST /channels/{id}/messages` with
  `Authorization: Bot <token>` and a Discord-required `User-Agent`;
  `reply_to` maps to `message_reference`. Returns
  [`SendReceipt`](https://docs.rs/provider-core)`{message_id, ts}`.
- **Errors** — gateway close code 4004 → `Auth` (fatal); REST 401/403 → `Auth`,
  429 → `RateLimit`, other 4xx/5xx → `Protocol`; network failures → `Network`
  (reconnect with capped exponential backoff).

## Usage

```rust
use std::sync::Arc;
use provider_core::{ChatProvider, ProviderEvents, ChannelMessage};
use provider_discord::DiscordProvider;

struct Sink; // transport implements ProviderEvents
impl ProviderEvents for Sink {
    fn on_message(&self, msg: ChannelMessage) { /* forward to agent */ }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut provider = DiscordProvider::new("YOUR_BOT_TOKEN", Arc::new(Sink));
    provider.start().await?;
    // agent loop ...
    provider.stop().await?;
    Ok(())
}
```

## Configuration (builder)

| Method                 | Default                                        | Purpose                          |
| ---------------------- | ---------------------------------------------- | -------------------------------- |
| `with_gateway_url`     | `wss://gateway.discord.gg/?v=10&encoding=json` | gateway endpoint (tests/proxies) |
| `with_rest_base`       | `https://discord.com/api/v10`                  | REST base (tests/self-hosted)    |
| `with_intents`         | `DEFAULT_INTENTS`                              | gateway intents bitmask          |
| `with_request_timeout` | none                                           | per-request REST timeout         |

## Notes

- `MESSAGE_CONTENT` is a **privileged intent** — enable it in the Discord
  developer portal, or inbound `content` will always be empty.
- `send()` is text-only in v0.1; attachments are logged and ignored. Media
  outbound (multipart/form-data) is a future milestone.
- Close-code diagnostics follow the Gateway spec (4004 auth failure is fatal;
  other 4000-range codes trigger resume/reconnect).

## Tests

No network: heartbeat scheduling under `tokio::time` paused-clock
(immediate first beat, exact intervals), `MESSAGE_CREATE` / READY parsing with
fixture JSON (snowflake timestamps, threads, mentions, attachments), gateway
frame builders (IDENTIFY/RESUME/HEARTBEAT), REST send against a hand-rolled
mock server (bot auth + User-Agent + message_reference asserted), 403/429 error
mapping, and `stop()` terminating the reconnect loop.

```bash
cargo test -p provider-discord
```
