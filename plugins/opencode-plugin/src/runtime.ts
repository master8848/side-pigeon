/**
 * ProviderConnectRuntime — wires the `pc` sidecar to opencode sessions.
 *
 * Lifecycle: spawn `pc` → `initialize` + `capabilities` + `listen` → forward
 * `event.message` notifications into per-chat opencode sessions (created on
 * demand, mapping persisted in plugin state) → expose `send_message` and
 * `provider_status` tools → graceful `shutdown` on dispose.
 *
 * The runtime only depends on a minimal `ClientLike` (structurally satisfied
 * by the opencode SDK client) so tests can drive it with fakes.
 */

import { tool } from "@opencode-ai/plugin/tool";
import type { ResolvedConfig } from "./config.js";
import { childEnv } from "./config.js";
import { messageText } from "./format.js";
import { PcClient, RpcError, type SpawnFn, type WireError, type WireMessage } from "./pc-client.js";
import { SessionMap, type ChatMapping } from "./session-map.js";

/** Minimal opencode client surface the runtime needs. */
export interface ClientLike {
  session: {
    list(): Promise<Array<{ id: string; title: string }>>;
    create(options: { body: { title: string } }): Promise<{ id: string; title: string }>;
    prompt(options: { path: { id: string }; body: Record<string, unknown> }): Promise<unknown>;
    promptAsync(options: { path: { id: string }; body: Record<string, unknown> }): Promise<unknown>;
  };
}

/** Log sink (defaults to console). */
export interface Logger {
  info(message: string, meta?: Record<string, unknown>): void;
  warn(message: string, meta?: Record<string, unknown>): void;
  error(message: string, meta?: Record<string, unknown>): void;
}

export const consoleLogger: Logger = {
  info: (m) => console.log(`[provider-connect] ${m}`),
  warn: (m) => console.warn(`[provider-connect] ${m}`),
  error: (m) => console.error(`[provider-connect] ${m}`),
};

export interface SendReceipt {
  message_id: string;
  ts: number;
}

/** A recorded event.error (kept in a bounded ring for `provider_status`). */
export interface ErrorRecord {
  at: number;
  provider?: string | null;
  code: number;
  message: string;
}

export interface RuntimeDeps {
  /** Override the child spawner (tests inject a mock `pc`). */
  spawnFn?: SpawnFn;
  /** Override the session-map loader (tests inject a temp file). */
  loadState?: (file: string) => Promise<SessionMap>;
  /** Logger. */
  log?: Logger;
}

const MAX_RECENT = 2_000;
const MAX_ERRORS = 20;

export class ProviderConnectRuntime {
  readonly config: ResolvedConfig;
  private readonly client: ClientLike;
  private readonly log: Logger;
  private readonly deps: Required<Pick<RuntimeDeps, "loadState">> & RuntimeDeps;

  private pc?: PcClient;
  private map!: SessionMap;
  private started: string[] = [];
  private readonly recentIds = new Map<string, number>();
  private readonly recentSent = new Map<string, number>();
  private readonly errors: ErrorRecord[] = [];
  private messagesHandled = 0;
  private startError?: string;
  private disposed = false;
  private gracefulExit = false;

  constructor(client: ClientLike, config: ResolvedConfig, deps: RuntimeDeps = {}) {
    this.client = client;
    this.config = config;
    this.log = deps.log ?? consoleLogger;
    this.deps = {
      loadState: deps.loadState ?? ((file) => SessionMap.load(file)),
      ...deps,
    };
  }

  // ------------------------------------------------------------------ setup

  /** Spawn the sidecar, handshake, and start listening. Never throws: failures land in `status()`. */
  async start(): Promise<void> {
    try {
      this.map = await this.deps.loadState(this.config.stateFile);
      if (this.config.providers.length === 0) {
        this.startError =
          "no providers configured (set plugin option providers or PC_PROVIDERS); " +
          "inbound messages will not be received";
        this.log.warn(this.startError);
        return;
      }
      const args = this.config.pcConfigFile
        ? ["-c", this.config.pcConfigFile, ...this.config.pcArgs]
        : this.config.pcArgs;
      this.log.info(`spawning sidecar: ${this.config.pcBin} ${args.join(" ")}`);
      this.pc = PcClient.start(this.config.pcBin, args, childEnv(this.config), {
        spawnFn: this.deps.spawnFn,
      });
      this.pc.on("message", (msg: WireMessage) => {
        void this.handleMessage(msg);
      });
      this.pc.on("provider-error", (err: WireError) => this.recordError(err));
      this.pc.on("exit", ({ code, signal }) => {
        if (this.gracefulExit) {
          this.log.info("sidecar exited cleanly");
          return;
        }
        const detail = `sidecar exited unexpectedly (code=${String(code)} signal=${String(signal)})`;
        this.log.error(detail);
        this.recordError({ at: Date.now(), provider: null, code: -32000, message: detail });
      });
      this.pc.on("protocol-error", (err: Error) =>
        this.recordError({ at: Date.now(), provider: null, code: -32700, message: err.message }),
      );

      const caps = await this.pc.request<{ protocolVersion?: string; providers?: string[] }>(
        "initialize",
      );
      this.log.info(
        `sidecar initialized (protocol ${caps?.protocolVersion ?? "?"}, providers ${JSON.stringify(caps?.providers ?? [])})`,
      );
      const started = await this.pc.request<{ started: string[] }>("listen", {
        providers: this.config.providers,
      });
      this.started = started?.started ?? [];
      this.log.info(`listening: ${this.started.join(", ") || "(none started)"}`);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.startError = message;
      this.log.error(`start failed: ${message}`);
    }
  }

