/**
 * Minimal JSON-RPC 2.0 client over a child process's stdio (NDJSON framing),
 * speaking the provider-connect wire contract (docs/api-contract.md).
 *
 * stdout is the JSON-RPC channel (one JSON document per line); stderr carries
 * `pc`'s tracing logs and is piped through to this process's stderr. Responses
 * and `event.*` notifications are interleaved on stdout in production order,
 * so responses are matched by id and notifications are emitted as events.
 */

import { spawn } from "node:child_process";
import { createInterface, type Interface } from "node:readline";
import { EventEmitter } from "node:events";

/** The minimal child surface the client needs (satisfied by ChildProcess and test doubles). */
export interface ChildLike {
  pid?: number;
  stdin: { write(chunk: string): unknown; end(): unknown };
  stdout: { on(event: "data" | "error", cb: (...args: unknown[]) => void): unknown };
  stderr?: { on(event: "data" | "error", cb: (...args: unknown[]) => void): unknown };
  on(event: string, cb: (...args: unknown[]) => void): unknown;
  kill(signal?: NodeJS.Signals | number): unknown;
}

export type SpawnFn = (
  bin: string,
  args: string[],
  options: { stdio: ["pipe", "pipe", "pipe" | "inherit"]; env: NodeJS.ProcessEnv },
) => ChildLike;

/** Normalized inbound message (wire shape from provider_core::ChannelMessage). */
export interface WireMessage {
  id: string;
  channel: string;
  channel_id: string;
  sender: {
    id: string;
    name?: string | null;
    username?: string | null;
    avatar_url?: string | null;
  };
  reply_target?: string | null;
  content?: Array<string | { Text?: string } | { Media?: unknown } | unknown>;
  attachments?: Array<unknown>;
  explicitly_addressed?: boolean;
  ts?: number;
  raw?: unknown;
}

/** Payload of the `event.error` notification (provider_transport::ErrorEvent). */
export interface WireError {
  provider?: string | null;
  code: number;
  message: string;
  data?: unknown;
}

/** Error thrown for JSON-RPC error responses. */
export class RpcError extends Error {
  readonly code: number;
  readonly data: unknown;
  constructor(code: number, message: string, data?: unknown) {
    super(message);
    this.name = "RpcError";
    this.code = code;
    this.data = data;
  }
}

export interface PcClientOptions {
  /** Timeout for a request/response roundtrip, ms. Default 10_000. */
  requestTimeoutMs?: number;
}

/**
 * Events:
 *  - `"message"` (WireMessage) — `event.message` notification
 *  - `"provider-error"` (WireError) — `event.error` notification
 *  - `"exit"` ({ code: number | null, signal: NodeJS.Signals | null }) — child exited
 *  - `"protocol-error"` (Error) — unparseable stdout line or unmatched response
 */
export class PcClient extends EventEmitter {
  readonly child: ChildLike;
  private readonly rl: Interface;
  private nextId = 1;
  private readonly pending = new Map<
    number,
    { resolve: (v: unknown) => void; reject: (e: Error) => void; timer: NodeJS.Timeout }
  >();
  private readonly requestTimeoutMs: number;
  private exited = false;

  constructor(child: ChildLike, options: PcClientOptions = {}) {
    super();
    this.child = child;
    this.requestTimeoutMs = options.requestTimeoutMs ?? 10_000;
    this.rl = createInterface({ input: child.stdout as unknown as NodeJS.ReadableStream });
    this.rl.on("line", (line) => this.onLine(line));
    child.on("exit", (code: unknown, signal: unknown) => {
      this.exited = true;
      for (const { reject } of this.pending.values()) {
        reject(
          new Error(`pc exited (code=${String(code)} signal=${String(signal)}) before responding`),
        );
      }
      this.pending.clear();
      this.emit("exit", { code: code as number | null, signal: signal as NodeJS.Signals | null });
    });
  }

