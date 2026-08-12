# provider-connect — Architecture Review

> Status: review of the v0.1 implementation (workspace at `crates/`, `bin/pc`,
> commits `344068c..c21c552`). Read-only review of all `src/`, `docs/`,
> `scripts/`, `examples/`, `Cargo.toml`, `Cargo.lock`; only this file was
> written. All verification below was re-run on 2026-08-12 (rustc 1.97.1).

## 0. Verification evidence (re-run during this review)

| Check | Command | Result |
|---|---|---|
| Tests | `cargo test --workspace --all-features` | **PASS** — 57 tests + 2 doc-tests, 0 failures (core 7, telegram 9, discord 14, transport stdio 13 / ws 1 / http 1, bin config 2, doc-tests 2) |
| Lint | `cargo clippy --workspace --all-features --all-targets` | **PASS** — 0 warnings |
| Format | `cargo fmt --all --check` | **FAIL** — 25 diffs (provider-telegram 6, provider-discord 19, all in test code) |
| Feature matrix | `cargo check -p provider-core --no-default-features`, `provider-transport` `--no-default-features` / `--features http` / `--features ws` | **PASS** |
| Supply chain | `./scripts/check-supply-chain.sh` | **PASS** — 159 registry crates, all ≥ 14 d old; youngest `zmij` 237 d |
| Memory | `/usr/bin/time -l ./target/release/pc --help` / idle (3 s stdin sleep) | **PASS** — 1.64 MB / 2.28 MB max RSS |
| Binary size | `target/release/pc` | 1,202,416 B ≈ 1.15 MB stripped (note: built with **default features only — demo provider**; the 1.1 MB claim in `docs/research/rust-ecosystem.md` holds for the demo build, not for `--features telegram,discord`) |
| Unsafe audit | `grep -r unsafe` over all `.rs` | **PASS** — zero `unsafe` tokens; `#![forbid(unsafe_code)]` only in `provider-core` + `provider-transport` |
| Git hygiene | `git status` clean, `Cargo.lock` tracked, identity configured | PASS |

## 1. Strengths

1. **Memory discipline is real and verified.** 2.28 MB idle RSS for the sidecar
   vs the 400 MB–1 GB JS baseline is the product's core promise and it is met;
   the `current_thread` tokio runtime, `panic = "abort"` + `strip` release
   profile, and `default-features = false` on `reqwest` (rustls, no native-tls)
   are the right levers, applied consistently.
2. **Dependency minimalism.** The whole workspace adds exactly 9 direct deps
   (tokio, serde, serde_json, thiserror, tracing(+subscriber), reqwest,
   tokio-tungstenite, futures-util, async-trait), all old and battle-tested;
   the optional `ws`/`http` transports are feature-gated so the default stdio
   binary pays nothing for them. The supply-chain gate script enforces the
   14-day policy mechanically.
3. **Clean capability contract.** `provider-core` implements `docs/api-contract.md`
   verbatim: one normalized `ChannelMessage`, a 6-variant `ProviderError`
   taxonomy with stable wire `kind`, and exactly two traits
   (`ChatProvider`, `ProviderEvents`). Providers depend only on core + std —
   dependency inversion vs ZeroClaw was done correctly.
4. **Transport discipline.** stdio owns stdout exclusively (logs to stderr),
   NDJSON framing is exact, `parse_request` returns spec-correct `-32700` /
   `-32600` errors, `shutdown` drains the writer and exits cleanly (tested:
   response-then-EOF).
5. **Compile-time provider pruning** (`bin/pc` features `telegram`/`discord`)
   plus the registry behind a cargo feature works as designed and is tested.
6. **Test culture is above average for a v0.1**: hand-rolled mock Telegram
   API, mock Discord REST, paused-clock heartbeat tests, and full stdio
   protocol tests with **zero external test dependencies** — this is exactly
   the "small helpers in-workspace / base utilities over deps" policy in
   action.

## 2. LIB ALTERNATIVES ANALYSIS — concrete verdicts

