# Phase 09 — Polish & release

**Lens:** all · **Status:** planned

## Why

The vision's finish line is release-ready docs, supply chain, and DX like `create-next-app`/`create-rsbuild`/`bun create`.

## Scope

- DX: `create-pc-app` / `pc init` scaffold (from Phase 03/04), `pc dev` alias (Phase 08), `defineConfig` typed helper for `pc.config.ts`.
- Cache/revalidation: replace `runtime.ts:177` `recentIds` sort-eviction with TTL `lru-cache` (`ttl:5min`) + `revalidateTag(chatKey)` after `send` (Next.js ISR analog); Next/Bun lenses both flag current LRU.
- Docs: `README.md` (today 9 lines) → install, `pc.config.ts` example, `pc serve` diagram, provider matrix (telegram/discord/demo), limitations; `docs/api-contract.md:1` + `architecture.md` updated for new transports/adapters.
- Supply chain: `scripts/check-supply-chain.sh` extended for new crates/features, `docs/supply-chain.md` updated, `base64` + `figment`/`clap` dep ages checked (published >=14d).
- Devtools: `provider-devtools` subscribing to `EventBus` (TankStack devtools analog) — optional, can slip.
- Release: `cargo publish` order (`provider-core` → `provider-transport` → `provider-*` → `pc` + `provider-config` → `provider-ffi`), npm `packages/*` publish with prebuilt `pc-{os}-{arch}` artifacts.

## Exit criteria

- `pc init && pc serve` zero-config path exists.
- `cargo test` + `bun test` (CLI + TS core) + `bin/pc/tests/e2e.rs` green in CI; binary size/idle RSS regression guard.
- `docs/phases/README.md` table all phases **done**.

## Files

- `README.md`, `docs/{architecture.md,api-contract.md,supply-chain.md}`, `scripts/check-supply-chain.sh`
- `Cargo.toml:11` workspace version, `bin/pc` release profile (`LTO thin` etc. at `Cargo.toml:29`)
- `packages/*/package.json` version alignment