  /** Graceful shutdown: `shutdown` request → close stdin → kill if stuck. */
  async dispose(): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    if (this.pc) {
      this.gracefulExit = true;
      await this.pc.shutdown();
      this.pc = undefined;
    }
  }

  /** Best-effort sync kill on hard process exit (child would otherwise orphan). */
  killOnExit(): void {
    if (this.pc?.isRunning) this.pc.kill();
  }

  // ------------------------------------------------------------- inbound

  private async handleMessage(msg: WireMessage): Promise<void> {
    try {
      if (!this.pc) return;
      if (this.config.ignoreSenderIds.has(msg.sender?.id ?? "")) {
        this.log.info(`ignoring message from ${msg.sender?.id} (ignoreSenderIds)`);
        return;
      }
      const key = `${msg.channel}:${msg.channel_id}`;
      const now = Date.now();
      if (this.recentIds.has(msg.id)) return; // duplicate delivery (reconnect replay)
      this.recentIds.set(msg.id, now);
      if (this.recentIds.size > MAX_RECENT) {
        const oldest = [...this.recentIds.entries()].sort((a, b) => a[1] - b[1])[0];
        if (oldest) this.recentIds.delete(oldest[0]);
      }
      if (this.isRecentSend(msg)) {
        this.log.info(`ignoring own echo (${msg.channel}/${msg.id})`);
        return;
      }
      if (!this.chatAllowed(msg.channel, msg.channel_id)) {
        this.log.info(`chat ${key} not in rooms allowlist; ignoring`);
        return;
      }
      const text = messageText(msg);
      if (text === "") {
        this.log.info(`message ${key} has no text content; ignoring`);
        return;
      }
      await this.deliver(key, msg.channel, msg.channel_id, text);
      this.messagesHandled += 1;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.log.error(`handleMessage failed: ${message}`);
      this.recordError({ at: Date.now(), provider: msg.channel, code: -32000, message });
    }
  }

  private chatAllowed(provider: string, chatId: string): boolean {
    const allowed = this.config.rooms[provider];
    if (!allowed || allowed.length === 0) return true;
    return allowed.includes(chatId);
  }

  /** True when `msg` matches a message we recently sent (echo suppression). */
  private isRecentSend(msg: WireMessage): boolean {
    if (this.recentSent.size === 0) return false;
    const suffix = msg.id.split("/").pop() ?? msg.id;
    return (
      this.recentSent.has(`${msg.channel}:${msg.channel_id}:${msg.id}`) ||
      this.recentSent.has(`${msg.channel}:${msg.channel_id}:${suffix}`)
    );
  }

  private async deliver(
    key: string,
    provider: string,
    chatId: string,
    text: string,
  ): Promise<void> {
    let mapping = this.map.get(provider, chatId);
    if (!mapping) {
      mapping = await this.createSession(provider, chatId);
      await this.map.set(provider, chatId, mapping);
    }
    const body: Record<string, unknown> = {
      parts: [{ type: "text", text }],
    };
    if (this.config.agent) body.agent = this.config.agent;
    if (this.config.model) body.model = this.config.model;
    try {
      if (this.config.awaitReply) {
        await this.client.session.prompt({ path: { id: mapping.sessionID }, body });
      } else {
        await this.client.session.promptAsync({ path: { id: mapping.sessionID }, body });
      }
    } catch (err) {
      // If the mapped session is gone, recreate it once and retry.
      if (
        this.looksLikeMissingSession(err) &&
        this.map.get(provider, chatId)?.sessionID === mapping.sessionID
      ) {
        this.log.warn(`session ${mapping.sessionID} missing; recreating for ${key}`);
        const fresh = await this.createSession(provider, chatId);
        await this.map.set(provider, chatId, fresh);
        if (this.config.awaitReply) {
          await this.client.session.prompt({ path: { id: fresh.sessionID }, body });
        } else {
          await this.client.session.promptAsync({ path: { id: fresh.sessionID }, body });
        }
        return;
      }
      throw err;
    }
  }

  private looksLikeMissingSession(err: unknown): boolean {
    const status = (err as { status?: unknown } | undefined)?.status;
    if (status === 404) return true;
    const text = err instanceof Error ? err.message : String(err);
    return /404|not found|no session/i.test(text);
  }

  private async createSession(provider: string, chatId: string): Promise<ChatMapping> {
    const prefix = this.config.sessionPrefix ?? `[${provider}] `;
    const title = `${prefix}${chatId}`;
    const session = await this.client.session.create({ body: { title } });
    const now = Date.now();
    return { sessionID: session.id, title: session.title, createdAt: now, lastMessageAt: now };
  }

  // ------------------------------------------------------------- outbound

  /** Send a message through the sidecar. Provider/chat default from the calling session's mapping. */
  async sendMessage(
    args: { provider?: string; chat?: string; text: string; replyTo?: string },
    sessionID?: string,
  ): Promise<SendReceipt> {
    if (!this.pc) {
      throw new Error(`sidecar is not running${this.startError ? `: ${this.startError}` : ""}`);
    }
    const mapped = sessionID ? this.map.bySessionID(sessionID) : undefined;
    const provider = args.provider ?? mapped?.provider ?? this.config.providers[0];
    const chat = args.chat ?? mapped?.chatId;
    if (!provider) throw new Error("no provider configured; pass provider explicitly");
    if (!chat)
      throw new Error("no chat given and no chat is mapped to this session; pass chat explicitly");
    if (!this.config.providers.includes(provider)) {
      throw new Error(
        `provider "${provider}" is not configured (configured: ${this.config.providers.join(", ") || "none"})`,
      );
    }
    if (!this.chatAllowed(provider, chat)) {
      throw new Error(`chat "${chat}" on provider "${provider}" is not in the rooms allowlist`);
    }
    const message: Record<string, unknown> = { channel_id: chat, text: args.text };
    if (args.replyTo) message.reply_to = args.replyTo;
    const receipt = await this.pc.request<SendReceipt>("send", { provider, message });
    const suffix = receipt.message_id.split("/").pop() ?? receipt.message_id;
    this.recentSent.set(`${provider}:${chat}:${suffix}`, Date.now());
    if (this.recentSent.size > MAX_RECENT) {
      const oldest = [...this.recentSent.entries()].sort((a, b) => a[1] - b[1])[0];
      if (oldest) this.recentSent.delete(oldest[0]);
    }
    return receipt;
  }

  // ------------------------------------------------------------- status

  private recordError(err: WireError | ErrorRecord): void {
    const record: ErrorRecord =
      "at" in err
        ? err
        : { at: Date.now(), provider: err.provider ?? null, code: err.code, message: err.message };
    this.errors.push(record);
    if (this.errors.length > MAX_ERRORS) this.errors.shift();
  }

  /** Snapshot for `provider_status` and logs. */
  status(): Record<string, unknown> {
    return {
      running: Boolean(this.pc?.isRunning),
      pid: this.pc?.pid ?? null,
      configuredProviders: this.config.providers,
      startedProviders: this.started,
      sessionsMapped: this.map ? this.map.size : 0,
      messagesHandled: this.messagesHandled,
      lastErrors: [...this.errors].slice(-5),
      startError: this.startError ?? null,
    };
  }

  // ------------------------------------------------------------- tools

  /** The tools exposed to opencode agents. */
  tools() {
    return {
      send_message: tool({
        description:
          "Send a text message to a chat on a messaging provider (telegram/discord/demo) through the provider-connect sidecar. " +
          "Provider and chat default to the chat this session is bridged to; omit them to reply in the current chat.",
        args: {
          text: tool.schema.string().describe("Message text to send"),
          provider: tool.schema
            .string()
            .optional()
            .describe(
              `Provider id, one of: ${this.config.providers.join(", ") || "(none configured)"}`,
            ),
          chat: tool.schema.string().optional().describe("Chat/room id to send to"),
          replyTo: tool.schema
            .string()
            .optional()
            .describe("Provider message id this replies to (in-thread reply)"),
        },
        // Arrow functions capture the runtime lexically; opencode may call the
        // execute without a `this` binding.
        execute: async (
          args: { text: string; provider?: string; chat?: string; replyTo?: string },
          context: { sessionID: string },
        ) => {
          try {
            const receipt = await this.sendMessage(args, context.sessionID);
            return {
              title: "Message sent",
              output: `sent to ${args.provider ?? "mapped chat"} (message_id=${receipt.message_id}, ts=${receipt.ts})`,
              metadata: receipt,
            };
          } catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            return { title: "Send failed", output: `send_message failed: ${message}` };
          }
        },
      }),
      provider_status: tool({
        description:
          "Report the provider-connect sidecar status: running, providers, sessions bridged, recent provider errors.",
        args: {},
        execute: async () => {
          const status = this.status();
          return {
            title: "provider-connect status",
            output: JSON.stringify(status, null, 2),
            metadata: status,
          };
        },
      }),
    };
  }
}

/** Re-exported for tests that want the RPC error type. */
export { RpcError };