Supply-chain gate status for every candidate below: **all pass the 14-day
gate** (verified against crates.io `created_at` 2026-08-12), so age is never
the deciding factor here — binary size, maintenance burden, and fit are.

### 2.1 jsonrpsee vs hand-rolled JSON-RPC — **KEEP hand-rolled**

- Hand-rolled surface: `jsonrpc.rs` + `parse_request` ≈ 350 lines, zero new
  deps, and it already implements exactly what the wire needs (NDJSON framing,
  error codes, notifications). jsonrpsee (first pub 2020-02-28, 2.36 M
  downloads) is built around **WS/HTTP servers and its own subscription
  machinery** — it has *no stdio transport*, so the NDJSON loop would still be
  hand-written; you would additionally inherit its tower/hyper/soketto closure
  (~30+ crates) and its proc-macro API for a handful of methods.
- Cost of switching: +0.5–1 MB binary, +~30 crates to audit, 2–3 days of
  integration, no user-visible gain. **Verdict: correct call, keep.** Revisit
  only if the method surface grows to 20+ methods with server-side
  subscriptions (draft streaming at scale), and even then prefer `jsonrpsee`
  only for the ws/http transports, never stdio.

### 2.2 teloxide vs hand-rolled Telegram — **KEEP hand-rolled**

- Hand-rolled: ~900 lines incl. tests, covers the entire used surface
  (`getUpdates` offset cursor, long-poll, `sendMessage`, 401/409/429 mapping
  with `retry_after`). teloxide (2020-02-19) is a full framework (dptree,
  tower, its own dispatcher, `Bot` builder, download/upload machinery) — its
  dispatcher model does not map onto the `ProviderEvents` sink and it would add
  ~40–60 crates to the closure for features this provider deliberately does not
  use (webhooks, inline queries, games, payments…). **Verdict: correct call,
  keep.** The only teloxide feature worth borrowing later is `sendPhoto`/
  `sendDocument` multipart helpers — those are ~100 lines of `reqwest::multipart`
  when media outbound lands.

### 2.3 serenity vs hand-rolled Discord Gateway — **KEEP hand-rolled (with an escape hatch: twilight)**

- This is the only area where a real library exists and was declined, and the
  decline is **justified by the project's own constraints**: serenity
  (2016-11-30) is enormous (its full tree is one of the heaviest in the
  ecosystem; minutes-long compiles, +2–4 MB binary, a cache + framework layer
  that wants to own the event loop). The hand-rolled gateway here is ~1,300
  lines (gateway protocol + heartbeat + message normalization), tested, and
  already implements the hard parts: RESUME with session id + seq, immediate
  first heartbeat with ACK tracking, `INVALID_SESSION` resumability handling.
