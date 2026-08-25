# examples/app — normal application examples (use @mbsks/side-pigeon)

App developers: **prefer `@mbsks/side-pigeon`** over raw JSON-RPC.
`examples/node` is the low-level NDJSON wire reference; these are the
copy-paste starters for real apps.

| Example              | What                                                                            | Run                                                                 |
| -------------------- | ------------------------------------------------------------------------------- | ------------------------------------------------------------------- |
| [express/](express/) | HTTP server — `subscribe` → SSE, `POST /send`, `GET /health`, graceful shutdown | `PC_PROVIDERS=demo node express/server.mjs`                         |
| [cron/](cron/)       | Cron/CLI — one-shot `send` or `poll --timeout` with `dedup`                     | `node cron/notify.mjs --send --provider demo --chat room --text hi` |

Install: `bun add @mbsks/side-pigeon` (or `npm add`).

See also: `packages/core` exports (`createProviderClient`, `stdio`, `dedup`),
`docs/api-contract.md` for the wire methods, and `cli/README.md` for the
`pc-connect` one-shot contract these examples mirror in-process.
