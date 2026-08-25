/**
 * provider-connect extension for Prime Agent.
 *
 * Registers three LLM-callable tools that drive the provider-connect `pc`
 * sidecar (Rust, JSON-RPC 2.0 over stdio) as a subprocess:
 *
 *   pc_check   - provider status / capabilities
 *   pc_send    - send a message to a chat (prefer this over raw provider APIs)
 *   pc_listen  - poll for inbound messages (bounded; not a daemon)
 *
 * Zero npm dependencies: `@earendil-works/pi-coding-agent` (types) and
 * `typebox` (tool schemas) are provided by the Prime Agent runtime's module
 * aliases/virtual modules, so this file installs as-is — copy it to
 * ~/.prime/agent/extensions/ (or load with `prime-agent -e <path>`).
 *
 * The sidecar binary is resolved via $PC_BIN, the provider-connect repo's
 * target/{release,debug}/pc, or PATH. Provider credentials come from the
 * environment (PC_TELEGRAM_TOKEN, PC_DISCORD_TOKEN, ...) or a config file
 * passed via $PC_CONFIG / the tools' `config` parameter.
 */

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import os from "node:os";

// ---------------------------------------------------------------------------
// pc sidecar resolution
// ---------------------------------------------------------------------------

/** Locate the `pc` sidecar binary: $PC_BIN, repo target/, common install spots, PATH. */
function resolvePcBinary(): string {
  const envBin = process.env.PC_BIN;
  if (envBin) return envBin;
  const candidates: string[] = [];
  // Repo-relative lookup works when the extension runs from the repo
  // (pi-plugin/extension/provider-connect.ts -> <repo>/target/...).
  let here: string | undefined;
  try {
    here = path.dirname(fileURLToPath(import.meta.url));
  } catch {
    // jiti/CJS fallback
    here = __dirname;
  }
  if (here) {
    const repo = path.resolve(here, "..", "..");
    candidates.push(path.join(repo, "target", "release", "pc"));
    candidates.push(path.join(repo, "target", "debug", "pc"));
  }
  // Common install spots for the sidecar binary.
  const home = os.homedir();
  candidates.push(
    path.join(home, ".local", "bin", "pc"),
    path.join(home, ".cargo", "bin", "pc"),
    "/opt/homebrew/bin/pc",
    "/usr/local/bin/pc",
  );
  for (const candidate of candidates) {
    try {
      if (existsSync(candidate)) return candidate;
    } catch {
      /* ignore */
    }
  }
  return "pc"; // fall back to PATH
}

// ---------------------------------------------------------------------------
// Minimal JSON-RPC 2.0 client over a child process (NDJSON framing)
// ---------------------------------------------------------------------------

interface PcFrame {
  jsonrpc: string;
  id?: number | string;
  method?: string;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
  params?: unknown;
}

class PcSidecar {
  private child;
  private nextId = 1;
  private pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private notifications: PcFrame[] = [];
  private readerDone: Promise<void>;

  constructor(
    private args: string[],
    private env: Record<string, string | undefined>,
  ) {
    this.child = spawn(args[0], args.slice(1), {
      stdio: ["pipe", "pipe", "inherit"], // pc logs go to stderr
      env: { ...process.env, ...env },
    });
    // A spawn failure (binary missing) must reject pending requests loudly
    // instead of hanging the tool call.
    this.child.on("error", (err) => {
      for (const entry of this.pending.values()) entry.reject(err);
      this.pending.clear();
    });
    this.child.stdin.on("error", () => {
      /* EPIPE after sidecar exit: pending requests are rejected via 'error'
         or the response timeout; do not crash the host. */
    });
    const rl = createInterface({ input: this.child.stdout });
    this.readerDone = new Promise((resolve) => {
      rl.on("line", (line) => {
        if (!line.trim()) return;
        let msg: PcFrame;
        try {
          msg = JSON.parse(line);
        } catch {
          return; // stdout must be NDJSON; ignore junk
        }
        if (msg.id !== undefined && msg.id !== null && msg.method === undefined) {
          const entry = this.pending.get(msg.id as number);
          if (entry) {
            this.pending.delete(msg.id as number);
            if (msg.error) {
              entry.reject(new Error(`${msg.error.code} ${msg.error.message}`));
            } else {
              entry.resolve(msg.result);
            }
          }
        } else if (msg.method) {
          this.notifications.push(msg);
        }
      });
      rl.on("close", () => resolve());
    });
  }