- **Escape hatch:** `twilight-gateway` (2020-08-30, focused gateway client,
  ~1/10th of serenity's footprint) is the right *if we ever* need sharding,
  zlib/permessage-deflate compression, or the full opcode matrix. Effort to
  switch today: 2–3 days + new dep tree; **not recommended for v0.2** given the
  current feature surface (messages only, no voice/interactions/sharding).
- **Real maintenance risk found in the hand-roll (see §3.2):** close codes
  **4010–4014 are not treated as fatal** (only 4004 is), so an invalid-intents
  misconfiguration loops reconnect forever instead of surfacing an error. This
  is a bug in the hand-roll, not an argument for serenity — fix it in-place
  (~10 lines).

### 2.4 reqwest feature set — **KEEP, with a note**

- `default-features = false` + `rustls-tls` is the correct size/security
  trade (no OpenSSL, static-friendly). reqwest 0.12.28 is pinned while 0.13.4
  is current — fine for v0.1 (0.12 is the mature line); schedule the 0.13 bump
  with the v0.2 breaking window. The heavy `idna`/`icu` closure is the price of
  URL parsing and is shared with tungstenite; `ureq`/`attohttpc` would cut it
  but are sync/less-proven and would fragment TLS handling — not worth it for a
  sidecar that already idles at 2.3 MB.

### 2.5 tokio runtime choice — **KEEP (current_thread for `pc`)**

- `Builder::new_current_thread()` for the sidecar is right: one thread, no
  worker-thread pool, minimal RSS. The workspace tokio feature set
  (`rt-multi-thread` etc.) is for library consumers who run their own runtime —
  correct. Two nits: (a) `provider-discord` pulls **`test-util` in its
  `[dependencies]`** (only used by `start_paused` tests) — move to
  `[dev-dependencies]` so release builds of consumers don't compile the
  paused-clock machinery; (b) `tokio-tungstenite` 0.24 is two majors behind
  (0.30) — no urgency, but fold into the v0.2 dependency bump.

### 2.6 tracing vs log — **KEEP tracing**

- tracing is the right choice for async (span context, per-provider task
  correlation); `EnvFilter` + stderr writer is minimal and the 
  `matchers`/`regex-automata` closure it adds is acceptable. `log` would save
  ~5 crates but lose span context for zero user-visible gain. Keep.

### 2.7 async-trait vs native async fn in traits (edition 2024?) — **KEEP async-trait for v0.2**

- Edition is 2021; native AFIT (async fn in traits, stable since 1.75) is not
  dyn-compatible for `&mut self` methods, and `ChatProvider::start/stop` take
  `&mut self` because the registry stores `Box<dyn ChatProvider>`. async-trait
  (0.1.92, 2019-07-23) is the minimal, correct tool for that pattern; its
  per-call boxing cost is irrelevant at this scale. A redesign to `&self` +
  interior-mutable task handles (both providers already clone state into
  `Arc<Self>` at `start()`, so the shape is half-way there) would make the
  trait dyn-compatible *without* async-trait and unlock edition 2024 — **~1 day
  of work, defer to the edition-2024 breaking window, not worth it in v0.2.**

### 2.8 hyper hand-rolled HTTP server (vs axum) — **KEEP**

- The `http` transport is one `POST` handler; hyper 1 + hyper-util +
  http-body-util (all feature-gated, default-off) is the minimal correct
  substrate. axum (2021-07-22) would add tower layers and routing machinery for
  a single route. Keep.

### 2.9 "Should have used a lib" — the genuinely hand-rolled-where-a-lib-exists spots

These are the real answers to the review question (small, concrete):

1. **`provider-discord/src/message.rs` ISO-8601 parsing** — hand-rolled
   `days_from_civil` (Hinnant) + `iso_ts` (~40 lines of date math). The comment
   even says it is a fallback for a field (`timestamp`) that **always** has a
   snowflake id on `MESSAGE_CREATE`. Either delete `iso_ts` (recommended —
   snowflake is authoritative) or use the `time`/`chrono` crate. Hand-rolled
   civil-date math is exactly the kind of thing that should use a lib.
2. **`MediaAttachment.data` byte encoding** — the schema ships `Vec<u8>` as a
   JSON array of numbers (`[1,2,3,255]`, 3.7× wire expansion) while
   `docs/architecture.md` §7.2.4 and the crate docs say "base64-encoded by the
   JSON-RPC layer". The documented base64 encoding was never implemented. Use
   `base64` (2015-12-04, gate-passing, already in the closure via rustls) or
   change the docs — one or the other, they currently disagree.
3. **Hand-rolled mock HTTP servers in tests** (telegram/discord `#[cfg(test)]`
   `mock_api`/`mock_rest`, ~200 lines of raw TCP each) — a deliberate zero-dep
   choice that has real value (no test-only deps, exercises the real HTTP
   stack). **Keep**, but note `wiremock` exists if the fixtures grow.
4. **`Heartbeat` scheduler** — already built on `tokio::time::Interval`
   (good); not a finding, just confirming the correct lib is used underneath.

## 3. Architecture issues

### 3.1 Error handling

- **Fatal provider errors never reach the JSON-RPC client (P0).** The
  `event.error` notification type (`ErrorEvent`, `EVENT_ERROR`) exists and is
  advertised in `capabilities().notifications`, but **nothing in the workspace
  ever constructs it** (`Notification::error` has zero call sites). A Telegram
  fatal 401/409 or Discord 4004 stops the provider and the error sits in
  `take_last_error()` — invisible to a Node/Python host, which sees a provider
  that simply went silent. This is the single most damaging gap for the
  product's actual users.
- `RateLimit(String)` loses structure: Telegram parses `parameters.retry_after`
  but discards it on the error path (only used for local sleep); Discord REST
  429 carries `retry_after` in the body and it is ignored entirely (no wait/retry,
  no exposure). Add `retry_after: Option<Duration>` to the variant or to the
  `event.error` data blob.
- Provider errors store strings, so `std::error::Error::source()` chains are
  lost (transport layer does use `#[from]` correctly). Acceptable for v0.1;
  consider `#[source]` fields for the network variants in v0.2.

### 3.2 Reconnection / backoff

- **Discord close codes 4010–4014 are not fatal (P1).** Per the Gateway spec
  and the project's own research (`zeroclaw.md` §7.1.4: "fatal close-code
  handling (4004/4010–4014)"), 4013 (invalid intents) / 4014 (disallowed
  intent) are configuration errors that will **never** succeed on retry — the
  current code routes them into `Reconnect`, producing an infinite reconnect
  loop at up to one attempt/30 s with no client-visible error. 4004 is handled
  correctly; extend the match to the 4000–4014 fatal range.
