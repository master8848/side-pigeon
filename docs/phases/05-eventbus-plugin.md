# Phase 05 — Typed EventBus + Plugin pipeline

**Lens:** TankStack · **Status:** planned

## Why

`traits.rs:36` `ProviderEvents::on_message(ChannelMessage)` is sync, global, unfilterable, un-unsubscribable. `state.rs:43` `broadcast::Sender<Outbound>` is single global `512` (now `32` from Phase 02) with `Lagged -> dropped_frames_notification`. Policy (`recentIds`/`recentSent` dedup + `ignoreSenderIds` + `rooms` allowlist) lives scattered in `plugins/opencode-plugin/src/runtime.ts:77,168` and `plugins/pi-plugin/pc_connect.py:654`. TankStack solved this with `query-core` headless + middleware/observer subscriptions.

## Scope

### 1) Headless client

- New `crates/provider-core/src/client.rs`:
  ```rust
  pub struct ProviderClient { bus: EventBus, registry: ProviderRegistry, plugins: Vec<Box<dyn Plugin>> }
  impl ProviderClient {
      pub fn builder() -> ProviderClientBuilder { ... }
      pub fn subscribe(&self, filter: EventFilter, cb: impl Fn(&ChannelMessage)+Send+Sync+'static) -> Subscription;
      pub fn send(&self, provider: ProviderId, msg: SendMessage) -> Mutation<SendReceipt>;
      pub fn use_plugin<P: Plugin>(&mut self, p: P);
  }
  // EventFilter { provider: Option<ProviderId>, channel_id: Option<String>, explicitly_addressed: Option<bool> }
  // Subscription = RAII unsubscribe handle
  ```

### 2) Plugin trait

- `crates/provider-core/src/plugin.rs`:
  ```rust
  pub trait Plugin: Send+Sync {
      fn on_message(&self, msg: &mut ChannelMessage) -> ControlFlow; // Continue/Drop/Rewrite
      fn on_send(&self, msg: &mut SendMessage) -> ControlFlow;
      fn on_error(&self, provider: &str, err: &ProviderError) {}
  }
  // Built-ins: DedupPlugin{window}, AllowListPlugin{rooms}, RetryPlugin{max:3}, LoggerPlugin
  ```
- `EventBus::publish` runs `on_message` chain before fanout; `Registry::send` runs `on_send` chain.
- `ProviderRegistry` keeps string-id API; `ProviderClientBuilder::provider(factory)` merges typed config.

### 3) Integration

- `AppState::with_client(client)` delegates `dispatch_message/dispatch_error` at `registry.rs:183` to `client.bus.publish`.
- Move `runtime.ts:168` `handleMessage` dedup/allowlist into `DedupPlugin`/`AllowListPlugin` so `runtime.ts` shrinks to `client.subscribe -> sessionMap`.

## Exit criteria

- `client.subscribe({provider:"telegram", explicitlyAddressed:true}, cb) => unsubscribe` typed, filterable, per-subscriber bounded queue (no global loss).
- Cargo feature = compile gate, `Plugin` = runtime composition (no more forking provider for pacing).
- Existing `Transport` tests pass via new path; new `provider-core::client::tests::subscribe_filter` + `plugin::tests::allowlist_blocks, dedup_suppresses_replay` (port `plugins/opencode-plugin/test/runtime.test.ts` cases).

## Files

- `crates/provider-core/src/client.rs` (new), `plugin.rs` (new), `lib.rs:1` exports
- `crates/provider-transport/src/state.rs:39` `AppState` constructor delegates to `ProviderClient`
- `crates/provider-core/src/registry.rs:22` stays, wrapped by `ProviderClient`
