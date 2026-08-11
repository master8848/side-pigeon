# provider-connect

Rust sidecar library + binary that connects AI agents to messaging providers
(Discord, WhatsApp, Telegram, Slack, ...) with a clean, language-agnostic API:
JSON-RPC 2.0 over stdio (primary), WebSocket and HTTP (optional), plus direct
Rust library calls. Target: idle RSS < 30-50 MB (fixes the ~400 MB idle-RAM
problem of JS agent SDKs).

Status: implementation in progress (see docs/architecture.md).
