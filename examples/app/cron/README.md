# examples/app/cron — cron / CLI using @provider-connect/core

Two one-shot modes (no daemon), same `pc-connect` contract via the core client:

```bash
# one-shot send (mirrors `pc-connect send`)
node notify.mjs --send --provider demo --chat room --text "deploy done"
PC_TELEGRAM_TOKEN=123:abc node notify.mjs --send --provider telegram --chat -100123 --text "hi"

# one-shot poll (mirrors `pc-connect listen --once --timeout 10`)
node notify.mjs --poll --timeout 10 --provider demo
```

Exits after receipt (or JSON error with sidecar code `-32001..-32603`) in
`--send` mode, or after the first `event.message` / `event.error` / timeout in
`--poll` mode — with the `dedup()` plugin so retries don't double-deliver.

When to use daemon vs cron: for **sending**, cron/one-shot is fine; for
**receiving** you need a long-lived process — either this poll in a supervised
loop or the `pc` sidecar daemon (every stop has data loss; see
`cli/README.md` data-loss table and the receiving matrix once
`docs/app-integration.md` lands). For a persistent app, use
`examples/app/express`.