- **No jitter in either backoff.** Telegram `2^n * base` and Discord
  `500 ms * 2^n` are deterministic and capped at 30 s; N restarted instances
  synchronize, and synchronized Telegram restarts cause 409 conflicting
  long-poll storms (which are *fatal* here — a transient 409 kills polling
  permanently). Add ±20% jitter and treat 409 as retryable-once (another
  instance polling is exactly the "retry after a moment" case).
- Telegram long-poll has no overall connection-timeout cap on the `getUpdates`
  round beyond reqwest's 60 s request timeout — acceptable, but a permanent
  network partition means the poll task sleeps forever with no `event.error`.

### 3.3 Event ordering / delivery guarantees

- **Ordering is per-producer, not global.** Responses are enqueued by the read
  loop task; notifications by provider tasks; both share one broadcast channel
  (stdio) or separate paths (ws: response via `out_tx`, notification via
  bcast→forwarder→`out_tx`). The stdio docs claim "written in the order they
  were produced" — true per producer, **not** across producers. The
  `send_returns_receipt_and_echoes` test only passes because the demo provider
  calls `on_message` synchronously *inside* `send()`. Real providers will
  interleave arbitrarily; document this and make clients key on `id`
  (they already must — fine).
- **Silent drop on overload (P1).** `broadcast::channel(512)` with
  `RecvError::Lagged` → warn + drop: inbound messages are *dropped* under burst
  load with no client-visible signal (the warning goes to stderr logs). For a
  messaging sidecar, dropping an inbound message is the worst failure mode.
  Prefer a bounded `mpsc` writer queue with backpressure (providers block on
  send), or on lag emit `event.error` so the host knows it missed messages.
- **ws per-connection queue is unbounded (P1).** `mpsc::unbounded_channel` for
  outbound frames per ws client: a slow/stalled client grows memory without
  bound. Use a bounded channel + drop-oldest or disconnect policy.

### 3.4 Config

- **`PC_*_CONFIG` and the JSON config `config` blob are dead (P1).**
  `config.rs` merges `PC_TELEGRAM_CONFIG`/`PC_DISCORD_CONFIG` and the JSON
  file's per-provider `config` object, and the USAGE text advertises them, but
  `build_provider` only ever reads `config.token`. `base_url`, poll interval,
  long-poll timeout, intents, request timeout — all the `with_*` builder knobs —
  are unreachable from the sidecar. Either wire them through or delete the
  documentation; today it is a trap (user sets `base_url`, it is silently
  ignored).
- Config errors are reported once at startup (good); missing `token` fails
  with a clear message (good). No secret redaction needed — `Debug` impls
  deliberately omit the token (good).

### 3.5 Feature-gating

