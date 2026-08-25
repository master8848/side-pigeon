/**
 * High-level headless client: `createProviderClient` + `createAgentAdapter`.
 *
 * Wraps a `PcClient` (stdio transport) with:
 *  - provider lifecycle (initialize + listen)
 *  - subscribe(filter, cb) => unsubscribe (EventFilter over ChannelMessage)
 *  - send({provider, channelId, text, replyTo, attachments}) -> SendReceipt
 *  - plugins (dedup / logger / retry)
 *  - createSendMutation + use(plugin)
 *  - [Symbol.asyncDispose] for `await using`
 */

import { PcClient, type SpawnFn, type ChildLike } from "./client.js";
import type { ChannelMessage, SendMessage, SendReceipt } from "./schema.js";
import { stdio } from "./transports/stdio.js";

// ------------------------------------------------------------------ types

export interface ProviderDef {
  id: string; // "telegram" | "discord" | "demo"
  token?: string;
  config?: Record<string, unknown>;
}

export interface Transport {
  kind: string;
  options?: unknown;
  connect(): PcClient;
}

export type EventFilter =
  | { provider?: string; channelId?: string; explicitlyAddressed?: boolean }
  | ((msg: ChannelMessage) => boolean);

export interface Plugin {
  name: string;
  /** Return true to suppress delivery of this message. */
  onMessage?(msg: ChannelMessage): boolean | void;
  onError?(err: unknown): void;
}

export interface SendInput {
  provider: string;
  channelId: string;
  text: string;
  replyTo?: string;
  attachments?: SendMessage["attachments"];
}

export interface ProviderClientOptions {
  providers: ProviderDef[];
  transports?: Transport[];
  plugins?: Plugin[];
  pcBin?: string;
  pcArgs?: string[];
  spawnFn?: SpawnFn;
  defaultSendOptions?: { timeoutMs?: number; retryOn?: string[] };
  requestTimeoutMs?: number;
}

export interface ProviderClient {
  readonly pc: PcClient | undefined;
  subscribe(filter: EventFilter, cb: (msg: ChannelMessage) => void): () => void;
  send(msg: SendInput): Promise<SendReceipt>;
  use(plugin: Plugin): void;
  createSendMutation(opts?: {
    onMutate?: (vars: SendInput) => void;
    onSuccess?: (receipt: SendReceipt, vars: SendInput) => void;
    onError?: (err: unknown, vars: SendInput) => void;
  }): {
    mutate(vars: SendInput): Promise<SendReceipt>;
    mutateAsync(vars: SendInput): Promise<SendReceipt>;
  };
  start(): Promise<void>;
  shutdown(): Promise<void>;
  [Symbol.asyncDispose](): Promise<void>;
}

// ------------------------------------------------------------------ helpers

function matchesFilter(msg: ChannelMessage, filter: EventFilter): boolean {
  if (typeof filter === "function") return filter(msg);
  if (filter.provider !== undefined && msg.channel !== filter.provider) return false;
  if (filter.channelId !== undefined && msg.channel_id !== filter.channelId) return false;
  if (
    filter.explicitlyAddressed !== undefined &&
    Boolean(msg.explicitly_addressed) !== filter.explicitlyAddressed
  )
    return false;
  return true;
}

function providerEnv(providers: ProviderDef[]): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env };
  env.PC_PROVIDERS = providers.map((p) => p.id).join(",");
  for (const p of providers) {
    if (p.token) env[`PC_${p.id.toUpperCase()}_TOKEN`] = p.token;
    if (p.config && Object.keys(p.config).length > 0)
      env[`PC_${p.id.toUpperCase()}_CONFIG`] = JSON.stringify(p.config);
  }
  return env;
}

// ------------------------------------------------------------------ impl

