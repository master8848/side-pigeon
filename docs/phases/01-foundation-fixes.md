# Phase 01 — Foundation fixes (review P0/P1)

**Lens:** architecture review · **Status:** done

## Why this phase exists

Pre-vision correctness debt flagged by the supply-chain / architecture review. Must land before any vision work or later phases lie.

## What shipped

| Commit    | What                                                                                       | Files                                                                                                                                                                                                                                                                                                                                                                                          |
| --------- | ------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `e04ec4a` | Base64 wire codec for `MediaAttachment.data` (contract promised base64, shipped raw array) | `provider-core/src/schema.rs:88` + `base64_bytes` mod, `Cargo.toml` `base64 0.22` direct dep, `serde_roundtrip.rs` wire assertion                                                                                                                                                                                                                                                              |
| `430a8ca` | Async error events + honest backpressure                                                   | `traits.rs:10` `TRANSIENT_ERROR_EVENT_THRESHOLD=10`, `registry.rs:183` `dispatch_error`, `state.rs:237` `NotifyEvents::on_error`, `state.rs:273` `dropped_frames_notification(-32006)`, `stdio.rs:74` + `ws.rs:67` bounded queue `1024` with close-on-overflow, telegram jittered backoff + threshold event, discord `classify_close` (fatal 4004/4010-4014), `default-features=false` hygiene |
| `a20911b` | Provider config blob wired to builders                                                     | `bin/pc/src/main.rs:202` `build_provider` applies `base_url/poll_interval/long_poll_timeout/request_timeout` (telegram) + `gateway_url/rest_base/intents/request_timeout` (discord) via `config_str/config_u64`; `e2e.rs` stdio harness                                                                                                                                                        |

## Exit criteria

- `MediaAttachment.data` round-trips as base64 on wire.
- Fatal + sustained transient provider errors emit `event.error`; `capabilities_value` honest (`state.rs:142` only `event.message`+`event.error`, `features:["send"]`).
- `PC_<ID>_CONFIG` blob actually reaches providers; e2e `initialize→capabilities→listen→send→shutdown` passes on demo provider.

## Verification

```sh
cargo test -p provider-core
cargo test -p provider-transport
cargo test -p pc --test e2e
```