- **`provider-core`'s `registry` feature is not pruned from provider crates
  (P2).** `crates/provider-core/Cargo.toml` says the feature exists so "provider
  crates themselves" can build lean ("Disable for a lean embed that only needs
  schema + traits"), and `docs/architecture.md` §4 promises "providers depend
  only on core + std" — but `provider-telegram` and `provider-discord` depend on
  `provider-core` **with default features**, so the registry module is compiled
  into every provider build (verified via `cargo tree -e features`). One-line
  fix: `default-features = false`.
- Everything else (feature-gated `demo`/`telegram`/`discord` in `pc`, gated
  `ws`/`http` transports, gated registry) is correct and verified.

### 3.6 API ergonomics — Rust users

- `ChatProvider` is small and right; `ProviderEvents` being sync keeps the
  provider hot path allocation-free. `take_last_error` is a nice touch but is
  the *only* error channel (see 3.1).
- **Discord REST has no default timeout** (`reqwest::Client::new()`), Telegram
  defaults 60 s — inconsistent; a hung Discord `send` blocks the caller
  indefinitely. Default to a timeout on both.
- Builder-pattern config on providers is good; the sidecar just doesn't use it
  (§3.4).

### 3.7 API ergonomics — JSON-RPC clients (Node/Python)

- `capabilities()` advertises `"features": ["send", "draft", "choice"]` but
  **`draft`/`choice` have no RPC methods and no code path emits
  `event.draft`/`event.choice`** (types exist, zero call sites). Advertising
  unimplemented features is a contract lie (P0/P1) — implement or drop from the
  list.
- `event.message` wraps its payload as `{"message": …}` while `event.draft`/
  `event.choice`/`event.error` are flat — inconsistent envelope shape; pick one
  and document it (the docs currently imply flat everywhere).
- `Id` accepts u64/string/null but not negative numbers (legal per the JSON-RPC
  spec) — rejected as `-32600`. Minor; document or widen.
- Batch requests are rejected with `-32600` (a spec deviation; JSON-RPC 2.0
  expects batch support). It is documented and fine for v0.1, but Python/JS
  clients that batch will break — say so in the contract.
- No `health`/`ping` method; `initialize` is the only liveness probe. Consider
  adding `health` in v0.2 (trivial).
- Node example comment says "reply_to/attachments are required fields" — wrong
  (`#[serde(default)]` makes them optional); the example works by passing
  `null`/`[]`. Doc nit.

### 3.8 Missing capabilities (roadmap honesty)

- No Slack / WhatsApp providers yet — `docs/architecture.md` §3 and
  `zeroclaw.md` §7.1.4 document both patterns (Slack Socket Mode WS with
  `apps.connections.open`; WhatsApp Cloud webhook + HMAC-SHA256, fail-closed
  without `app_secret`). WhatsApp Cloud needs only `hmac`/`sha2` (both already
  in the rustls closure) — the cheapest next provider.
- **Outbound media is stubbed**: both providers `warn!` and ignore
  `attachments`. The schema supports `data`/`url`; telegram needs `sendPhoto`/
  `sendDocument`, discord needs multipart — each ~100–150 lines on reqwest.
- Draft streaming / choice / typing / reactions: wire types exist
  (`DraftEvent`, `ChoiceEvent`) but nothing implements them; `capabilities`
  should not claim them (see 3.7).
- `whatsapp-rust` 0.7.0 (2025-10-07, 308 d — gate-passing) is the documented
  path for WhatsApp Web, but the cloud webhook is the right v0.2 first step.

### 3.9 Test coverage gaps

- **No end-to-end test that spawns the `pc` binary** — the Node example is
  manual; add a `tests/` integration that runs the built binary over a real
  stdio pipe (would have caught the `event.error` and config-plumbing gaps).
- ws transport: 1 test (initialize + shutdown). Missing: fan-out to multiple
  clients, notification forwarding, lag/overflow behavior, slow-client bound.
- http transport: no body-size limit, no content-type check, no batch test
  over http (only via stdio parse path).
- No test that a fatal provider error surfaces to the client (it can't today —
  writing this test first would drive the fix).
- Discord gateway state machine (connect→HELLO→IDENTIFY→READY→resume→
  reconnect) is not tested end-to-end; only unit pieces (payload builders,
  heartbeat, message parse). A scripted fake-gateway WS server test is the
  natural next step.