  /** Spawn `pc` and return a client connected to its stdout. */
  static start(
    bin: string,
    args: string[],
    env: NodeJS.ProcessEnv,
    options: PcClientOptions & { spawnFn?: SpawnFn } = {},
  ): PcClient {
    const spawnFn: SpawnFn =
      options.spawnFn ??
      ((b, a, o) =>
        spawn(b, a, { stdio: ["pipe", "pipe", "inherit"], env: o.env }) as unknown as ChildLike);
    const child = spawnFn(bin, args, { stdio: ["pipe", "pipe", "inherit"], env });
    return new PcClient(child, options);
  }

  get isRunning(): boolean {
    return !this.exited;
  }

  get pid(): number | undefined {
    return this.child.pid;
  }

  private onLine(line: string): void {
    const trimmed = line.trim();
    if (trimmed === "") return;
    let msg: {
      id?: unknown;
      method?: unknown;
      result?: unknown;
      error?: unknown;
      params?: unknown;
    };
    try {
      msg = JSON.parse(trimmed) as typeof msg;
    } catch (err) {
      this.emit(
        "protocol-error",
        new Error(`unparseable stdout line: ${line.slice(0, 200)} (${String(err)})`),
      );
      return;
    }
    if (msg.id !== undefined && msg.id !== null) {
      const pending = this.pending.get(Number(msg.id));
      if (!pending) {
        this.emit("protocol-error", new Error(`unmatched response for id ${String(msg.id)}`));
        return;
      }
      clearTimeout(pending.timer);
      this.pending.delete(Number(msg.id));
      if (msg.error) {
        const err = msg.error as { code?: number; message?: string; data?: unknown };
        pending.reject(new RpcError(err.code ?? -32603, err.message ?? "rpc error", err.data));
      } else {
        pending.resolve(msg.result);
      }
      return;
    }
    if (msg.method === "event.message") {
      const params = (msg.params ?? {}) as { message?: WireMessage };
      if (params.message) this.emit("message", params.message);
      return;
    }
    if (msg.method === "event.error") {
      this.emit("provider-error", msg.params as WireError);
      return;
    }
    if (msg.method) {
      this.emit("notification", { method: msg.method, params: msg.params });
    }
  }

  /** Send a JSON-RPC request and await its response. */
  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      if (this.exited) {
        reject(new Error("pc is not running"));
        return;
      }
      const id = this.nextId++;
      const frame: Record<string, unknown> = { jsonrpc: "2.0", id, method };
      if (params !== undefined) frame.params = params;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`request ${method} timed out after ${this.requestTimeoutMs}ms`));
      }, this.requestTimeoutMs);
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject, timer });
      try {
        this.child.stdin.write(`${JSON.stringify(frame)}\n`);
      } catch (err) {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  /** Send a JSON-RPC notification (no response expected). */
  notify(method: string, params?: unknown): void {
    if (this.exited) return;
    const frame: Record<string, unknown> = { jsonrpc: "2.0", method };
    if (params !== undefined) frame.params = params;
    this.child.stdin.write(`${JSON.stringify(frame)}\n`);
  }

  /** Send `shutdown`, close stdin, and wait for the child to exit. */
  async shutdown(timeoutMs = 2_000): Promise<void> {
    if (this.exited) return;
    const exited = new Promise<void>((resolve) => {
      if (this.exited) return resolve();
      this.once("exit", () => resolve());
    });
    try {
      await this.request("shutdown");
    } catch {
      // Sidecar already gone or unresponsive; fall through to stdin close.
    }
    try {
      this.child.stdin.end();
    } catch {
      // Already closed.
    }
    const timer = new Promise<void>((resolve) => setTimeout(resolve, timeoutMs));
    await Promise.race([exited, timer]);
    if (!this.exited) {
      this.kill();
    }
  }

  /** Force-kill the child (SIGTERM). */
  kill(): void {
    if (this.exited) return;
    try {
      this.child.kill("SIGTERM");
    } catch {
      // Best effort.
    }
  }
}