  request(method: string, params?: unknown, timeoutMs = 30_000): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const id = this.nextId++;
      this.pending.set(id, { resolve, reject });
      const frame: PcFrame = { jsonrpc: "2.0", id, method };
      if (params !== undefined) frame.params = params;
      this.child.stdin.write(`${JSON.stringify(frame)}\n`);
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`timeout waiting for '${method}' response from pc`));
      }, timeoutMs);
      // clear the timer when settled
      const origResolve = resolve;
      const origReject = reject;
      this.pending.set(id, {
        resolve: (v) => {
          clearTimeout(timer);
          origResolve(v);
        },
        reject: (e) => {
          clearTimeout(timer);
          origReject(e);
        },
      });
    });
  }

  notificationsSince(last: number): PcFrame[] {
    return this.notifications.slice(last);
  }

  countNotifications(): number {
    return this.notifications.length;
  }

  async shutdown(): Promise<void> {
    try {
      await this.request("shutdown", undefined, 5_000);
    } catch {
      // best effort
    }
    try {
      this.child.stdin.end();
    } catch {
      /* ignore */
    }
    await this.readerDone;
    if (this.child.exitCode === null) {
      const killer = setTimeout(() => this.child.kill(), 5_000);
      await new Promise<void>((resolve) => {
        this.child.once("exit", () => {
          clearTimeout(killer);
          resolve();
        });
        if (this.child.exitCode !== null) resolve();
      });
    }
  }
}

interface RunPcOptions {
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
async function runPc(opts: RunPcOptions): Promise<{ text: string; raw: unknown }> {
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

function extractText(content: unknown): string {
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

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

const pcBin = Type.Optional(Type.String({ description: "Path to the pc sidecar binary (default: $PC_BIN, repo target/, or PATH)" }));
const config = Type.Optional(Type.String({ description: "Path to a pc JSON config file (default: $PC_CONFIG)" }));

const pcCheckParams = Type.Object({
  provider: Type.Optional(Type.String({ description: "Only report this provider id" })),
  config,
  pcBin,
});

const pcSendParams = Type.Object({
  provider: Type.String({ description: "Provider id: telegram, discord, or demo" }),
  channel_id: Type.String({ description: "Chat/room id to send to" }),
  text: Type.String({ description: "Message text" }),
  reply_to: Type.Optional(Type.String({ description: "Provider message id this replies to" })),
  config,
  pcBin,
});

const pcListenParams = Type.Object({
  provider: Type.Optional(Type.String({ description: "Only start this provider" })),
  timeout_secs: Type.Optional(Type.Number({ description: "Seconds to poll (default 30)" })),
  once: Type.Optional(Type.Boolean({ description: "Stop after the first message" })),
  config,
  pcBin,
});

// ---------------------------------------------------------------------------
// Extension entry point
// ---------------------------------------------------------------------------

export default function (pi: ExtensionAPI) {
  pi.registerTool({
    name: "pc_check",
    label: "pc check",
    description:
      "Check the provider-connect `pc` sidecar: which messaging providers are compiled in and their status. Use before sending or listening.",
    parameters: pcCheckParams,
    async execute(_toolCallId, params) {
      const { text } = await runPc({ check: true, pcBin: params.pcBin, config: params.config });
      return { content: [{ type: "text", text }], details: {} };
    },
  });

  pi.registerTool({
    name: "pc_send",
    label: "pc send",
    description:
      "Send a text message through a messaging provider via the provider-connect `pc` sidecar. Prefer this over reimplementing provider APIs. Returns the provider message id.",
    parameters: pcSendParams,
    async execute(_toolCallId, params) {
      const { text } = await runPc({
        send: true,
        pcBin: params.pcBin,
        config: params.config,
        provider: params.provider,
        channelId: params.channel_id,
        text: params.text,
        replyTo: params.reply_to,
      });
      return { content: [{ type: "text", text }], details: {} };
    },
  });

  pi.registerTool({
    name: "pc_listen",
    label: "pc listen",
    description:
      "Poll for inbound messages from messaging providers via the provider-connect `pc` sidecar (bounded; this is not a daemon). Returns messages seen within the timeout.",
    parameters: pcListenParams,
    async execute(_toolCallId, params) {
      const { text } = await runPc({
        listen: true,
        pcBin: params.pcBin,
        config: params.config,
        providers: params.provider ? [params.provider] : undefined,
        timeoutSecs: params.timeout_secs,
        once: params.once,
      });
      return { content: [{ type: "text", text }], details: {} };
    },
  });
}
