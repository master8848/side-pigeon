# Improvements — Features vs Security vs Polish vs Docs

> One doc to track everything from the 26-08-2026 audits (9 subagents). Split deliberately so `Features` ships value, `Security` ships safe, `Polish` ships quality, `Docs` ships clarity. Each item has file:line + priority + status.

---

## 1. Features (user-requested, not bugs)

| # | Feature | What changes | Files touched | Priority | Status |
|---|---|---|---|---|---|
| F1 | **Multi-bot per provider** — 2x Telegram / 2x Discord on one `pc` | Split `id` (alias) vs `config.kind` (driver). `build_provider` reads `config["kind"].unwrap_or(id)`, `fn id()->&'static str` leaks alias via `Box::leak`. `pc.config.json` example: `[{"id":"tg-main","config":{"kind":"telegram","token":"..."}},{"id":"tg-ops","config":{"kind":"telegram","token":"..."}}]`. Routing via `EventFilter{provider:"tg-ops"}` `crates/provider-core/src/client.rs:22`. Fixes `crates/provider-core/src/registry.rs:51` duplicate check + `bin/pc/src/main.rs:977` match. | `crates/provider-config/src/lib.rs:21`, `crates/provider-core/src/registry.rs:51`, `bin/pc/src/main.rs:977`, `crates/provider-ffi/src/lib.rs:189`, `crates/provider-core/src/client.rs:22` | P0 | Spec — `docs/guides/multi-bot-routing.md` |
| F2 | **Spawn any script (no Rust)** — wake `hermes`/other app per `event.message` | Documented via `pc serve` `GET /api/events` SSE `crates/provider-transport/src/http.rs:182` + `POST /api/providers/:id/send`. Shell/Python/Node watcher loops `curl -N` -> `Popen`. Future `pc serve --on-message-exec "hermes --stdin"` `docs/guides/hermes-on-demand.md:240` (stub today). No Rust `Plugin` required. | `crates/provider-transport/src/http.rs:182`, `crates/provider-transport/src/state.rs:205`, `bin/pc/src/main.rs:1140` | P0 | Works today via watcher — `docs/guides/spawn-script.md` |
| F3 | **Human delay / debounce** — 2m buffer, coalesce 3 msgs, `/now` immediate | New `DebouncePlugin{delay:120s, immediate:["/now","!send"]}` `crates/provider-core/src/plugin.rs:34` style; also watcher JS `Map<chatKey,{msgs,timer}>`. Even first message waits 2m (user confirmed). Flush coalesced `content.join("\n")` -> single `hermes` spawn. Needs `persist` `docs/persistence.md` so buffered replay survives crash. | `crates/provider-core/src/plugin.rs:18`, `crates/provider-core/src/client.rs:158`, `crates/provider-transport/src/persist.rs:49` | P1 | New — `docs/guides/human-delay.md` |
| F4 | **Idle auto-kill** — kill `hermes` 5m after last activity | New `IdleKillPlugin{ttl:300s}` behind `--idle-kill` flag `bin/pc/src/main.rs:108` style. `on_message`/`on_send` touch `HashMap<chatKey,(Child,Instant)>`, background `tokio::time::sleep(ttl)` -> `child.kill()`. Only enabled when flag set `AppState::with_plugin` `crates/provider-transport/src/state.rs:153`. | `crates/provider-core/src/plugin.rs:34`, `crates/provider-transport/src/state.rs:153`, `bin/pc/src/main.rs:108` | P1 | New — `docs/guides/idle-autokill.md` |
| F5 | **Polyglot Go/Dart (high-perf)** | Two surfaces: A) HTTP/SSE `pc serve` — any lang `POST /api/providers/:id/send`, `GET /api/events` (Go `net/http`, Dart `HttpClient`+`Process.start`). B) FFI `provider-ffi` `crates/provider-ffi/src/lib.rs:259` `cdylib`+`C ABI` via `cgo`/`dart:ffi`. Add `examples/go/`, `examples/dart/`. | `crates/provider-ffi/src/lib.rs:259`, `crates/provider-transport/src/http.rs:11`, `crates/provider-transport/src/state.rs:43` | P1 | New — `docs/guides/polyglot.md` |
| F6 | **Config formats: TOML / JSONC / Lua** | `provider-config` `crates/provider-config/src/lib.rs:53` currently JSON only `serde_json`. Add `toml` crate (`pc.config.toml`), `jsonc` strip `//`/`/* */`, `mlua` (`pc.config.lua` returns table -> `Value`). Behind `toml,jsonc,lua` Cargo features, precedence `CLI --config` > `PC_CONFIG` > `pc.config.{json,toml,lua}` > env. | `crates/provider-config/src/lib.rs:53`, `bin/pc/src/main.rs:235` | P1 | Planned — `docs/guides/config-formats.md` |
| F7 | **Delayed first-message + immediate shortcut** | Same as F3 but explicitly even first message delayed (user: `for first message this would be even better`). Config `debounce_first=true`. | `crates/provider-core/src/plugin.rs:101` | P1 | Spec — see F3 |

