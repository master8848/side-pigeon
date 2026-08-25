import { Type } from "typebox";
import { resolvePcBinary } from "./binary.js";
import { PcSidecar } from "./sidecar.js";

export interface RunPcOptions {
  pcBin?: string;
  config?: string;
  provider?: string;
  providers?: string[];
  channelId?: string;
  text?: string;
  replyTo?: string;
  timeoutSecs?: number;
  once?: boolean;
  listen?: boolean;
  send?: boolean;
  check?: boolean;
}

/** Spawn pc, drive one operation, return (summaryText, raw). */
export async function runPc(opts: RunPcOptions): Promise<{ text: string; raw: unknown }> {
  const pcBin = opts.pcBin || resolvePcBinary();
  const args = opts.config ? [pcBin, "-c", opts.config] : [pcBin];
  const sidecar = new PcSidecar(args, {});
  try {
    const caps = (await sidecar.request("initialize")) as Record<string, unknown>;
    let result: unknown = caps;
    let text: string;

    if (opts.check) {
      const providers = Array.isArray(caps.providers) ? (caps.providers as string[]).join(", ") : "(none)";
      text = [
        `pc sidecar: ${pcBin}`,
        `protocolVersion: ${caps.protocolVersion ?? "?"}`,
        `providers: ${providers}`,
        `methods: ${Array.isArray(caps.methods) ? (caps.methods as string[]).join(", ") : "?"}`,
        `features: ${Array.isArray(caps.features) ? (caps.features as string[]).join(", ") : "?"}`,
      ].join("\n");
    } else if (opts.send) {
      const receipt = (await sidecar.request("send", {
        provider: opts.provider,
        message: {
          channel_id: opts.channelId,
          text: opts.text ?? "",
          ...(opts.replyTo ? { reply_to: opts.replyTo } : {}),
        },
      })) as Record<string, unknown>;
      result = receipt;
      text = `sent via ${opts.provider} to ${opts.channelId}: message_id=${receipt.message_id} ts=${receipt.ts}`;
    } else {
      // listen
      const started = (await sidecar.request("listen", opts.providers ? { providers: opts.providers } : undefined)) as Record<string, unknown>;
      const deadline = Date.now() + (opts.timeoutSecs ?? 30) * 1000;
      const messages: Array<Record<string, unknown>> = [];
      let cursor = 0;
      while (Date.now() < deadline) {
        const notifs = sidecar.notificationsSince(cursor);
        cursor = sidecar.countNotifications();
        for (const n of notifs) {
          if (n.method === "event.message") {
            const params = (n.params ?? {}) as Record<string, unknown>;
            const msg = (params.message ?? params) as Record<string, unknown>;
            messages.push(msg);
            if (opts.once) break;
          }
        }
        if (opts.once && messages.length > 0) break;
        await sleep(200);
      }
      result = { started, messages };
      if (messages.length === 0) {
        text = `listened on ${(opts.providers ?? []).join(", ") || "all providers"} for ${opts.timeoutSecs ?? 30}s: no messages`;
      } else {
        text = messages
          .map((m) => {
            const sender = (m.sender ?? {}) as Record<string, unknown>;
            const who = sender.name ?? sender.username ?? sender.id ?? "?";
            const body = extractText(m.content);
            return `${m.channel} chat=${m.channel_id} sender=${who} id=${m.id}: ${body}`;
          })
          .join("\n");
      }
    }
    return { text, raw: result };
  } finally {
    await sidecar.shutdown();
  }
}

export function extractText(content: unknown): string {
  if (!Array.isArray(content)) return String(content ?? "");
  const parts: string[] = [];
  for (const part of content) {
    if (typeof part === "string") {
      parts.push(part);
    } else if (part && typeof part === "object") {
      const p = part as Record<string, unknown>;
      if (typeof p.Text === "string") parts.push(p.Text);
      else if (p.Media) parts.push(`[${String((p.Media as Record<string, unknown>).kind ?? "media").toLowerCase()}]`);
    }
  }
  return parts.join("\n");
}

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

export const pcBin = Type.Optional(Type.String({ description: "Path to the pc sidecar binary (default: $PC_BIN, repo target/, or PATH)" }));
export const config = Type.Optional(Type.String({ description: "Path to a pc JSON config file (default: $PC_CONFIG)" }));

export const pcCheckParams = Type.Object({
  provider: Type.Optional(Type.String({ description: "Only report this provider id" })),
  config,
  pcBin,
});

export const pcSendParams = Type.Object({
  provider: Type.String({ description: "Provider id: telegram, discord, or demo" }),
  channel_id: Type.String({ description: "Chat/room id to send to" }),
  text: Type.String({ description: "Message text" }),
  reply_to: Type.Optional(Type.String({ description: "Provider message id this replies to" })),
  config,
  pcBin,
});

export const pcListenParams = Type.Object({
  provider: Type.Optional(Type.String({ description: "Only start this provider" })),
  timeout_secs: Type.Optional(Type.Number({ description: "Seconds to poll (default 30)" })),
  once: Type.Optional(Type.Boolean({ description: "Stop after the first message" })),
  config,
  pcBin,
});
