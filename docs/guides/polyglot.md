# Polyglot — Go / Dart / Lua as high-perf watchers

`pc serve` HTTP `crates/provider-transport/src/http.rs:11` `GET /health`, `POST /api/providers/:id/send`, `GET /api/events` SSE + `broadcast::Sender<Outbound>` `crates/provider-transport/src/state.rs:19` is language-agnostic.

## Option A — HTTP/SSE (recommended, no FFI)

### Go

```go
package main

import ("bufio"; "encoding/json"; "net/http"; "os/exec"; "strings")

func main(){
  resp,_:=http.Get("http://127.0.0.1:8788/api/events")
  defer resp.Body.Close()
  sc:=bufio.NewScanner(resp.Body); seen:=map[string]bool{}
  for sc.Scan(){
    l:=sc.Text(); if !strings.HasPrefix(l,"data: ") {continue}
    var evt struct{Method string `json:"method"`; Params struct{Message struct{ID string `json:"id"`; Channel, ChannelID string `json:"channel,channel_id"`; Content []map[string]string `json:"content"`} `json:"message"`} `json:"params"`}
    json.Unmarshal([]byte(l[6:]), &evt)
    if evt.Method!="event.message"||seen[evt.Params.Message.ID]{continue}
    seen[evt.Params.Message.ID]=true
    text:=""; for _,p:=range evt.Params.Message.Content {text+=p["Text"]+" "}
    exec.Command("hermes","--text",text).Start()
    // reply: http.Post("http://127.0.0.1:8788/api/providers/tg-main/send","application/json", strings.NewReader(`{"channel_id":"123","text":"hi"}`))
  }
}
```

### Dart

```dart
import 'dart:convert'; import 'dart:io';
void main() async {
  final req=await HttpClient().getUrl(Uri.parse('http://127.0.0.1:8788/api/events'));
  final resp=await req.close(); final seen=<String>{};
  await for(final line in resp.transform(utf8.decoder).transform(const LineSplitter())){
    if(!line.startsWith('data: ')) continue;
    final evt=jsonDecode(line.substring(6)); if(evt['method']!='event.message') continue;
    final m=evt['params']['message']; if(!seen.add(m['id'])) continue;
    final text=(m['content'] as List).map((p)=>p['Text']??'').join(' ');
    Process.start('hermes',['--text',text]);
  }
}
```

## Option B — FFI `cdylib`

`crates/provider-ffi/src/lib.rs:259` `PcHandle` + `extern "C" pc_init/pc_poll` is the only `unsafe` `docs/architecture.md:84`. Go `cgo` `// #include "pc.h"` + `C.pc_init(cfg)`, Dart `dart:ffi` `DynamicLibrary.open("libpc.so")`. Shares tokio `current_thread` `Cargo.toml:22`. Prefer A unless you need in-process `EventBus` `crates/provider-core/src/client.rs:105`.

## Lua

Use Lua as **config** `docs/guides/config-formats.md` (`pc.config.lua` returns table via `mlua`) or as watcher via `http` + `copas`. Performance note `docs/architecture.md:5` — Lua watcher ~2MB.

## See

* `docs/app-integration.md:194` HTTP section, `docs/guides/spawn-script.md` for shell/Python equivalents.
* `examples/node/index.mjs:64` `JsonRpcClient` STDIO variant if you prefer NDJSON over SSE.