---

## 2. Security bugs (must fix before publish)

| # | Bug | Severity | File:line | Fix |
|---|---|---|---|---|
| S1 | Default binds `0.0.0.0:8787/8788` despite docs `localhost-only` | Critical | `bin/pc/src/main.rs:1048`, `:1068`, `crates/provider-transport/src/http.rs:13` | Default `127.0.0.1`, require `--public` with `warn!` if `is_unspecified()` |
| S2 | CORS `origin.contains("localhost")` bypass `http://evil.com?localhost` | High | `crates/provider-transport/src/http.rs:108`, `ws.rs:23` | Parse `Url`, exact host `127.0.0.1/localhost/[::1]` |
| S3 | No auth on `/rpc`, `/api/providers/:id/send`, `?since=` replay | High | `crates/provider-transport/src/http.rs:287`, `:362`, `:182` | `PC_AUTH_TOKEN` bearer check opt-in, `401` |
| S4 | Body `collect()` before `MAX_BODY_BYTES` (1MiB `http.rs:106`) + WS/stdio unbounded | High | `crates/provider-transport/src/http.rs:302`, `ws.rs:153`, `stdio.rs:25` | Early `Content-Length` check + `Limited`, `max_message_size 1MiB` |
| S5 | `persist` path traversal/URI `file:...?mode=memory` + `0644` + symlink TOCTOU | High | `crates/provider-transport/src/persist.rs:25`, `bin/pc/src/main.rs:1109`, `:216` | Reject `..`,`:`,`?`, `canonicalize`, `O_NOFOLLOW|O_EXCL`, `chmod 0600/0700` |
| S6 | Supply-chain gate checks crate `created_at` not version `created_at` | High | `scripts/check-supply-chain.sh:43` | Query `/api/v1/crates/$name/$version` |
| S7 | Sync WAL `log.append` blocks gateway/poll 0.5-5ms + unbounded growth | High | `crates/provider-transport/src/persist.rs:49`, `crates/provider-transport/src/state.rs:363` | `mpsc` writer thread + `prune` + `journal_size_limit` |
| S8 | Unbounded `replay_since` when `limit=None`, 10M rows OOM | Medium | `crates/provider-transport/src/persist.rs:61`, `crates/provider-ffi/src/persist.rs:59` | Hard `LIMIT 1000` even when `None` |
| S9 | No `Authorization` on SSE replay but header advertises it | Medium | `crates/provider-transport/src/http.rs:119` `allow-headers: authorization` | Either enforce or drop header |
| S10 | `MAX_BODY_BYTES` after `collect` allows 500MiB allocate | Medium | `crates/provider-transport/src/http.rs:314` | Pre-check |

---

## 3. Polish (quality debt — ship after security)

