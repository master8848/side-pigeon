# Phase 02 — Runtime trim

**Lens:** Bun · **Status:** done (`e9278cc`)

## Why

Idle RSS/size budget (<30 MB) pays for every unused feature. Both binaries use `tokio::Builder::new_current_thread()` (`bin/pc/src/main.rs:160`, `cli/src/main.rs:395`) yet `Cargo.toml:19` pulled `rt-multi-thread`. Broadcast `512` at `state.rs:15` is `512×~1KB` idle overhead per connection at chat rate.

## What shipped

- `Cargo.toml:19` `rt-multi-thread` → `rt`
- `state.rs:15` `OUTBOUND_CAPACITY` `512` → `32`

## Exit criteria

- `cargo build --release -p pc` smaller, `requestAnimationFrame`-free single-thread runtime (no multi-thread scheduler linked).
- Backpressure story explicit: `dropped_frames_notification(-32006)` + lag handling already in place from Phase 01.

## Next step (follow-up, not this commit)

- `mimalloc`/`jemalloc` evaluation (allocator is 5–10 MB RSS on Linux; `docs/architecture.md:74` TODO).
