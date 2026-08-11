# ZeroClaw — Provider Integration Research

> **Research doc for `provider-connect`** — a Rust sidecar crate connecting AI agents to messaging providers via a clean language-agnostic API (JSON-RPC over stdio/WebSocket/HTTP, direct Rust, FFI). This document is the implementation blueprint: it catalogs how ZeroClaw integrates ~35 messaging providers, extracts its event/message abstractions, analyzes the memory problem it solves, and gives concrete reuse-vs-redesign recommendations.
>
> **Research date:** 2026-08-11 · **ZeroClaw commit studied:** `82942853d50fadf221beb653cbf6f6094eac449f` (v0.8.4, master) · **OpenClaw commit studied:** shallow clone of `openclaw/openclaw` (master, same date).

---

## 1. Executive summary

- **ZeroClaw** (`github.com/zeroclaw-labs/zeroclaw`, ~32.5k stars, MIT OR Apache-2.0) is a **Rust agent runtime** — a single static binary that talks to LLM providers (~20) and reaches users through **30+ messaging channels** (Discord, Telegram, Slack, WhatsApp ×2, Signal, Matrix, email ×2, IRC, LINE, iMessage, WeChat, QQ, DingTalk, Lark, Mattermost, Nextcloud Talk, Nostr, Bluesky, Twitter, Reddit, Git forges, MQTT/AMQP, webhooks, voice, CLI, ACP, gateway REST/WS, …).
- It is explicitly positioned as the **lightweight Rust alternative to OpenClaw** (the Node/TypeScript assistant, `github.com/openclaw/openclaw`, ~386k stars, formerly Clawdbot/Moltbot). This is *the* project whose Node.js SDK stack idles at hundreds of MB of RAM — the exact problem `provider-connect` exists to solve.
- **Architecture:** a layered Cargo workspace. `zeroclaw-api` defines the public traits (`Channel`, `ModelProvider`, `Tool`, `Memory`, …). `zeroclaw-channels` implements the messaging integrations, each behind a compile-time feature flag. All inbound events are normalized into one struct — `ChannelMessage` — and handed to a single agent loop through a `tokio::sync::mpsc` channel; all outbound goes through `SendMessage`.
- **Key finding for us:** ZeroClaw already proves the whole stack in Rust *at app scale* (idle core < 5 MB, ~6.6–8.8 MB binary, milliseconds startup). Its channel implementations are **hand-rolled on `reqwest` + `tokio-tungstenite`** for the big platforms (Discord, Slack, Telegram, WhatsApp Cloud) — no heavy SDK crates — which is exactly the dependency posture our supply-chain policy wants. Its `Channel` trait and `ChannelMessage`/`SendMessage` structs are a near-perfect starting schema for a library Provider trait + JSON-RPC wire contract.
- **What we should NOT copy:** ZeroClaw is an *app* (agent loop, memory, SOP engine, security policies, config schema baked in). `provider-connect` must be a *library/sidecar*: decouple providers from any agent runtime, split the monolithic per-channel files, make capabilities type-level, and expose everything over JSON-RPC (ZeroClaw's own ACP channel is a working reference for JSON-RPC-over-stdio).

---

## 2. Canonical repo confirmation & ecosystem disambiguation

Three names collide in this space; they are related but distinct:

| Name | Repo | Lang | Role | Stars* |
|---|---|---|---|---|
| **OpenClaw** (formerly **Clawdbot**, **Moltbot**) | `github.com/openclaw/openclaw` | TypeScript | Personal AI assistant; Node.js; 29+ channels; the memory hog | ~386k |
| **ZeroClaw** ✅ canonical | `github.com/zeroclaw-labs/zeroclaw` | Rust | Rust reimplementation/alternative of the OpenClaw concept | ~32.5k |
| ZeroClaw fork | `github.com/openagen/zeroclaw` | Rust | fork of `zeroclaw-labs/zeroclaw` (API confirms `fork: true`, 1.9k stars) — **not** canonical | ~1.9k |

\* GitHub API, 2026-08-11. Confirmation chain: websearch results, `zeroclaw.net` ("View on GitHub" → zeroclaw-labs/zeroclaw; "Image source: github.com/openagen/zeroclaw" is a fork), GitHub API metadata (`full_name`, `homepage: zeroclawlabs.ai`, `topics: [agent, agentic, ai, infra, ml, openclaw, os, zeroclaw]`).

**Relationship:** ZeroClaw was "bootstrapped by AI tools working from OpenClaw's TypeScript codebase" (FND-001, §1) and ports the multi-channel + provider-agnostic model to Rust. OpenClaw's own docs/community frequently recommend ZeroClaw when RAM is scarce (e.g. a r/raspberry_pi thread: "Why not install zeroclaw (needs less than 5mb of RAM) directly…?").

**Why both matter to `provider-connect`:**
- **ZeroClaw** = the concrete Rust implementation of every provider integration we plan to rebuild. Its source is our primary blueprint (and, being MIT/Apache-2.0, directly portable/adaptable).
- **OpenClaw** = the *counter-example* that justifies the sidecar: its per-channel Node SDKs and runtime are what make a JS agent idle at ~400 MB (and deployments routinely recommend 1–2 GB RAM).

---

## 3. Architecture

### 3.1 Workspace layout (relevant crates)

```
zeroclaw (workspace root, binary "zeroclaw", v0.8.4)
├── crates/
│   ├── zeroclaw-api          ← THE KERNEL ABI: traits Channel / ModelProvider / Tool / Memory / Peripheral,
│   │                            shared types (ChannelMessage, SendMessage, MediaAttachment), jsonrpc module
│   ├── zeroclaw-channels     ← 30+ messaging integrations, feature-gated (this is our catalog source)
│   ├── zeroclaw-gateway      ← HTTP/WebSocket gateway, web dashboard, webhook ingress (axum)
│   ├── zeroclaw-runtime      ← agent loop, security policy, SOP engine, cron, subagents
│   ├── zeroclaw-providers    ← LLM clients (Anthropic, OpenAI, Ollama, …) + routing/retry
│   ├── zeroclaw-memory       ← SQLite + embeddings + vector retrieval
│   ├── zeroclaw-config       ← TOML schema, encrypted secrets, autonomy levels
│   ├── zeroclaw-tools, zeroclaw-plugins (WASM), zeroclaw-hardware, … (supporting crates)
└── apps/ (zerocode TUI, tauri desktop)
```

High-level data flow (from `docs/book/src/architecture/overview.md`):

```
CLI / chat platforms / gateway clients / ACP IDEs  ──►  zeroclaw-channels / zeroclaw-gateway
                                                              │  (ChannelMessage via mpsc)
                                                              ▼
                                     zeroclaw-runtime  (agent loop · security · SOP · cron)
                                                              │
                                                              ▼
                                 zeroclaw-providers ─► LLM APIs      zeroclaw-tools ─► fs/shell/net
```

### 3.2 The channel boundary (the part we reuse)

Each channel is one `impl Channel` in `zeroclaw-channels/src/<name>.rs` (or a module dir like `discord/`). The runtime never touches platform APIs; it only sees `ChannelMessage` inbound and `SendMessage` outbound.

```
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;
    async fn send(&self, message: &SendMessage) -> anyhow::Result<()>;
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()>;  // long-running
    async fn health_check(&self) -> bool { true }
    // capability probes (all default false/None and overridden per channel):
    async fn start_typing / stop_typing
    fn supports_draft_updates(&self) -> bool
    fn supports_outbound_send(&self) -> bool           // inbound-only transports (AMQP/MQTT) return false
    fn self_handle(&self) -> Option<String>            // self-loop guard (never reply to own messages)
    fn self_addressed_mention(&self) -> Option<String> // per-channel system-prompt mention form
    async fn send_choice(&self, recipient, prompt, options: &[(id,label)])
    async fn send_draft / update_draft / update_draft_progress / finalize_draft / cancel_draft
    async fn add_reaction / remove_reaction
    fn is_direct_message(&self, msg) -> bool
    fn supports_multi_message_streaming(&self) -> bool / fn multi_message_delay_ms(&self) -> u64
    async fn forge_request(&self, req: ForgeApiRequest) -> ForgeApiResponse  // git-forge passthrough
}
```
— `crates/zeroclaw-api/src/channel.rs` (line ~609), 48 KB file, MIT/Apache-2.0.

Key design properties of this trait that map directly onto a library:
1. **Inbound and outbound are separate hooks** (`listen` / `send`).
2. **Capabilities are declared, not assumed** — `supports_*` probes plus defaults, so a channel can implement a subset (e.g. AMQP is inbound-only, TTS is outbound-only).
3. **Streaming/drafts are a first-class protocol** (`send_draft` → `update_draft*` → `finalize_draft`/`cancel_draft`), not an app hack.
4. **Platform identity knowledge is the channel's job** (`self_handle`, `self_addressed_mention`, `is_direct_message`, `drop_self_messages`), keeping the loop platform-agnostic.
5. **Approval/choice prompts** (`send_choice`, `ChannelGatePrompt` + `GateChoiceKind` wire tokens) — channels render the same semantic prompt natively (Discord buttons, Telegram inline keyboards, WhatsApp interactive, Signal native polls, plain-text fallback).

### 3.3 Channel lifecycle

- Every channel is **feature-gated at compile time** (`channel-<name>` features in `zeroclaw-channels/Cargo.toml`). Default build: ACP, webhook, email, Telegram, Discord, filesystem. Prebuilt binaries add Matrix, Lark, WhatsApp Web. `--features channels-full` = everything.
- `collect_configured_channels` builds one channel instance per enabled `[channels.<type>.<alias>]` config entry bound to an agent's `channels` list.
- Each `listen()` runs as its own tokio task; inbound `ChannelMessage`s flow through a bounded mpsc into channel dispatch → agent turn → `send()`/draft APIs back out.
- Authorization of senders is app-level via **peer groups** (not inside channels): an empty peer set denies everyone; pairing flows (`/bind`, one-time codes) are app-level too.

---

## 4. Provider integration catalog

### 4.1 Master table (37 integrations, verified against source + docs)

Legend — inbound transport: **WS** = persistent WebSocket, **LP** = HTTP long-poll, **POLL** = periodic REST polling, **WH** = inbound webhook (HTTP POST), **SSE** = server-sent events, **TCP** = raw socket, **STDIO** = stdio. All outbound is REST/WS/TCP per platform. "SDK" = Rust crate used; "hand-rolled" = direct `reqwest`/`tokio-tungstenite` code, **no** platform SDK crate.

| # | Provider | Feature flag | SDK (Rust) | Inbound transport | Auth model |
|---|---|---|---|---|---|
| 1 | Discord | `channel-discord` | hand-rolled (reqwest + tokio-tungstenite; no serenity) | **WS** Gateway + REST | bot token (Gateway `identify` + REST `Authorization`) |
| 2 | Telegram | `channel-telegram` | hand-rolled (reqwest; no teloxide) | **LP** `getUpdates` (offset cursor, 30 s) | bot token (`/bot<token>/<method>`) |
| 3 | Slack | `channel-slack` | hand-rolled (reqwest + tokio-tungstenite; no @slack/bolt) | **WS** Socket Mode (`apps.connections.open`) or HTTP events | bot token `xoxb-` + app token `xapp-` |
| 4 | Matrix | `channel-matrix` | `matrix-sdk` 0.18 (e2e-encryption, sqlite, markdown) | **WS** `/sync` long-poll via SDK | login (user/device id) + access/refresh token, E2EE store |
| 5 | WhatsApp Cloud API | `channel-whatsapp-cloud` | hand-rolled (Meta Graph API v18.0) | **WH** on gateway `/whatsapp/<alias>`, HMAC-SHA256 verify | `phone_number_id` + access token + app secret + verify token |
| 6 | WhatsApp Web | `whatsapp-web` | `whatsapp-rust`/`wacore`/`waproto` (wa-rs, oxidezap) — native Rust, **no browser** | **WS** to WhatsApp servers (protobuf) | QR / pair-code device linking, SQLite session store |
| 7 | Signal | `channel-signal` | hand-rolled client for **signal-cli** daemon | **SSE** from local signal-cli HTTP daemon | none at ZeroClaw level; signal-cli owns account/keys |
| 8 | Email (IMAP/SMTP) | `channel-email` | `async-imap`, `lettre`, `mail-parser` | **POLL** IMAP (default 60 s) | mailbox password (app passwords for Gmail/Outlook) |
| 9 | Gmail Push | `channel-email` (`gmail_push`) | Google Pub/Sub push + Gmail history API | **WH** (Pub/Sub push) | OAuth token + webhook secret |
| 10 | iMessage | `channel-imessage` (+`channel-linq`) | hand-rolled Linq Partner API bridge | via Linq relay (macOS-only) | Linq API key |
| 11 | IRC | `channel-irc` | hand-rolled tokio TCP (+rustls) | **TCP** IRC protocol | (server pass/SASL per config) |
| 12 | Twitch | `channel-twitch` | thin adapter over IRC channel | **TCP** IRC | OAuth token (Twitch) |
| 13 | Mattermost | `channel-mattermost` | hand-rolled (reqwest + WS) | **POLL** REST v4 (3 s) or **WS** `listen_mode` | `bot_token` (+ password alt), UUID peer matching |
| 14 | LINE | `channel-line` | hand-rolled Messaging API (reqwest) | **WH** embedded HTTP server | `channel_access_token` + `channel_secret` (HMAC), env fallback |
| 15 | Nextcloud Talk | `channel-nextcloud` | hand-rolled Talk Bot API (reqwest) | **WH** on gateway `/nextcloud-talk/<alias>`, HMAC verify | shared bot secret (signs both directions); needs Talk ≥ 17.1 |
| 16 | Bluesky | `channel-bluesky` | hand-rolled AT Protocol (reqwest, `com.atproto.*`) | **POLL** for mentions | app password → JWT session (refresh) |
| 17 | Nostr | `channel-nostr` | `nostr-sdk` 0.44 (nip04, nip59) | **WS** NIP-01 relays | raw `nsec` private key |
| 18 | Twitter / X | `channel-twitter` | hand-rolled (reqwest, `api.x.com/2`) | **stream** v2 Filtered Stream | OAuth 2.0 Bearer token only |
| 19 | Reddit | `channel-reddit` | hand-rolled (reqwest JSON API) | **POLL** | OAuth 2.0 refresh token (script app) |
| 20 | Git forges | `channel-git` | hand-rolled REST; GitHub App JWT (`jsonwebtoken` aws-lc-rs) or Gitea PAT | **POLL** with `since` cursors (min 15 s) | GitHub App (App ID + private key) / PAT |
| 21 | MQTT | `channel-mqtt` | `rumqttc` 0.25 | **WS/TCP** broker subscription | broker user/pass, TLS (inbound-only) |
| 22 | AMQP | `channel-amqp` | `lapin` 2 (rustls) | **TCP/TLS** broker consume | URL, optional (m)TLS certs (inbound-only) |
| 23 | Filesystem | `channel-filesystem` | `notify` 6 + `glob` | inotify/FSEvents | n/a (inbound-only) |
| 24 | Webhook | `channel-webhook` | axum embedded server | **WH** `POST {listen_path}` | HMAC-SHA256 `secret` (mandatory; 401 without) |
| 25 | CLI | always | stdio (tokio) | **STDIO** | n/a |
| 26 | Gateway REST/WS | `gateway` (default) | axum + tokio-tungstenite | **HTTP + WS** | gateway auth (pairing/WebAuthn) |
| 27 | ACP | `channel-acp-server` | hand-rolled JSON-RPC 2.0 | **STDIO** NDJSON | none (local) |
| 28 | ClawdTalk | `channel-clawdtalk` | hand-rolled Telnyx SIP | **SIP** real-time voice | Telnyx API key |
| 29 | Voice Call | `channel-voice-call` | hand-rolled Twilio/Telnyx/Plivo | **WH + WS** media streams | per-vendor API keys |
| 30 | Voice Wake | `voice-wake` | `cpal` | local mic | n/a |
| 31 | TTS | always | OpenAI/ElevenLabs/Google/Edge/Piper (reqwest) | n/a (outbound only) | per-vendor keys |
| 32 | WeChat | `channel-wechat` | hand-rolled iLink Bot (AES-128-ECB, MD5) | iLink Bot protocol | bot credentials (proprietary) |
| 33 | WeCom | `channel-wecom` / `channel-wecom-ws` | hand-rolled (AES-CBC) | **WH** or **WS** (`wecom_ws`) | corp credentials |
| 34 | QQ | `channel-qq` | hand-rolled (reqwest, `api.sgroup.qq.com`) | app channel API | app token (`getAppAccessToken`) |
| 35 | DingTalk | `channel-dingtalk` | hand-rolled | **WH** | app credentials |
| 36 | Lark | `channel-lark` | hand-rolled (`prost` protobuf) | **WH**/long-conn | app credentials |
| 37 | Mochat / Notion | `channel-mochat` / `channel-notion` | hand-rolled | **WH** / **POLL** | app / Notion API token |

(*Counts to ~37 distinct integrations; README says "30+ channels". Rows 32–37 are China-market/niche platforms — details thinner, protocol confirmed at the transport level only.*)

### 4.2 Event types listened to / message types sent (verified in source)

| Provider | Inbound event types (listens to) | Outbound message types (sends) |
|---|---|---|
| **Discord** (`discord/` mod) | Gateway `MESSAGE_CREATE` (guild + DM), `MESSAGE_REACTION_*`, `INTERACTION_CREATE` (slash commands, button/component clicks); intents: `GUILDS \| GUILD_MESSAGES \| DIRECT_MESSAGES \| MESSAGE_CONTENT` (+ privileged `GUILD_MEMBERS`, `GUILD_PRESENCES` when configured); gateway Resume sessions, close-code diagnostics (4004/4010–4014) | text, embeds (`discord/embed.rs`), threaded replies, draft edits (`PATCH /channels/{id}/messages/{id}`), reactions, slash commands + component replies (`discord/slash.rs`, `interaction.rs`, `custom_id.rs`) |
| **Telegram** | `message` (text/photo/voice/document), `callback_query`; `allowed_updates: ["message","callback_query"]`; offset-cursor long-poll with 30 s timeout | `sendMessage`, `editMessageText` (drafts), `sendPhoto`/`sendVoice`/`sendDocument`, inline keyboards, bot commands via `setMyCommands` (max 100), `sendChatAction` (typing) |
| **Slack** | Socket Mode envelopes: `events_api` (event payload: `message`, `app_mention`, …), `interactive` (`block_actions`), `disconnect`; REST `conversations.history`/`replies` for thread hydration (respects `Retry-After`) | `chat.postMessage`, `chat.update` (drafts), threaded replies (`thread_ts`), reactions, interactive blocks/buttons |
| **Matrix** | room `m.room.message` (text, images, audio, video, files) via `/sync`; thread relations; E2EE-decrypted events | text + HTML (markdown), file/voice uploads (`org.matrix.msc3245.voice`), threaded replies, reactions |
| **WhatsApp Cloud** | webhook `messages[]` typed `text`, `image`, `audio`, `video`, `document`, `interactive` (`button_reply`, `list_reply`), statuses; group detection via `context.group_id` | `POST /v18.0/{phone_number_id}/messages`: `text`, `image`, `document`, `interactive` buttons/lists (choice prompts round-trip callback ids as `[choice]<id>`) |
| **WhatsApp Web** | wa-rs events: text, media (image/audio/video/document), location (live locations dropped), group events, reactions; `mode="personal"` DM/group/self-chat policies | text, media upload, reactions, read receipts; passive group context (no agent turn) |
| **Signal** | signal-cli SSE: `Envelope.dataMessage` (text, attachments, `pollAnswer`/`pollVote`), `syncMessage`, group info; `dm_only`/`group_ids`/`ignore_stories` filters | JSON-RPC 2.0 to `signal-cli` `/api/v1/rpc`: `send`, `sendAttachment`, `sendPollCreate`, reactions |
| **Email** | IMAP new-mail (text/html body, attachments, subject); threading via `In-Reply-To`/`References`; Gmail Push via Pub/Sub `historyId` diff | SMTP `multipart/alternative` (plain + rendered-HTML) or plain; attachments; threaded replies |
| **Git forges** | polling issues, PRs, comments, review comments, CI runs, releases (per-event routing table; cold start = no replay; comment edits ignored) | issue/PR comments (draft + in-place edits ≥ 2 s spacing), reactions |
| **IRC/Twitch** | `PRIVMSG` (+ standard IRC traffic) | `PRIVMSG` chunked ≤ 512-byte protocol payloads |
| **Nostr** | kind-1 (text), kind-4 DM (NIP-04), kind-1059 gift-wrap (NIP-17), zaps (experimental) | same kinds, NIP-04 encrypted |
| **Webhook** | `POST` JSON `{sender, content, thread_id}` (HMAC verified) | optional `POST`/`PUT` to `send_url` with `{content, thread_id, recipient}` + `Authorization` |
| **ACP** | JSON-RPC 2.0 over stdio: `initialize`, `session/new`, `session/load`, `session/resume`, `session/prompt`, `session/close` | `session/update` notifications, elicitation (approval prompts as RPC requests) |

**Cross-cutting inbound semantics** (all channels normalize into `ChannelMessage`): `mention_only` (ignore non-@-mentions), `explicitly_addressed` (platform-level @-mention flag), `passive_context` (record but don't trigger a turn), `interruption_scope_id` (cancel in-flight reply on new message), `conversation_scope` (sender-scoped vs room-scoped history), attachments → media pipeline (audio transcription, image normalization), subject threading for email, `internal_sop_event` reserved for forge/SOP ingress.

---

## 5. Event/message abstractions (the reusable unified schema)

All in `crates/zeroclaw-api/src/channel.rs` (+ `media.rs`). These are the closest thing to a "unified schema" in this space and are directly portable (MIT/Apache-2.0).

### 5.1 Inbound — `ChannelMessage`

```rust
pub struct ChannelMessage {
    pub id: String,                    // platform message id (e.g. wamid.xxx, snowflake)
    pub sender: String,                // normalized peer handle
    pub reply_target: String,          // where to reply (chat id / thread / email addr)
    pub content: String,               // normalized text (attachment markers stripped by channel)
    pub channel: String,               // platform key, e.g. "discord"
    pub channel_alias: Option<String>, // config alias for multi-bot setups (session key scoping)
    pub timestamp: u64,
    pub thread_ts: Option<String>,     // platform thread anchor (Slack ts, Discord thread id)
    pub interruption_scope_id: Option<String>, // thread isolation for cancel-in-flight
    pub attachments: Vec<MediaAttachment>,
    pub subject: Option<String>,       // email subject / thread label
    pub internal_sop_event: Option<String>,   // reserved for forge/SOP ingress (never serde)
    pub passive_context: bool,         // record-only; must NOT start a turn
    pub explicitly_addressed: bool,    // platform-level @mention observed
    pub conversation_scope: ChannelConversationScope, // Sender | Room
}
```

Design lessons worth copying:
- **One struct for every platform** — no per-platform event types leak past the adapter.
- **Threading and cancellation are first-class** (`thread_ts`, `interruption_scope_id`) — these drive Slack/Discord/WhatsApp thread behavior and "new message cancels in-flight reply" (ZeroClaw's `interrupt_on_new_message`).
- **`passive_context` + `explicitly_addressed`** let the agent loop implement mention-only and ambient-context policies without knowing the platform.
- **Alias scoping** (`channel_alias`) keeps multi-bot-per-platform sessions separate — directly relevant to our multi-provider sidecar.
- **`internal_sop_event` never round-trips through serde** — a good security pattern for internal routing fields.

### 5.2 Outbound — `SendMessage`

```rust
pub struct SendMessage {
    pub content: String,
    pub recipient: String,
    pub subject: Option<String>,
    pub thread_ts: Option<String>,
    pub cancellation_token: Option<CancellationToken>, // interruptible multi-message delivery
    pub attachments: Vec<MediaAttachment>,
    pub in_reply_to: Option<String>,   // email threading
    pub suppress_voice: bool,          // never synthesize as voice note
    pub force_voice: bool,             // force voice-note delivery
}
```

### 5.3 Media — `MediaAttachment`

```rust
pub struct MediaAttachment { pub file_name: String, pub data: Vec<u8>, pub mime_type: Option<String> }
// + MediaKind classification (Audio/Image/Video/File), MIME-driven
```

For a sidecar with large media we'd ship bytes differently (base64 in JSON-RPC for small files, file-refs/temp files for large ones — ZeroClaw keeps `Vec<u8>` in memory and stores inbound media under `<workspace>/attachments/<conversation>/`).

### 5.4 Choice/approval prompts (reusable wire tokens)

- `ChannelApprovalRequest` / `ChannelApprovalResponse` (`approve|deny|always|deny_with_edit`) + `AttributedApprovalResponse` (who decided, fail-closed provenance).
- `ChannelGatePrompt` (title/description/reference/choices) with **fixed wire vocabulary** `GateChoiceKind::id()`: `approve|deny|edit|revise` — deliberately one stringly-typed token shared across Discord `custom_id`s and text replies. *"Adding a choice is a compile error at every place that must handle it."* — great pattern for a wire protocol: keep the vocabulary in one enum that both sides compile against.

### 5.5 Streaming/draft protocol (reusable RPC surface)

`send_draft → update_draft / update_draft_progress / update_draft_lifecycle / finalize_draft / cancel_draft`, with `supports_draft_updates()`, `supports_multi_message_streaming()`, `multi_message_delay_ms()`, `draft_update_interval_ms` (default 500 ms). `ProgressEvent` lifecycle states: `Received, Planning, WaitingOnModel, RunningTool, CompactingContext, FinalizingResponse`. This is effectively a ready-made JSON-RPC method set for streaming replies — map 1:1 onto sidecar RPC methods.

### 5.6 Extra transport-adjacent abstractions

- `zeroclaw-api/src/jsonrpc.rs` — a small JSON-RPC 2.0 module already in the ABI (used by ACP and gateway).
- Gateway REST/WS + ACP together prove **the same agent session over three transports** (stdio NDJSON, WebSocket, HTTP).
- `paced_channel.rs` — per-(channel,recipient) reply pacing wrapper (bounded FIFO, LRU state) — reusable as sidecar middleware.
- `allowlist.rs` / peer-group matching — app policy; **keep out of the library** (see §7).

---

## 6. Resource usage profile — why a Node/JS agent idles at ~400 MB

### 6.1 The measured/claimed numbers

| | OpenClaw (Node/TS) | ZeroClaw (Rust) |
|---|---|---|
| Idle memory | "over 1 GB of RAM plus TypeScript runtime overhead" (pinggy.io); deployment docs: **512 MB too small, 2 GB recommended** (Fly.io), "minimum 1 GB RAM" (Raspberry Pi guide), 1 GB Droplet = "tight, add swap" | **< 5 MB core** (zeroclaw.net; r/raspberry_pi comment thread); vision constraint "< 5 MB RAM on $10 hardware" (FND-001 §7) |
| Binary size | pnpm monorepo, 64 root deps + per-extension deps, node_modules typically hundreds of MB | **6.6 MB** foundation build (measured, FND-001 §7, v0.7.0); **~8.8 MB** full monolith (v0.6.x, same doc; pinggy reports ~8.8 MB) |
| Startup | Node boot + loading SDKs: seconds | "< 10 ms on 0.6 GHz cores", "400× faster startup" (zeroclaw.net) |
| Hardware floor | 1 GB RAM min, 2 GB recommended | runs on $10 boards (r/raspberry_pi recommends ZeroClaw over OpenClaw on a Pi Zero 2W for this reason) |

The parent's "~400 MB idle" figure sits comfortably inside OpenClaw's documented envelope (1–2 GB deployments, "resource hog over 1 GB"); a lean OpenClaw with only a couple of channels + no browser tools lands in the low hundreds of MB, and a full config with WhatsApp + Matrix + Signal + browser tooling crosses 1 GB. The exact number depends on channel count and tools — but the *mechanisms* below are what matter.

### 6.2 What causes the bloat (mechanisms, with evidence)

1. **Node/V8 baseline.** A modern Node process (OpenClaw requires `node >= 22.22`) carries the V8 isolate + heap reservations + JIT; the interpreter/runtime floor is tens of MB before any application code. Everything else stacks on top of it.
2. **The whole extension graph loads in one process.** OpenClaw is a pnpm monorepo whose root `package.json` pins 64 direct deps, including heavyweight native/WASM modules pulled in for features most installs never use: `playwright-core` (browser automation), `quickjs-wasi`, `tree-sitter` + `web-tree-sitter` (WASM), `linkedom` (DOM), `@mozilla/readability`, `photon-node` (native image), `clawpdf`, `rastermill` (native), `@matrix-org/matrix-sdk-crypto-wasm` (Rust→WASM crypto), `@discordjs/voice` + `libopus-wasm`. Each native/WASM module costs real RSS even when idle (code pages, WASM linear memory, native allocators).
3. **Per-channel SDK weight.** Channel extensions add SDKs per connection: `@slack/bolt` + `socket-mode` + `web-api` (Slack), `grammy` + `@grammyjs/runner` + `transformer-throttler` (Telegram), `matrix-js-sdk` + crypto WASM (Matrix — one of the heaviest), `baileys` (WhatsApp Web protocol client with in-memory proto/state caches), `@twurple/*` (Twitch), `@line/bot-sdk` (LINE), `@tencent-connect/qqbot-connector` + `silk-wasm` + `mpg123-decoder` (QQ). These are separate from the *connection* cost: long-lived WS keepalives (Discord gateway, Slack Socket Mode, WhatsApp, Matrix sync), protocol buffers, caches, and reconnect state.
4. **The historical whatsapp-web.js/Puppeteer trap.** Clawdbot/Moltbot (OpenClaw's predecessors) drove WhatsApp through `whatsapp-web.js` + **Puppeteer launching a headless Chromium** — a single WhatsApp connection alone meant ~300–500 MB of browser RSS. OpenClaw replaced this with **Baileys** (pure-JS WhatsApp Web protocol, no browser) — a big improvement, but the total process still lands in the hundreds of MB. This is the single most instructive example for our Rust sidecar: **the same WhatsApp Web protocol is implementable natively** (ZeroClaw does it with `wa-rs`, no browser; our supply-chain table shows `whatsapp-rust` 0.7.0 on crates.io, 307 days old).
5. **Peripherals that are whole other runtimes.** The Signal channel shells out to `signal-cli`, "a **Java-based CLI**" — a JVM process (~200 MB class) — which OpenClaw's own Fly.io guide flags: "keep memory at 2GB+". ZeroClaw's Signal channel talks to the same signal-cli over HTTP/SSE — the Rust part stays tiny; the JVM cost is a platform tax either way (there is no official Signal Bot API).
6. **No memory control.** In Node there is no way to bound RSS the way a Rust binary can: GC heaps grow and fragment, WASM linear memories are sticky, native addons allocate outside V8's accounting. Idle RSS ≠ heap-used; a "quiet" agent still shows hundreds of MB.
7. **Per-feature tax with no compile-time pruning.** JS ships everything to everyone; tree-shaking doesn't remove native/WASM deps. ZeroClaw's feature-flag model (`--no-default-features --features channel-slack`) means a Slack-only sidecar literally does not compile the Discord/Matrix/WhatsApp code, and its foundation binary measures 6.6 MB.

### 6.3 Why Rust fixes it (ZeroClaw as proof)

- Single static binary, no JIT/heap baseline: idle RSS ~ MBs, not hundreds of MB.
- Async I/O multiplexed on tokio: N channels ≠ N processes; one event loop serves all connections.
- Compile-time feature gating prunes unused providers (matches our supply-chain + "minimal dependencies" policy).
- Native TLS/WS/protobuf without WASM translation layers (`tokio-rustls`, `tokio-tungstenite`, `prost`).
- `wa-rs` proves even the hardest protocol (WhatsApp Web, which forced Puppeteer on JS) is a pure-Rust WS+protobuf client.

---

## 7. Reuse vs redesign — recommendations for `provider-connect`

Goal restated: `provider-connect` = Rust library + sidecar (JSON-RPC over stdio/WS/HTTP, direct Rust, FFI) providing a clean `Provider` trait with per-provider adapters, targeting idle RSS 30–50 MB. ZeroClaw is an app; we are a library. Everything below is concrete.

### 7.1 REUSE (port/adapt from ZeroClaw; MIT/Apache-2.0, directly compatible)

1. **The message schemas, nearly verbatim** — `ChannelMessage` + `SendMessage` + `MediaAttachment` (§5). Add a `protocol_version` and `non_exhaustive`-style additive fields. These become the JSON-RPC wire types (serde-derived; note ZeroClaw keeps `internal_sop_event` out of serde — keep internal fields off the wire).
2. **The capability surface** — `send`, `listen`, `health_check`, `send_draft/update_draft/finalize_draft/cancel_draft`, `start_typing/stop_typing`, `add_reaction/remove_reaction`, `send_choice`, `self_handle`, `is_direct_message`. **But restructure as composable capability traits** (see 7.2 #2).
3. **The wire-token pattern** — `GateChoiceKind::id()` style: single enum per interactive vocabulary, shared producer/consumer, compile-checked.
4. **Protocol knowledge per provider** — the hard-won details in `crates/zeroclaw-channels/src/*.rs`, especially:
   - Telegram: `getUpdates` offset-cursor long-poll (`allowed_updates=["message","callback_query"]`, 30 s timeout, probe loop) — no webhook/URL needed; outbound via `/bot<token>/<method>` REST.
   - Discord: gateway WS with intents bitmask (`GUILDS|GUILD_MESSAGES|DIRECT_MESSAGES|MESSAGE_CONTENT`), `Resume` sessions, fatal close-code handling (4004/4010–4014), REST sends; slash/interaction handling with `custom_id` round-trips.
   - Slack: Socket Mode `apps.connections.open` → WS; `events_api`/`interactive`/`disconnect` envelopes; REST for thread hydration with `Retry-After` handling.
   - WhatsApp Cloud: webhook + HMAC-SHA256 body verification (fail-closed 401 without `app_secret`), verify-token GET handshake, interactive button/list replies carrying callback ids.
   - WhatsApp Web: wa-rs session lifecycle (QR/pair-code linking, SQLite signal store, `mode="personal"` policies).
   - Signal: signal-cli HTTP daemon — SSE inbound + JSON-RPC 2.0 outbound (`/api/v1/rpc`) — a ready-made external-bridge pattern.
   - Matrix: `matrix-sdk` sync loop + E2EE store; email: IMAP poll + SMTP multipart/alternative + threading headers; IRC: 512-byte PRIVMSG chunking; Git: polling with per-resource `since` cursors (no webhooks needed — great for NAT'd hosts).
5. **Compile-time feature gating** (`channel-*` features in `zeroclaw-channels/Cargo.toml`) — adopt as-is for per-provider cargo features; it is our supply-chain policy's enforcement mechanism.
6. **The `paced_channel` reply-pacing wrapper** (bounded FIFO + LRU idle-state eviction) as optional middleware.
7. **Progress/lifecycle event enum** (`ProgressEvent`) for streaming status RPCs.

### 7.2 REDESIGN (do differently for a library/sidecar)

1. **Invert the dependency direction.** ZeroClaw channels depend on `zeroclaw-config` (TOML schema), `zeroclaw-runtime` (i18n, security scrub, pairing), `zeroclaw-log`, `zeroclaw-memory`, `zeroclaw-tools`. `provider-connect` providers must depend on **nothing but the core + std** — config arrives as typed serde structs from the host, secrets are injected, and observability goes through a tiny host-provided `Observer` trait. This is what makes it embeddable via direct Rust, FFI, and JSON-RPC alike.
2. **Split the monolithic trait into capability traits.** `slack.rs` is 382 KB, `telegram.rs` 309 KB, `matrix.rs` 271 KB — one file, one trait, every method. For a library, define `Provider` (identity, listen, send, health) plus optional marker capabilities: `DraftStreamingProvider`, `TypingProvider`, `ReactionProvider`, `ChoiceProvider`, `MediaProvider`, `VoiceProvider`. Hosts and RPC clients discover capabilities via `initialize` (mirrors ZeroClaw's `supports_*` probes but at the type/wire level, so a minimal Telegram adapter doesn't pay for a Discord embed renderer).
3. **Replace `anyhow` with typed errors** (`thiserror` + an error taxonomy: `Transport`, `Auth`, `RateLimited{retry_after}`, `Malformed`, `Unsupported`) — a library contract, not an app convenience.
4. **Define the JSON-RPC contract explicitly** (ZeroClaw's ACP is the precedent; its `zeroclaw-api/src/jsonrpc.rs` is a head start). Suggested v1 surface:
   - Handshake: `initialize` → `{protocolVersion, capabilities[], providerId, identity, authRequired}` (auth flows like QR/pair-code become async RPC methods: `auth/qr`, `auth/pair_code`, `auth/status`).
   - Outbound (requests): `send`, `send_draft`, `update_draft`, `update_draft_progress`, `finalize_draft`, `cancel_draft`, `send_choice`, `add_reaction`, `remove_reaction`, `start_typing`, `stop_typing`, `health`, `send_media` (or `attachments` field).
   - Inbound (notifications, server→host): `message` (a `ChannelMessage`), `draft_acked`, `send_result`, `progress`, `error`, `auth_required`.
   - Keep it **versioned on the wire** (`"jsonrpc":"2.0"` + `protocolVersion`) and **correlation-keyed** (`message_id` on drafts; approval replies carry `reference`).
   - Media: base64 for small attachments; file-refs (fd/path) for large — do not ship raw `Vec<u8>` over the wire.
5. **Move app policy out.** Peer groups/allowlists, autonomy levels, tool policies, SOP engines, memory — all stay in the host. The library exposes the *primitives* (`explicitly_addressed`, `passive_context`, `conversation_scope`, `mention_only` filtering as an optional middleware) and lets the host decide.
6. **Per-provider crates in-workspace** (not one mega-file): `provider-connect-core` (traits + schemas + JSON-RPC), `provider-connect-transports` (stdio/WS/HTTP), `provider-connect-provider-{discord,telegram,slack,whatsapp,…}` — each optional, each pinned, each with its own feature. This is both our workspace rule ("small helpers in-workspace") and ZeroClaw's feature-flag lesson.
7. **Don't copy the app-side machinery**: agent loop, model providers, memory/embeddings, SOP, hardware, plugins/WASM — out of scope. `provider-connect` is the *channel layer* only.
8. **Binary/RSS budget discipline**: benchmark idle RSS per provider; target < 30–50 MB total. ZeroClaw's 6.6 MB foundation build is the reference point that proves the budget is achievable.
9. **Supply-chain specifics**: prefer hand-rolled-on-`reqwest`/`tokio-tungstenite` adapters (as ZeroClaw did for Discord/Slack/Telegram/WhatsApp Cloud) over heavy SDK crates; where a protocol client is unavoidable (`matrix-sdk`, `nostr-sdk`, `rumqttc`, `lapin`, `whatsapp-rust`), pin exact versions and verify publish dates (all shortlisted crates pass the 14-day gate — see Appendix A). Note: ZeroClaw pins `whatsapp-rust` to a git rev "until upstream ships 0.6.1 to crates.io"; upstream has since shipped **0.7.0 (2025-10-07)** — use the crates.io release, not a git pin.

### 7.3 Suggested milestone order (tomorrow's plan input)

1. `core`: schemas (`ChannelMessage`/`SendMessage` port), `Provider` + capability traits, JSON-RPC v1 contract, stdio transport + `initialize` handshake. *(Mirror ACP's shape; it is already JSON-RPC 2.0 over stdio.)*
2. Providers in order of ROI for the parent ecosystem: **Telegram** (long-poll, zero hosting) → **Discord** (gateway WS) → **Slack** (Socket Mode) → **WhatsApp Cloud** (webhook) → **WhatsApp Web** (wa-rs) → **Signal** (signal-cli bridge) → **Matrix** → email.
3. WS + HTTP transports; then FFI + direct-Rust API; shared test matrix across bindings per CONTRIBUTING.md.

---

## 8. Source URLs

### ZeroClaw (primary)
- Repo: https://github.com/zeroclaw-labs/zeroclaw (canonical; fork: https://github.com/openagen/zeroclaw)
- Channel trait + schemas: https://github.com/zeroclaw-labs/zeroclaw/blob/master/crates/zeroclaw-api/src/channel.rs
- Media types: https://github.com/zeroclaw-labs/zeroclaw/blob/master/crates/zeroclaw-api/src/media.rs
- Channels crate (Cargo.toml with features/deps): https://github.com/zeroclaw-labs/zeroclaw/blob/master/crates/zeroclaw-channels/Cargo.toml
- Channel implementations: https://github.com/zeroclaw-labs/zeroclaw/tree/master/crates/zeroclaw-channels/src (discord/, git/, slack.rs, telegram.rs, whatsapp*.rs, signal.rs, matrix.rs, email_channel.rs, webhook.rs, acp_channel.rs, nostr.rs, …)
- Docs:
  - Channels overview: https://github.com/zeroclaw-labs/zeroclaw/blob/master/docs/book/src/channels/overview.md
  - Telegram: …/channels/telegram.md · Discord: …/channels/discord.md · Slack: …/channels/slack.md · Signal: …/channels/signal.md · WhatsApp: …/channels/whatsapp.md · Matrix: …/channels/matrix.md · Email: …/channels/email.md · Webhook: …/channels/webhook.md · ACP: …/channels/acp.md · LINE: …/channels/line.md · Mattermost: …/channels/mattermost.md · Nextcloud Talk: …/channels/nextcloud-talk.md · Git: …/channels/git.md (+ git-github-app.md, git-gitea-forgejo.md) · Social: …/channels/social.md · Other chat: …/channels/chat-others.md · Voice: …/channels/voice.md · MQTT/AMQP/Filesystem: …/channels/{mqtt,amqp,filesystem}.md
  - Architecture overview: …/architecture/overview.md
  - FND-001 (metrics, 6.6 MB foundation, vision <5 MB): …/foundations/fnd-001-intentional-architecture.md
- Project sites: https://www.zeroclawlabs.ai/ · https://zeroclaw.net/ (claims: <5 MB, <10 ms boot, 400× startup, $10 hardware)

### OpenClaw (context / counter-example)
- Repo: https://github.com/openclaw/openclaw (formerly Clawdbot/Moltbot)
- Site: https://openclaw.ai/ ("29 channels")
- Docs (memory guidance): https://docs.openclaw.ai/install/fly.md (512 MB too small; 2 GB recommended), …/install/raspberry-pi.md (min 1 GB RAM), …/install/digitalocean.md (1 GB tips)
- Root deps: https://github.com/openclaw/openclaw/blob/master/package.json · channel extensions: …/extensions/{discord,whatsapp,telegram,slack,matrix,signal,…}/package.json
- History: https://milvus.io/blog/openclaw-formerly-clawdbot-moltbot-explained-a-complete-guide-to-the-autonomous-ai-agent.md · https://en.wikipedia.org/wiki/OpenClaw

### Third-party comparisons
- https://pinggy.io/blog/zeroclaw_lightweight_openclaw_alternative/ ("resource hog over 1 GB of RAM", ZeroClaw ~8.8 MB binary / <5 MB RAM)
- https://zeroclaw.net/blog/zeroclaw-rust-openclaw-alternative/ (setup walkthrough, multi-channel)
- https://dev.to/lightningdev123/zeroclaw-a-minimal-rust-based-ai-agent-framework-for-self-hosted-systems-5593
- r/raspberry_pi "Personal Assistant Device using OpenClaw and Pi Zero 2W" (community recommending ZeroClaw <5 MB)
- https://github.com/pedroslopez/whatsapp-web.js/issues/75 (memory cost of the Puppeteer approach)

---

## Appendix A — Supply-chain check for shortlisted Rust deps (crates.io `created_at`, verified 2026-08-11)

All candidates are ≥ 14 days old (policy: CONTRIBUTING.md). ZeroClaw's own pins are included as evidence they are proven in production.

| Crate | Latest ver | First published | Age |
|---|---|---|---|
| serde / serde_json | 1.0.229 / 1.0.151 | 2014 / 2015 | ~11.7y / ~11y |
| tokio | 1.53.1 | 2016-07 | ~10.1y |
| reqwest | 0.13.4 | 2016-10 | ~9.8y |
| tokio-tungstenite | 0.30.0 | 2017-03 | ~9.4y |
| axum | 0.8.9 | 2021-07 | ~5y |
| serenity | 0.12.5 | 2016-11 | ~9.7y (we recommend hand-rolled instead) |
| teloxide | 0.17.0 | 2020-02 | ~6.4y (we recommend hand-rolled instead) |
| slack-morphism | 2.24.1 | 2020-08 | ~6y |
| matrix-sdk | 0.18.0 | 2020-05 | ~6.2y |
| nostr-sdk | 0.45.1 | 2022-11 | ~3.7y |
| rumqttc | 0.25.1 | 2020-06 | ~6.1y |
| lapin | 4.10.0 | 2019-04 | ~7.3y |
| lettre | 0.11.23 | 2015-10 | ~10.8y |
| async-imap / mail-parser | 0.11.3 / 0.11.6 | 2019 / 2021 | ~6.7y / ~4.7y |
| **whatsapp-rust / wacore** | **0.7.0** | **2025-10-07** | **307 days** ✅ (ZeroClaw pins git rev cbcdd2a for the older 0.6 line; use crates.io 0.7.0) |
| tokio-rustls / rustls | 0.26.4 / (ring) | 2017-02 / 2016-08 | ~9.5y / ~9.9y |
| thiserror / anyhow | 2.0.20 / 1.0.104 | 2019 | ~6.8y |
| uuid / chrono / base64 / sha2 / hmac | — | 2014–2016 | ~10y |
| tokio-socks | 0.5.3 | 2019-01 | ~7.5y |

**Rule of thumb adopted from ZeroClaw's own choices:** hand-roll REST/WS providers on `reqwest` + `tokio-tungstenite` (they did exactly this for Discord, Slack, Telegram, WhatsApp Cloud, LINE, Reddit, Twitter, Bluesky, Git); use a purpose-built SDK crate only when the protocol is complex and the crate is mature (`matrix-sdk` for E2EE Matrix, `nostr-sdk` for NIPs, `rumqttc`/`lapin` for brokers, `whatsapp-rust` for WhatsApp Web).

---

## Appendix B — Facts verified on 2026-08-11 (research integrity notes)

- GitHub API: zeroclaw-labs/zeroclaw = Rust, 32,555★ / 4,892 forks, created 2026-02-13, pushed 2026-08-11, topics include `openclaw`; openclaw/openclaw = TypeScript, 385,927★; openagen/zeroclaw = fork.
- ZeroClaw commit studied: `82942853` (v0.8.4); channel file sizes (LOC proxy): slack.rs 382 KB, telegram.rs 309 KB, matrix.rs 271 KB, whatsapp_web.rs 167 KB.
- ZeroClaw channels Cargo.toml confirms: no serenity/teloxide; `channel-discord=[]` (zero optional deps → hand-rolled); `matrix-sdk 0.18`; `nostr-sdk 0.44`; `whatsapp-rust` git-pinned; `jsonwebtoken` with `aws_lc_rs` to dodge RUSTSEC-2023-0071.
- OpenClaw root package.json confirms: 64 deps incl. playwright-core, quickjs-wasi, tree-sitter, web-tree-sitter, linkedom, photon-node, matrix WASM crypto (via matrix extension), grammy, baileys (whatsapp extension), @slack/bolt (slack extension).
- ZeroClaw measured binary sizes: 6.6 MB foundation (`--no-default-features`, FND-001 §7), ~8.8 MB full monolith v0.6.x; vision < 5 MB RAM.
- crates.io publish dates fetched via API for Appendix A.