| # | Debt | File:line | Effort |
|---|---|---|---|
| P1 | 3-way `DemoProvider` copy | `bin/pc/src/demo.rs`, `cli/src/demo.rs`, `crates/provider-ffi/src/lib.rs:13` | 1h -> `crates/provider-demo` |
| P2 | Triplicate `build_provider` drift (`baseUrl` alias only in FFI) | `bin/pc/src/main.rs:977`, `cli/src/providers.rs:21`, `crates/provider-ffi/src/lib.rs:189` | 1h -> `provider-config::factory` |
| P3 | `Mutex::lock().unwrap()` should be `unwrap_or_else(\|e\|e.into_inner())` | `crates/provider-telegram/src/lib.rs:389`, `crates/provider-discord/src/lib.rs:186` | 10m |
| P4 | `AppState` global `Mutex` held across `await` serializes `send` | `crates/provider-transport/src/state.rs:205`, `bin/pc/src/main.rs:1140` | 30m `RwLock` |
| P5 | `tokio test-util` in prod | `crates/provider-discord/Cargo.toml:10` | 2m -> `[dev-dependencies]` |
| P6 | `OUTBOUND_CAPACITY 32` + sync dedup `O(N)` `retain`+`min_by_key` | `crates/provider-transport/src/state.rs:19`, `crates/provider-core/src/plugin.rs:109` | 1h `LruCache` |
| P7 | `bin/pc/src/main.rs` 1247 lines god file | `bin/pc/src/main.rs:1` | 30m split `ops.rs/serve.rs` |
| P8 | Missing `forbid(unsafe_code)`/`warn(missing_docs)` 3 crates | `crates/provider-telegram/src/lib.rs:1`, `provider-discord`, `provider-config` | 5m |
| P9 | `cli` standalone workspace not gated by `check-supply-chain.sh` | `cli/Cargo.toml:25`, `scripts/check-supply-chain.sh:7` | 10m |
| P10 | Dual `EventBus` vs `broadcast` (`BridgeEvents` twice) | `crates/provider-core/src/client.rs:105`, `crates/provider-transport/src/state.rs:393` | 1h unify |

---

## 4. Docs (human-friendly — parallel to polish)

| # | Doc | Problem today | Fix |
|---|---|---|---|
| D1 | `README.md` hero + install contradicts app-integration | Lists Slack not shipped `README.md:3`, `pc serve` stdio fan-out wording `README.md:34`, TS stub lie `README.md:64` | Rewrite hero, 30s picker, feature table, point to `docs/guides/*` |
| D2 | `docs/architecture.md` draft 2026-08-11 stale | `PENDING merge` `docs/architecture.md:49`, `Build order (tomorrow)` `:60` | Mark persist done, expand §5 from `research/rust-ecosystem.md` |
| D3 | `docs/api-contract.md` missing `?since`, `limit`, `-32006` | `docs/api-contract.md:52` | Add SSE `?since`, replay shape, error codes |
| D4 | `docs/app-integration.md` in-memory stale vs shipped persist | `:277` vs `README.md:96` | Reconcile matrix `cli/README.md:24` |
| D5 | `docs/guides/hermes-on-demand.md` `--sqlite` typo, watch stub | `:247`, `:345` | Rename `--persist`, mark stub |
| D6 | New `docs/guides/multi-bot-routing.md` | missing | F1 |
| D7 | New `docs/guides/spawn-script.md` | missing | F2 |
| D8 | New `docs/guides/human-delay.md` | missing | F3/F7 |
| D9 | New `docs/guides/idle-autokill.md` | missing | F4 |
| D10 | New `docs/guides/polyglot.md` | Go/Dart only JS/Python today | F5 |
| D11 | New `docs/guides/config-formats.md` | TOML/Lua missing | F6 |

---

## 5. Order of work

1. **Security S1-S7** (blocks publish, <1 day)
2. **Features F1+F2** (unblocks your hermes on-demand, 2h) + **Docs D6-D8** (parallel)
3. **Polish P1-P4** (CR before publish)
4. **Features F5+F6 + Docs D10-D11** (follow-up)
5. `cargo fmt --check && cargo clippy --workspace --all-features -- -D warnings && cargo test` + `bun run lint typecheck` `ci.yml:21` gate + `cargo-deny` + refresh `docs/supply-chain.md`
