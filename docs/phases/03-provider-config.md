# Phase 03 — Unified provider-config crate

**Lens:** Rspack / Rsbuild + Next.js · **Status:** planned

## Why

`bin/pc/src/config.rs:41` `load(cli_path)` merges `CLI --config → $PC_CONFIG → env(PC_PROVIDERS, PC_<ID>_TOKEN, PC_<ID>_CONFIG via merge_into at config.rs:89)`. `cli/src/config.rs:3` is a verbatim copy ("keep the two in sync"). Every provider adds `PC_<ID>_TOKEN` env + `build_provider` match arm at `bin/pc/src/main.rs:202` — webpack-4-era loaderDX. Need unified `defineConfig` like `rsbuild.config.ts` / `next.config.js`.

## Scope

- New crate `crates/provider-config`: `SidecarConfig` + `load()` merging `pc.config.json | pc.config.toml | env(PC__ nested, via figment/config-rs)` with `schemars` validation; fail closed if `PC_<ID>_CONFIG` not object.
- Both `bin/pc` and `cli` depend on it; delete `cli/src/config.rs`.
- Add `pc.config.ts` TS helper `defineConfig` (types mirror `schema.rs:15`) that can emit JSON for Rust loader.
- Optional: `pc init` scaffolding (Phase 04 or 09).

## Exit criteria

- One config crate, zero duplication; `PC_*_CONFIG` invalid object -> `ProviderError::Config` at startup with path.
- `pc --help` and `cargo test -p provider-config` document all provider keys (today only `bin/pc/src/main.rs:40` CONFIG block).

## Verification

```sh
cargo test -p provider-config
cargo test -p pc --test e2e   # still passes
```

## Files

- `crates/provider-config/src/lib.rs` (new)
- `bin/pc/Cargo.toml:22`, `cli/Cargo.toml:12` depend on it
- `cli/src/config.rs` delete
- `bin/pc/src/config.rs` move/ re-export

## Risks

- `figment` vs `config-rs` choice; prefer small dep set (Bun lens: binary size). Spike both.
