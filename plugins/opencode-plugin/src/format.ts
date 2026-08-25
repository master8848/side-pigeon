/**
 * Wire-payload formatting helpers for inbound ChannelMessage content.
 *
 * `content` is an ordered list of parts. The sidecar serializes
 * `ContentPart::Text(String)` as `{"Text": "..."}` (serde externally-tagged
 * enum); older examples also tolerated bare strings, so both are accepted
 * here. Media parts are `{"Media": {...}}` and render as a `[media]` marker.
 */

import type { WireMessage } from "./pc-client.js";

/** Render one content part as plain text. */
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

/** Concatenate all content parts into one text string. */
export function contentText(msg: WireMessage): string {
  const parts = Array.isArray(msg.content) ? msg.content : [];
  return parts.map(partText).join(" ").trim();
}

/** A short human label for the sender, when one exists. */
export function senderLabel(msg: WireMessage): string | undefined {
  const sender = msg.sender;
  if (!sender) return undefined;
  const name = sender.name || sender.username;
  if (name) return String(name);
  return undefined;
}

/** Full message body handed to the session: optional sender prefix + text. */
export function messageText(msg: WireMessage): string {
  const text = contentText(msg);
  if (text === "") return "";
  const label = senderLabel(msg);
  // Telegram falls back to the chat id as sender; skip the prefix then.
  if (label && label !== msg.channel_id) return `${label}: ${text}`;
  return text;
}
