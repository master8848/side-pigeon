# Config formats — JSON / JSONC / TOML / Lua

## Today (shipped)

`crates/provider-config/src/lib.rs:53` `SidecarConfig{providers:[{id,config}]}` + env `PC_PROVIDERS`, `PC_<ID>_TOKEN`, `PC_<ID>_CONFIG` must be JSON object `README.md:79`. `pc.config.ts` `defineConfig` is type stub only `README.md:153` — Rust never loads TS.

## Planned (Cargo features `toml,jsonc,lua`)

| File | Load | Dep |
|---|---|---|
| `pc.config.json` | `serde_json` today `provider-config/src/lib.rs:53` | `serde_json` |
| `pc.config.jsonc` | strip `//`,`/* */`, trailing `,` then `serde_json` | `jsonc-parser` |
| `pc.config.toml` | `toml` crate -> `Value` | `toml` |
| `pc.config.lua` | `mlua` `Lua::load(file).eval::<mlua::Table>()` -> `Value` | `mlua` vendored |

Precedence: `CLI --config path` `bin/pc/src/main.rs:40` > `PC_CONFIG` env > `pc.config.{json,jsonc,toml,lua}` in CWD > env fallback `provider-config/src/lib.rs:63`. `PC_<ID>_CONFIG` JSON object merges `merge_into` `provider-config/src/lib.rs:94`.

## Examples

```toml
# pc.config.toml
[[providers]]
id="tg-main"
[providers.config]
kind="telegram"
token="123:aaa"
poll_interval_secs=1
```

```lua
-- pc.config.lua — return table
return { providers = { { id="tg-main", config={ kind="telegram", token=os.getenv("TG_TOKEN") } } } }
```

```jsonc
// pc.config.jsonc — comments + trailing commas allowed
{ "providers": [{ "id": "demo", }] }
```

## Security

`pc.config.json` is `0644` today `bin/pc/src/main.rs:216` leaks token; fix to `0600` + `0700` dir `crates/provider-transport/src/persist.rs:28` + `O_NOFOLLOW`. Tokens via `PC_*_TOKEN` env preferred. Fail-closed on non-object `PC_<ID>_CONFIG` `provider-config/src/lib.rs:78`.

## Watch

`--watch` polls `pc.config.*` mtimes `bin/pc/src/main.rs:1214` every 1s (stub, no hot-reload). Restart `pc` to apply after edit `docs/guides/hermes-on-demand.md:345`.
