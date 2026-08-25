# Phase 04 — Single `pc` binary with subcommands

**Lens:** Rspack / Bun · **Status:** planned

## Why

Two binaries (`bin/pc` stdio sidecar + `cli/` standalone workspace at `cli/Cargo.toml:25` empty `[workspace]`) share ~90% provider wiring (`bin/pc/src/main.rs:202` vs `cli/src/providers.rs:21`) and hand-rolled `parse_args` (`bin/pc/src/main.rs:90` + `cli/src/main.rs:141` ~500 LOC). Rsbuild collapsed to one `rsbuild` binary with `dev/build/preview/inspect`; Bun ships one `bun`.

## Scope

- Merge into one `pc` binary (keep `cli/` as re-export shim temporarily, or delete):
  ```
  pc                          # sidecar stdio (default, back-compat)
  pc sidecar [--config path]  # explicit sidecar
  pc send --provider id --chat id --text ...
  pc listen [--providers a,b] [--once] [--timeout 5]
  pc check [--provider id]
  pc serve [--ws :8787] [--http :8788]
  pc init
  ```
- Replace both `parse_args` loops with `clap` `#[derive(Parser)]` (one parser, typed, `--help` generated).
- Single `runtime()` / `init_tracing()`; default no-subcommand -> sidecar.

## Exit criteria

- `pc --help` shows subcommands; `pc send` / `pc listen` replace `pc-connect send/listen`.
- No duplicated `build_provider`; single `provider-config` crate (Phase 03) is sole config entry.
- Existing `opencode-plugin/src/pc-client.ts:82` `PcClient.start(bin)` still works (`bin = "pc"` with no args = sidecar).

## Files

- `bin/pc/src/main.rs:20,90` USAGE + parse
- `cli/src/main.rs:27,141` merge/delete
- `Cargo.toml:3` members (`cli` becomes re-export or removed)
- `crates/provider-config` (Phase 03) consumed here

## Notes

- Keep `provider-transport` feature selection single: `pc` has `telegram/discord/http/ws` features, not two sets.
