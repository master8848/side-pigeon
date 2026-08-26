# Idle auto-kill — kill `hermes` 5m after last activity

Heavy `hermes` should be 0 MB idle `docs/guides/hermes-on-demand.md:3`. `pc` (~30MB `docs/architecture.md:5`) stays alive.

## Model

One `Child` per `chatKey`. Touch on `on_message` or `on_send` `crates/provider-core/src/plugin.rs:34`.

```rust
pub struct IdleKillPlugin{ ttl: Duration, map: Mutex<HashMap<String,(Child,Instant)>> }
impl Plugin for IdleKillPlugin{
  fn on_message(&self, m:&mut ChannelMessage)->ControlFlow{ self.touch(&key(m)); ControlFlow::Continue }
  fn on_send(&self, m:&mut SendMessage)->ControlFlow{ self.touch(&m.channel_id); ControlFlow::Continue }
  fn on_error(&self, _p:&str, _e:&ProviderError){ /* ignore */ }
}
fn touch(&self, k:&str){
  let mut g=self.map.lock().unwrap_or_else(|e|e.into_inner());
  let entry=g.entry(k.into()).or_insert_with(|| spawn_hermes(k));
  entry.1=Instant::now();
  tokio::spawn({let ttl=self.ttl; let k=k.to_string(); async move{
    sleep(ttl).await; // check `duration_since` still >ttl then kill
  }});
}
```

## CLI — flag gated

```sh
pc hermes --idle-kill 300 # 5m, only when flag set
pc hermes --idle-kill 0   # disabled (default)
```

Wiring `bin/pc/src/main.rs:108` style:

```rust
if args.idle_kill_secs>0 { state.with_plugin(IdleKillPlugin::new(Duration::from_secs(args.idle_kill_secs))); }
```

`tokio::runtime::Builder::new_current_thread` `bin/pc/src/main.rs:255` — timer runs on same thread, `child.kill()` via `tokio::process::Command`.

## Notes

* Use `persist` so `event.message` replay `crates/provider-transport/src/persist.rs:61` survives kill.
* Per-chat TTL avoids killing active chat because another chat is idle.
* `LoggerPlugin` `crates/provider-core/src/plugin.rs:194` shows hook style.
