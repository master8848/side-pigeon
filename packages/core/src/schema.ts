/**
 * Schema types mirroring `crates/provider-core/src/schema.rs`.
 *
 * This is the single source of truth for the JSON-RPC wire shape on the TS
 * side (see `docs/api-contract.md`). Rust's `serde` uses externally-tagged
 * enums for `ContentPart` (`{"Text":"…"}` / `{"Media":{…}}`), so the TS types
 * match that exactly. `MediaAttachment.data` is base64-encoded on the wire
 * (`base64_bytes` serde adapter in schema.rs) and decoded here via helpers.
 */

export interface Sender {
  id: string;
  name?: string | null;
  username?: string | null;
  avatar_url?: string | null;
}

export type MediaKind = "Image" | "Audio" | "Video" | "File" | "Sticker";

export interface MediaAttachment {
  kind: MediaKind;
  url?: string | null;
  mime?: string | null;
  /** Inline bytes — base64 string on the wire (`schema.rs:base64_bytes`). Omit for URL refs. */
  data?: string | null;
  caption?: string | null;
}

/** One ordered part of a message body. Wire shape matches `ContentPart` in schema.rs. */
export type ContentPart = { Text: string } | { Media: MediaAttachment };

export interface ChannelMessage {
  id: string;
  channel: string;
  channel_id: string;
  sender: Sender;
  reply_target?: string | null;
  content: ContentPart[];
  thread_ts?: string | null;
  attachments: MediaAttachment[];
  explicitly_addressed?: boolean;
  ts: number;
  raw?: unknown;
}

export interface SendMessage {
  channel_id: string;
  text: string;
  reply_to?: string | null;
  attachments: MediaAttachment[];
}

export interface SendReceipt {
  message_id: string;
  ts: number;
}

/** `event.error` payload (provider_transport::ErrorEvent). */
export interface WireError {
  provider?: string | null;
  code: number;
  message: string;
  data?: unknown;
}

// ------------------------------------------------------------------
// Back-compat alias used by opencode-plugin's `WireMessage` (pc-client.ts:32)
// The opencode-plugin's hand-rolled `WireMessage` was a looser subset of
// ChannelMessage (optional content/attachments/raw, `explicitly_addressed` etc.).
// Re-export it so adapters can migrate gradually.
// ------------------------------------------------------------------
export type WireMessage = ChannelMessage & {
  // Loose compat: some producers send bare strings or unknown objects in content.
  content?: Array<string | { Text?: string } | { Media?: unknown } | unknown>;
};

// ------------------------------------------------------------------
// Base64 helpers for `MediaAttachment.data` (mirrors schema.rs:base64_bytes)
// Input strings are standard RFC 4648 base64; Node/Bun both provide Buffer or
// atob/btoa. This helper is runtime-agnostic.
// ------------------------------------------------------------------

/** Encode raw bytes (Uint8Array) to base64 string for the wire. */
export function encodeBytes(bytes: Uint8Array): string {
  if (typeof Buffer !== "undefined") return Buffer.from(bytes).toString("base64");
  let binary = "";
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]!);
  return btoa(binary);
}

/** Decode a base64 string (as produced by the sidecar) to bytes. */
export function decodeBytes(b64: string): Uint8Array {
  if (typeof Buffer !== "undefined") return new Uint8Array(Buffer.from(b64, "base64"));
  const binary = atob(b64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

/** Build an inline-bytes MediaAttachment (helper matching schema.rs MediaAttachment::inline). */
export function inlineAttachment(
  kind: MediaKind,
  mime: string,
  data: Uint8Array,
  caption?: string,
): MediaAttachment {
  return { kind, mime, data: encodeBytes(data), caption: caption ?? null, url: null };
}

// ------------------------------------------------------------------
// Content helpers (port of plugins/opencode-plugin/src/format.ts). Extracted here
// so any agent (not just opencode) can render ChannelMessage content.
// ------------------------------------------------------------------

export function partText(part: unknown): string {
  if (typeof part === "string") return part;
  if (part && typeof part === "object") {
    const obj = part as Record<string, unknown>;
    if (typeof obj.Text === "string") return obj.Text;
    if ("Media" in obj) {
      const media = obj.Media as Record<string, unknown> | undefined;
      const caption = media && typeof media.caption === "string" ? media.caption : "";
      const kind = media && typeof media.kind === "string" ? media.kind : "media";
      return caption ? `[${kind}] ${caption}` : `[${kind}]`;
    }
  }
  return "[media]";
}

export function contentText(msg: { content?: unknown }): string {
  const parts = Array.isArray((msg as { content?: unknown }).content)
    ? ((msg as { content: unknown[] }).content as unknown[])
    : [];
  return parts.map(partText).join(" ").trim();
}

export function senderLabel(msg: ChannelMessage): string | undefined {
  const s = (msg as ChannelMessage).sender;
  if (!s) return undefined;
  const name = s.name || s.username;
  if (name) return String(name);
  return undefined;
}

export function messageText(msg: ChannelMessage): string {
  const text = contentText(msg);
  if (text === "") return "";
  const label = senderLabel(msg);
  if (label && label !== msg.channel_id) return `${label}: ${text}`;
  return text;
}

// Schema-generated marker: this file is the TS source of truth, mirroring
// `crates/provider-core/src/schema.rs` (checked manually; drift noted in
// docs/phases/07-headless-ts.md: ContentPart::Text(String) vs bare string).