- `cargo fmt --check` fails (25 diffs) — the repo is not `cargo fmt` clean
  despite CONTRIBUTING.md requiring it; this is a CI-gate gap, not a code
  quality issue.

### 3.10 Unsafe audit

Zero `unsafe` tokens in the entire workspace; `#![forbid(unsafe_code)]` is set
on `provider-core` and `provider-transport` only. Add the attribute to
`provider-telegram`, `provider-discord`, and `bin/pc` (one line each) so the
property is enforced, not just true.

## 4. Ranked recommendations

### P0 — fix before any external user

1. **Wire fatal provider errors to `event.error`.** `ErrorEvent`/`EVENT_ERROR`
   exist and are advertised but never emitted; a dead Telegram 401 or Discord
   4004 is currently invisible to hosts. (~0.5 d: notify sink + poll/gateway
   loops + one integration test.)
2. **Make `capabilities()` truthful.** Either implement draft/choice or
   advertise only `["send"]`; advertising unimplemented features breaks client
   trust in the contract. (~0.25 d.)

### P1 — before v0.2

3. **Treat Discord gateway close codes 4000–4014 as fatal (Auth/Config), not
   reconnectable** — today an invalid-intents misconfig loops forever with no
   error. (~2 h, plus the test that would have caught it.)
4. **Replace drop-on-lag with backpressure or explicit overflow signaling** for
   inbound events (stdio: bounded mpsc writer queue; ws: bounded per-connection
   queue) — silent inbound-message loss is the worst failure mode for this
   product. (~1 d.)
5. **Wire per-provider config through `pc`** (`base_url`, poll interval,
   intents, timeouts) or delete the `PC_*_CONFIG`/`config`-blob documentation
   that currently promises dead options. (~0.5 d.)
6. **Add an e2e test that spawns the built `pc` binary** and drives it over a
   real stdio pipe (catches regressions in all of the above). (~1 d.)

### P2 — v0.2 hygiene

7. `default-features = false` on `provider-core` in `provider-telegram`/
   `provider-discord` (registry pruning, as the crate itself documents). (~5 min)
8. Move tokio `test-util` from `provider-discord` `[dependencies]` to
   `[dev-dependencies]`. (~5 min)
9. Add `#![forbid(unsafe_code)]` to the three remaining crates. (~5 min)
10. Add ±20% jitter to both backoffs; make Telegram 409 retryable; add a
    default REST timeout to the Discord provider. (~0.5 d)
11. Delete or library-ify the hand-rolled ISO-8601 fallback in
    `message.rs` (snowflake is authoritative — recommend deletion); align
    `MediaAttachment.data` encoding with the docs (base64) or fix the docs.
12. Normalize notification envelope shapes; document cross-producer ordering;
    fix the Node example's "required fields" comment; document batch-rejection
    and negative-id behavior in `docs/api-contract.md`.
13. `cargo fmt` (25 diffs) and add `cargo fmt --check` to CI; consider a
    fake-gateway WS integration test for the Discord state machine.
14. Fold dependency bumps into one v0.2 window: reqwest 0.12→0.13,
    tokio-tungstenite 0.24→0.30, edition 2024 (revisit async-trait then; keep
    it otherwise).

## 5. Bottom line

The v0.1 architecture is sound: the dependency discipline, memory results, and
capability contract are the product's moat and they hold up under review. **No
library switch is warranted for v0.2** — jsonrpsee, teloxide, serenity, axum
would all add size and closure for features this design deliberately does not
need; the one "should have used a lib" spots are two small hand-rolls
(ISO-8601 parsing, base64) and both have trivial fixes. The real problems are
not library choices but **observability of failure** (P0/P1: silent fatal
errors, silent drops, silent config dead-ends) and **truthfulness of the
advertised contract** (P0/P1: `features: ["draft","choice"]`, `event.error`
with no emitter). Fix those and v0.2 is in good shape for Slack/WhatsApp Cloud
(hmac/sha2 webhook) and media outbound.
