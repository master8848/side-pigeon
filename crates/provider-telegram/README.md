# provider-telegram

Telegram provider for [`provider-connect`](../../README.md): `getUpdates`
long-poll inbound, `sendMessage` outbound. Hand-rolled on `reqwest` — **no
`teloxide`/`telegram-bot` SDK** (ZeroClaw pattern: thin clients for simple
protocols).

## Features

- **Inbound** — background polling task started by [`ChatProvider::start`]
  (`getUpdates`, offset cursor, 30 s long-poll, 1 s idle poll interval, both
  configurable). Each `update.message` is normalized into a
  [`ChannelMessage`](https://docs.rs/provider-core) and delivered to the
  [`ProviderEvents`](https://docs.rs/provider-core) sink you supplied.
  - `id` = `"<update_id>/<message_id>"`, `channel = "telegram"`
  - `sender` from `from{id, first_name, username}`
  - `content` = message text (caption surfaces as text when there is no body)
  - `reply_target` = `reply_to_message.message_id`
  - `ts` = `date * 1000` (epoch millis), `raw` = full update JSON
  - media (photo/document/voice/audio/video/sticker) → `attachments`
- **Outbound** — [`ChatProvider::send`] → `sendMessage` →
  [`SendReceipt`](https://docs.rs/provider-core)`{message_id, ts}`; `reply_to`
  maps to `reply_to_message_id`.
- **Errors** — HTTP 401 → `ProviderError::Auth` (fatal, polling stops), 409 →
  `Protocol` (conflicting long-poll, fatal), 429 → `RateLimit` honoring
  `parameters.retry_after`, network failures → `Network` (transient, retried
  with capped exponential backoff). The last error is inspectable via
  [`TelegramProvider::take_last_error`].

## Usage

```rust
use std::sync::Arc;
use provider_core::{ChatProvider, ProviderEvents, ChannelMessage};
use provider_telegram::TelegramProvider;

struct Sink; // transport implements ProviderEvents
impl ProviderEvents for Sink {
    fn on_message(&self, msg: ChannelMessage) { /* forward to agent */ }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut provider = TelegramProvider::new("YOUR_BOT_TOKEN", Arc::new(Sink));
    provider.start().await?;
    // agent loop ...
    provider.stop().await?;
    Ok(())
}
```

## Configuration (builder)

| Method                        | Default                    | Purpose                                |
| ----------------------------- | -------------------------- | -------------------------------------- |
| `with_base_url`               | `https://api.telegram.org` | API base (self-hosted bot API / tests) |
| `with_poll_interval`          | `1 s`                      | idle delay between `getUpdates` rounds |
| `with_long_poll_timeout_secs` | `30`                       | `getUpdates` `timeout` parameter       |
| `with_request_timeout`        | `60 s`                     | per-request HTTP timeout               |

## Notes

- The update offset cursor persists in-process across `start()`/`stop()` cycles;
  it is advanced _after_ a message is dispatched to the events sink so a crash
  between delivery and ack re-delivers instead of dropping.
- `send()` is text-only (`sendMessage`); attachments are logged and ignored.
  Media outbound is a future milestone (`sendPhoto`/`sendDocument`).
- Telegram file URLs require a `getFile` round-trip; file ids are preserved in
  `ChannelMessage::raw` for the transport.

## Tests

Hand-rolled mock Telegram API on a local `tokio::net::TcpListener` (no external
test deps): update mapping, media, service-message skipping, sendMessage
payloads, 429/401/409 error mapping, double-start guard, and `stop()` cancelling
an in-flight 30 s long-poll.

```bash
cargo test -p provider-telegram
```
