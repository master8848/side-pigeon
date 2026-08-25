/**
 * dedup plugin — id + echo suppression.
 * Mirrors `opencode-plugin/src/runtime.ts:77` recentIds/recentSent (MAX_RECENT=2000).
 */

import type { Plugin } from "../provider-client.js";
import type { ChannelMessage } from "../schema.js";

const DEFAULT_WINDOW_MS = 5 * 60 * 1000;
const DEFAULT_MAX = 2_000;

export interface DedupOptions {
  /** Time window in ms to remember ids (default 5m). Pruned on check. */
  windowMs?: number;
  /** Max ids to remember (default 2000). Evicts oldest. */
  maxRecent?: number;
}

export function dedup(opts: DedupOptions = {}): Plugin {
  const windowMs = opts.windowMs ?? DEFAULT_WINDOW_MS;
  const maxRecent = opts.maxRecent ?? DEFAULT_MAX;
  const seen = new Map<string, number>();

  function prune(now: number): void {
    for (const [k, at] of seen) {
      if (now - at > windowMs) seen.delete(k);
    }
    if (seen.size > maxRecent) {
      const sorted = [...seen.entries()].sort((a, b) => a[1] - b[1]);
      for (let i = 0; i < seen.size - maxRecent; i++) seen.delete(sorted[i]![0]);
    }
  }

  return {
    name: "dedup",
    onMessage(msg: ChannelMessage): boolean {
      const now = Date.now();
      prune(now);
      if (seen.has(msg.id)) return true; // suppress duplicate
      seen.set(msg.id, now);
      return false;
    },
  };
}

/**
 * Echo dedup — suppress messages that match a recently-sent `message_id`.
 * Complements `dedup` (which keys on inbound `id`). Kept separate since
 * echo ids come from `send` receipts (see runtime.ts:305 recentSent).
 */
export function echoDedup(opts: DedupOptions = {}): { trackSent(provider: string, chat: string, messageId: string): void; isEcho(msg: ChannelMessage): boolean } {
  const windowMs = opts.windowMs ?? DEFAULT_WINDOW_MS;
  const maxRecent = opts.maxRecent ?? DEFAULT_MAX;
  const sent = new Map<string, number>();

  function prune(now: number): void {
    for (const [k, at] of sent) if (now - at > windowMs) sent.delete(k);
    if (sent.size > maxRecent) {
      const sorted = [...sent.entries()].sort((a, b) => a[1] - b[1]);
      for (let i = 0; i < sent.size - maxRecent; i++) sent.delete(sorted[i]![0]);
    }
  }

  return {
    trackSent(provider: string, chat: string, messageId: string): void {
      const now = Date.now();
      prune(now);
      const suffix = messageId.split("/").pop() ?? messageId;
      sent.set(`${provider}:${chat}:${suffix}`, now);
      sent.set(`${provider}:${chat}:${messageId}`, now);
    },
    isEcho(msg: ChannelMessage): boolean {
      if (sent.size === 0) return false;
      const suffix = msg.id.split("/").pop() ?? msg.id;
      return sent.has(`${msg.channel}:${msg.channel_id}:${msg.id}`) || sent.has(`${msg.channel}:${msg.channel_id}:${suffix}`);
    },
  };
}
