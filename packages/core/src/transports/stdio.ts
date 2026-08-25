/**
 * stdio transport — spawns the `pc` sidecar over NDJSON (JSON-RPC 2.0).
 * Mirrors `plugins/opencode-plugin/src/pc-client.ts:111` PcClient.start.
 */

import { spawn } from "node:child_process";
import type { ChildLike, SpawnFn } from "../client.js";
import { PcClient, type PcClientOptions } from "../client.js";

export interface StdioTransportOptions extends PcClientOptions {
  /** Path to `pc` binary. Default `"pc"` on PATH. */
  bin?: string;
  /** Extra CLI args for `pc` (e.g. `["-c","pc.json"]`). */
  args?: string[];
  /** Child env (defaults to process.env + PC_*). */
  env?: NodeJS.ProcessEnv;
  /** Override spawn implementation (Bun.spawn adapter or mock). */
  spawnFn?: SpawnFn;
}

export interface StdioTransport {
  kind: "stdio";
  options: StdioTransportOptions;
  /** Spawn and return a connected PcClient. */
  connect(): PcClient;
}

/** Build a child env snippet for stdio transports (PC_PROVIDERS + tokens). */
function buildEnv(
  base: NodeJS.ProcessEnv | undefined,
  extra: NodeJS.ProcessEnv | undefined,
): NodeJS.ProcessEnv {
  return { ...(base ?? process.env), ...(extra ?? {}) };
}

export function stdio(opts: StdioTransportOptions = {}): StdioTransport {
  return {
    kind: "stdio",
    options: opts,
    connect() {
      const bin = opts.bin ?? "pc";
      const args = opts.args ?? [];
      const env = buildEnv(process.env, opts.env);
      const spawnFn: SpawnFn =
        opts.spawnFn ??
        ((b, a, o) => spawn(b, a, { stdio: ["pipe", "pipe", "inherit"], env: o.env }) as unknown as ChildLike);
      return PcClient.start(bin, args, env, { spawnFn, requestTimeoutMs: opts.requestTimeoutMs });
    },
  };
}

/** Re-export for consumers that want to call `stdio.connect()` directly. */
export { PcClient };
