# Spawn any script on message — no Rust required

`pc` holds the provider connections (Telegram long-poll `crates/provider-telegram/src/lib.rs`, Discord gateway `crates/provider-discord/src/gateway.rs`). Your script just subscribes.

## `pc serve` once

```sh
cargo build -p pc --features telegram,discord,http
pc serve --http 127.0.0.1:8788 --persist ./pc-events.db &
curl -s http://127.0.0.1:8788/health | jq .  # capabilities_value() crates/provider-transport/src/state.rs:252
```

`--persist` enables replay `GET /api/events?since=CURSOR&limit=500` `crates/provider-transport/src/persist.rs:61` capped 1000 `http.rs:211`.

## Watcher — shell (10 lines)

```sh
curl -N http://127.0.0.1:8788/api/events | while IFS= read -r line; do
  [[ $line == data:* ]] || continue
  msg=$(echo "${line:6}" | jq -c 'select(.method=="event.message") | .params.message')
  [ -z "$msg" ] && continue
  echo "$msg" | python3 ./hermes_spawn.py &  # or bun watcher.mjs, lua, go
done
```

## Watcher — Python (stdlib)

```python
import json, urllib.request, subprocess
seen=set()
with urllib.request.urlopen("http://127.0.0.1:8788/api/events") as r:
  buf=""
  while True:
    buf+=r.read(1024).decode()
    while "\n\n" in buf:
      frame,buf=buf.split("\n\n",1)
      for l in frame.splitlines():
        if not l.startswith("data: "): continue
        evt=json.loads(l[6:])
        if evt.get("method")!="event.message": continue
        m=evt["params"]["message"]
        if m["id"] in seen: continue
        seen.add(m["id"])
        text=" ".join(p.get("Text","") for p in m.get("content",[]))
        subprocess.Popen(["hermes","run","--text",text])
```

Dedup by `message.id` `crates/provider-core/src/plugin.rs:47` required — reconnection replays last frame `http.rs:264`.

## Reply

```sh
curl -s http://127.0.0.1:8788/api/providers/tg-main/send \
  -H 'content-type: application/json' \
  -d '{"channel_id":"123","text":"hi","reply_to":"msg-id"}' | jq .
# or: pc send --provider tg-main --chat 123 --text "hi"
```

Future built-in `pc serve --on-message-exec "hermes --stdin"` `docs/guides/hermes-on-demand.md:240` is spec — watcher is the same without Rust.
