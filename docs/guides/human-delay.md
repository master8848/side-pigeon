# Human delay — lazy 2m buffer, coalesce, immediate `/now`

Normal bots reply aggressively to the first message. Humans batch. This plugin makes even the *first* message wait.

## Behavior

* Buffer per `chatKey = ${channel}:${channel_id}` `crates/provider-core/src/client.rs:60` for `delay=120s`.
* 3 messages in 2m coalesce `text = msgs.map(c=>content).join("\n")` -> one `hermes` spawn.
* Immediate shortcut: if any `content.Text` starts with `/now` or `!send`, flush instantly (skip delay).
* `delay_first=true` (default) — even a single message waits the full 2m.

## Watcher JS (no Rust)

```js
const DELAY=120_000; const buf=new Map(); // k->{msgs,timer}
function onMsg(m){
  const text=m.content.map(p=>p.Text??"").join(" ");
  if(text.startsWith("/now")||text.startsWith("!send")) return flush(m);
  const k=`${m.channel}:${m.channel_id}`; let e=buf.get(k)??{msgs:[]}; e.msgs.push(m);
  clearTimeout(e.timer); e.timer=setTimeout(()=>{ spawn(buf.get(k).msgs.splice(0)); buf.delete(k)}, DELAY); buf.set(k,e);
}
```

## Rust `DebouncePlugin` (like `DedupPlugin` `crates/provider-core/src/plugin.rs:66`)

```rust
pub struct DebouncePlugin{ delay: Duration, immediate: Vec<String>, buf: Mutex<HashMap<String, Vec<ChannelMessage>>> }
impl Plugin for DebouncePlugin{
  fn on_message(&self, m:&mut ChannelMessage)->ControlFlow{
    if self.is_immediate(m){ self.flush(&key(m)); return ControlFlow::Drop; }
    self.buffer(m); ControlFlow::Drop // hold, spawn via timer task
  }
}
```

Enable only with flag: `pc hermes --debounce 120 --immediate "/now,!send"` -> `AppState::with_plugin` `crates/provider-transport/src/state.rs:153`.

## Persistence

Run `pc serve --persist` so `GET /api/events?since=` `crates/provider-transport/src/persist.rs:61` replays if the debounced `hermes` crashes before flush.