export function createProviderClient(opts: ProviderClientOptions): ProviderClient {
  const plugins: Plugin[] = [...(opts.plugins ?? [])];
  const listeners: Array<{ filter: EventFilter; cb: (msg: ChannelMessage) => void }> = [];
  let pc: PcClient | undefined;
  let started = false;

  function dispatch(msg: ChannelMessage): void {
    for (const p of plugins) {
      try {
        if (p.onMessage?.(msg)) return; // suppressed
      } catch (err) {
        try {
          p.onError?.(err);
        } catch {
          /* ignore plugin error handler */
        }
      }
    }
    for (const { filter, cb } of listeners) {
      if (matchesFilter(msg, filter)) {
        try {
          cb(msg);
        } catch (err) {
          for (const p of plugins)
            try {
              p.onError?.(err);
            } catch {}
        }
      }
    }
  }

  function ensurePc(): PcClient {
    if (pc) return pc;
    if (opts.transports && opts.transports.length > 0) {
      pc = opts.transports[0]!.connect();
    } else {
      const env = providerEnv(opts.providers);
      const bin = opts.pcBin ?? "pc";
      const args = opts.pcArgs ?? [];
      const t = stdio({
        bin,
        args,
        env,
        spawnFn: opts.spawnFn,
        requestTimeoutMs: opts.requestTimeoutMs,
      });
      pc = t.connect();
    }
    pc.on("message", (m) => dispatch(m as ChannelMessage));
    pc.on("provider-error", (e) => {
      for (const p of plugins)
        try {
          p.onError?.(e);
        } catch {}
    });
    pc.on("protocol-error", (e) => {
      for (const p of plugins)
        try {
          p.onError?.(e);
        } catch {}
    });
    return pc;
  }

  const client: ProviderClient = {
    get pc() {
      return pc;
    },

    subscribe(filter: EventFilter, cb: (msg: ChannelMessage) => void): () => void {
      const entry = { filter, cb };
      listeners.push(entry);
      return () => {
        const idx = listeners.indexOf(entry);
        if (idx !== -1) listeners.splice(idx, 1);
      };
    },

    async send(input: SendInput): Promise<SendReceipt> {
      const c = ensurePc();
      const message: Record<string, unknown> = { channel_id: input.channelId, text: input.text };
      if (input.replyTo) message.reply_to = input.replyTo;
      if (input.attachments && input.attachments.length > 0)
        message.attachments = input.attachments;
      else message.attachments = [];
      const receipt = await c.request<SendReceipt>("send", { provider: input.provider, message });
      return receipt;
    },

    use(plugin: Plugin): void {
      plugins.push(plugin);
    },

    createSendMutation(mutationOpts = {}) {
      const mutateAsync = async (vars: SendInput): Promise<SendReceipt> => {
        try {
          mutationOpts.onMutate?.(vars);
          const receipt = await client.send(vars);
          mutationOpts.onSuccess?.(receipt, vars);
          return receipt;
        } catch (err) {
          mutationOpts.onError?.(err, vars);
          throw err;
        }
      };
      return { mutate: mutateAsync, mutateAsync };
    },

    async start(): Promise<void> {
      if (started) return;
      const c = ensurePc();
      await c.request("initialize");
      if (opts.providers.length > 0) {
        await c.request("listen", { providers: opts.providers.map((p) => p.id) });
      }
      started = true;
    },

    async shutdown(): Promise<void> {
      if (pc) {
        await pc.shutdown();
        pc = undefined;
      }
      started = false;
    },

    async [Symbol.asyncDispose](): Promise<void> {
      await client.shutdown();
    },
  };

  return client;
}

// ------------------------------------------------------------------
// Agent adapter: `createAgentAdapter({ onMessage })` subscribes + optional
// session mapping, replacing `runtime.ts:221` `deliver()` for non-opencode agents.
// Simple version: just fan-outs every message to onMessage and lets the agent
// reply by calling `client.send`.
// ------------------------------------------------------------------

export interface AgentAdapterOptions {
  client: ProviderClient;
  onMessage: (
    msg: ChannelMessage,
    reply: (text: string, opts?: { replyTo?: string }) => Promise<SendReceipt>,
  ) => Promise<string | void> | string | void;
  filter?: EventFilter;
}

export function createAgentAdapter(opts: AgentAdapterOptions): { unsubscribe: () => void } {
  const filter: EventFilter = opts.filter ?? {};
  const unsub = opts.client.subscribe(filter, async (msg) => {
    const reply = (text: string, replyOpts?: { replyTo?: string }) =>
      opts.client.send({
        provider: msg.channel,
        channelId: msg.channel_id,
        text,
        replyTo: replyOpts?.replyTo ?? msg.id,
      });
    const result = await opts.onMessage(msg, reply);
    if (typeof result === "string" && result.trim() !== "") {
      await reply(result);
    }
  });
  return { unsubscribe: unsub };
}

// Re-export spawn helpers for consumers that need them
export type { ChildLike, SpawnFn };
export { PcClient };
