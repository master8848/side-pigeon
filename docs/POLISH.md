# Polish / Code quality — publish gate

Separate from `docs/SECURITY.md` and features `docs/IMPROVEMENTS.md:18`.

| # | Debt | File:line | Fix |
|---|---|---|---|
| P1 | `Duplicate DemoProvider` x3 | `bin/pc/src/demo.rs`, `cli/src/demo.rs`, `crates/provider-ffi/src/lib.rs:13` | `crates/provider-demo` crate |
| P2 | Duplicate `build_provider` drift `baseUrl` only in FFI | `bin/pc/src/main.rs:977`, `cli/src/providers.rs:21`, `crates/provider-ffi/src/lib.rs:189` | `provider-config::factory` |
| P3 | `lock().unwrap()` should be `unwrap_or_else(\|e\|e.into_inner())` 14 sites | `crates/provider-telegram/src/lib.rs:389`, `crates/provider-discord/src/lib.rs:186`, `crates/provider-transport/src/http.rs:82` `expect` | migrate |
| P4 | `AppState` global `Mutex` held across `await` `handle_request &mut self` serializes `send` | `crates/provider-transport/src/state.rs:205`, `bin/pc/src/main.rs:1140` | `RwLock` + per-provider `Mutex` |
| P5 | `tokio test-util` in prod | `crates/provider-discord/Cargo.toml:10` | `[dev-dependencies]` |
| P6 | Dedup `O(N)` `retain`+`min_by_key` on every msg `2000` entries + `fmt` alloc `key={}:{} ` | `crates/provider-core/src/plugin.rs:109`, `:102`, `:119` | `LruCache` + `FxHash` |
| P7 | `bin/pc/src/main.rs` 1247 lines god file | `bin/pc/src/main.rs:1` | split `ops.rs/serve.rs` |
| P8 | Missing `forbid(unsafe_code)`+`warn(missing_docs)` 3 crates | `crates/provider-telegram/src/lib.rs:1` | add |
| P9 | `cli` standalone not gated `check-supply-chain.sh` | `cli/Cargo.toml:25`, `scripts/check-supply-chain.sh:7` | scan both workspaces |
| P10 | Dual `EventBus` vs `broadcast` `BridgeEvents` x2 + `-32006` outside range | `crates/provider-core/src/client.rs:105`, `crates/provider-transport/src/state.rs:393`, `:451` | unify behind adapter |
| P11 | `ws` per-conn `mpsc 1024` vs `broadcast 32` asymmetry + `MAX_CONNECTIONS 256` no per-IP | `crates/provider-transport/src/ws.rs:111`, `http.rs:44` | 256 + `try_send` |
| P12 | TS `oxlint` `pedantic:off` + `pi-plugin` excluded `typecheck` + `SessionMap` writeChain stuck | `.oxlintrc.json:7`, `package.json:12`, `plugins/opencode-plugin/src/session-map.ts:86` | re-enable, include |

Run: `cargo fmt --check && cargo clippy --workspace --all-features -- -D warnings && cargo test && bun run lint typecheck` `ci.yml:21` + `cargo-deny` + refresh `docs/supply-chain.md` 114->`cargo metadata --all-features`.
