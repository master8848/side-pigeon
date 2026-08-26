# Security audit — 26-08-2026 (subagents)

Separate from features `docs/IMPROVEMENTS.md:18` and docs polish `docs/IMPROVEMENTS.md:66`.

| # | Bug | Severity | File:line | Fix shard |
|---|---|---|---|---|
| S1 | `0.0.0.0` default bind vs `localhost-only` docs | Critical | `bin/pc/src/main.rs:1048`, `:1068`, `crates/provider-transport/src/http.rs:13` | default `127.0.0.1`, `--public` flag |
| S2 | CORS `contains` bypass `http://evil.com?127.0.0.1` | High | `crates/provider-transport/src/http.rs:108`, `ws.rs:23` | `Url::parse` exact host |
| S3 | No auth on `/rpc`, `/api/providers/:id/send`, `?since=` | High | `crates/provider-transport/src/http.rs:287`, `:362`, `:182` | `PC_AUTH_TOKEN` bearer |
| S4 | Body `collect` before `MAX_BODY_BYTES` `1MiB` + WS/stdio unbounded | High | `crates/provider-transport/src/http.rs:302`, `ws.rs:153`, `stdio.rs:25` | early `Content-Length` + `Limited`, `max_message_size 1MiB` |
| S5 | `persist` traversal `../../` + `file:` URI `?mode=memory` + `0644` + symlink TOCTOU | High | `crates/provider-transport/src/persist.rs:25`, `bin/pc/src/main.rs:1109`, `:216` | `canonicalize` + `O_NOFOLLOW|O_EXCL` + `0600` |
| S6 | Supply gate `created_at` not version `created_at` bypass | High | `scripts/check-supply-chain.sh:43` | query `/$name/$version` |
| S7 | Sync WAL `append` blocks gateway `0.5-5ms` + unbounded file | High | `crates/provider-transport/src/persist.rs:49`, `state.rs:363` | `mpsc` writer + `prune` + `journal_size_limit 32M` |
| S8 | `replay_since` `limit=None` unbounded OOM | Medium | `crates/provider-transport/src/persist.rs:61`, `crates/provider-ffi/src/persist.rs:59` | hard `LIMIT 1000` |
| S9 | `Authorization` header advertised not enforced | Medium | `crates/provider-transport/src/http.rs:119` | enforce or drop |
| S10 | `MAX_BODY_BYTES` after alloc | Medium | `crates/provider-transport/src/http.rs:314` | pre-check |

Plus: `http/ws` no `Authorization` on SSE `http.rs:182`, `query_param` hand-rolled `http.rs:93`, `base64` per-attachment `5MiB` `crates/provider-core/src/schema.rs:93` no aggregate, `SendMessage` `#[serde(default)]` empty `channel_id` `crates/provider-core/src/schema.rs:152`.

Zero `unsafe` outside `provider-ffi` `crates/provider-ffi/src/lib.rs:396` is correct `docs/architecture.md:84`, but 3 crates miss `forbid(unsafe_code)` `crates/provider-telegram/src/lib.rs:1`.
