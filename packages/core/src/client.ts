/**
 * Headless PcClient — JSON-RPC 2.0 over NDJSON stdio.
 * Extracted from `opencode-plugin/src/pc-client.ts` (82) and `examples/node/index.mjs:64`.
 * No dependency on `@opencode-ai/plugin`; works with Node or Bun.
 */

import { spawn } from "node:child_process";
import { EventEmitter } from "node:events";
import { attachNdjsonReader, parseLine } from "./ndjson.js";
import type { Interface } from "node:readline";
import type { ChannelMessage, WireError } from "./schema.js";

/** Minimal child surface (satisfied by ChildProcess, Bun.Subprocess wrapper, and test doubles). */
export interface ChildLike {
  pid?: number;
  stdin: { write(chunk: string): unknown; end(): unknown };
  stdout: { on(event: "data" | "error", cb: (...args: unknown[]) => void): unknown };
  stderr?: { on(event: "data" | "error", cb: (...args: unknown[]) => void): unknown };
  on(event: string, cb: (...args: unknown[]) => void): unknown;
  kill(signal?: NodeJS.Signals | number): unknown;
}

/**
 * Spawn function abstraction — generic over `child_process.spawn` and `Bun.spawn`.
 *
 * Node usage (default):
 *   (bin, args, opts) => spawn(bin, args, { stdio: ["pipe","pipe","inherit"], env: opts.env })
 *
 * Bun usage:
 *   (bin, args, opts) => adaptBunSpawn(Bun.spawn([bin, ...args], { stdin:"pipe", stdout:"pipe", stderr:"inherit", env: opts.env }))
 *
 * Provide `adaptBunSpawn` below to wrap a `Bun.Subprocess` into `ChildLike`.
 */
export type SpawnFn = (
  bin: string,
  args: string[],
  options: { stdio: ["pipe", "pipe", "pipe" | "inherit"]; env: NodeJS.ProcessEnv },
) => ChildLike;

/** Adapt a Bun.Subprocess (or any object with stdin/stdout/stderr + kill) to ChildLike. */
export function adaptBunSpawn(proc: {
  pid?: number;
  stdin: unknown;
  stdout: unknown;
  stderr?: unknown;
  kill(signal?: number | string): unknown;
  on?(event: string, cb: (...args: unknown[]) => void): unknown;
  exited?: Promise<unknown>;
}): ChildLike {
  const stdin = proc.stdin as { write(chunk: string): unknown; end(): unknown };
  const stdout = proc.stdout as { on(event: string, cb: (...args: unknown[]) => void): unknown };
  const stderr = proc.stderr as { on(event: string, cb: (...args: unknown[]) => void): unknown } | undefined;
  const child: ChildLike = {
    pid: proc.pid,
    stdin: stdin ?? { write() {}, end() {} },
    stdout: stdout ?? { on() {} },
    stderr,
    on(event: string, cb: (...args: unknown[]) => void) {
      if (typeof proc.on === "function") return proc.on(event, cb);
      // Bun: `exited` promise resolves on exit; bridge to "exit" event lazily
      if (event === "exit" && proc.exited) {
        void (proc.exited as Promise<unknown>).then(
          () => (cb as (code: number | null, signal: unknown) => void)(0, null),
          (err: unknown) => (cb as (code: unknown, signal: unknown) => void)(err, null),
        );
      }
    },
    kill(signal?: NodeJS.Signals | number) {
      return proc.kill(signal as unknown as string);
    },
  };
  return child;
}

/** Legacy alias — `WireMessage` is `ChannelMessage` (see schema.ts). */
export type WireMessage = ChannelMessage;
export type { ChannelMessage, WireError };

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
  requestTimeoutMs?: number;
}

/**
 * Events:
 *  - "message" (ChannelMessage)
 *  - "provider-error" (WireError)
 *  - "exit" ({ code, signal })
 *  - "protocol-error" (Error)
 *  - "notification" ({ method, params })
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
    this.rl = attachNdjsonReader(child.stdout as unknown as NodeJS.ReadableStream, (line) => this.onLine(line));
    child.on("exit", (code: unknown, signal: unknown) => {
      this.exited = true;
      for (const { reject } of this.pending.values()) {
        reject(new Error(`pc exited (code=${String(code)} signal=${String(signal)}) before responding`));
      }
      this.pending.clear();
      try { this.rl.close(); } catch {}
      this.emit("exit", { code: code as number | null, signal: signal as NodeJS.Signals | null });
    });
  }

  static start(
    bin: string,
    args: string[],
    env: NodeJS.ProcessEnv,
    options: PcClientOptions & { spawnFn?: SpawnFn } = {},
  ): PcClient {
    const spawnFn: SpawnFn =
      options.spawnFn ??
      ((b, a, o) => spawn(b, a, { stdio: ["pipe", "pipe", "inherit"], env: o.env }) as unknown as ChildLike);
    const child = spawnFn(bin, args, { stdio: ["pipe", "pipe", "inherit"], env });
    return new PcClient(child, options);
  }

  get isRunning(): boolean { return !this.exited; }
  get pid(): number | undefined { return this.child.pid; }

  private onLine(line: string): void {
    const parsed = parseLine(line);
    if (parsed.kind === "empty") return;
    if (parsed.kind === "parse-error") {
      this.emit("protocol-error", new Error(`unparseable stdout line: ${parsed.raw.slice(0, 200)} (${String(parsed.error)})`));
      return;
    }
    if (parsed.kind === "response") {
      const pending = this.pending.get(parsed.id);
      if (!pending) {
        this.emit("protocol-error", new Error(`unmatched response for id ${String(parsed.id)}`));
        return;
      }
      clearTimeout(pending.timer);
      this.pending.delete(parsed.id);
      if (parsed.error) {
        pending.reject(new RpcError(parsed.error.code, parsed.error.message, parsed.error.data));
      } else {
        pending.resolve(parsed.result);
      }
      return;
    }
    if (parsed.kind === "message") {
      if (parsed.message) this.emit("message", parsed.message);
      return;
    }
    if (parsed.kind === "provider-error") {
      this.emit("provider-error", parsed.error as WireError);
      return;
    }
    if (parsed.kind === "notification") {
      this.emit("notification", { method: parsed.method, params: parsed.params });
    }
  }

  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      if (this.exited) { reject(new Error("pc is not running")); return; }
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

  notify(method: string, params?: unknown): void {
    if (this.exited) return;
    const frame: Record<string, unknown> = { jsonrpc: "2.0", method };
    if (params !== undefined) frame.params = params;
    this.child.stdin.write(`${JSON.stringify(frame)}\n`);
  }

  async shutdown(timeoutMs = 2_000): Promise<void> {
    if (this.exited) return;
    const exited = new Promise<void>((resolve) => {
      if (this.exited) return resolve();
      this.once("exit", () => resolve());
    });
    try { await this.request("shutdown"); } catch { /* sidecar gone */ }
    try { this.child.stdin.end(); } catch { /* already closed */ }
    const timer = new Promise<void>((resolve) => setTimeout(resolve, timeoutMs));
    await Promise.race([exited, timer]);
    if (!this.exited) this.kill();
  }

  kill(): void {
    if (this.exited) return;
    try { this.child.kill("SIGTERM"); } catch { /* best effort */ }
  }

  [Symbol.asyncDispose]?(): Promise<void> { return this.shutdown(); }
}
