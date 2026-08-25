/**
 * Shared NDJSON / JSON-RPC line parser.
 *
 * Extracted from `plugins/opencode-plugin/src/pc-client.ts:134` `onLine` and
 * `examples/node/index.mjs:64` `JsonRpcClient`. This is the single
 * implementation; both call sites should converge here.
 *
 * stdout is one JSON document per line (NDJSON). Each line is either:
 *  - a JSON-RPC response (`{id, result|error}`) matched by `id`
 *  - a notification (`{method, params}`) e.g. `event.message` / `event.error`
 */

import { createInterface, type Interface } from "node:readline";

export interface JsonRpcResponse {
  id?: unknown;
  result?: unknown;
  error?: { code?: number; message?: string; data?: unknown };
  method?: string;
  params?: unknown;
}

export type ParsedLine =
  | {
      kind: "response";
      id: number;
      result?: unknown;
      error?: { code: number; message: string; data?: unknown };
    }
  | { kind: "message"; message: unknown }
  | { kind: "provider-error"; error: unknown }
  | { kind: "notification"; method: string; params: unknown }
  | { kind: "empty" }
  | { kind: "parse-error"; raw: string; error: Error };

/** Parse a single trimmed NDJSON line into a discriminated union. Pure function — no I/O. */
export function parseLine(line: string): ParsedLine {
  const trimmed = line.trim();
  if (trimmed === "") return { kind: "empty" };
  let msg: JsonRpcResponse;
  try {
    msg = JSON.parse(trimmed) as JsonRpcResponse;
  } catch (err) {
    return {
      kind: "parse-error",
      raw: line.slice(0, 500),
      error: err instanceof Error ? err : new Error(String(err)),
    };
  }
  if (msg.id !== undefined && msg.id !== null) {
    if (msg.error) {
      const e = msg.error as { code?: number; message?: string; data?: unknown };
      return {
        kind: "response",
        id: Number(msg.id),
        error: { code: e.code ?? -32603, message: e.message ?? "rpc error", data: e.data },
      };
    }
    return { kind: "response", id: Number(msg.id), result: msg.result };
  }
  if (msg.method === "event.message") {
    const params = (msg.params ?? {}) as { message?: unknown };
    return { kind: "message", message: params.message };
  }
  if (msg.method === "event.error") {
    return { kind: "provider-error", error: msg.params };
  }
  if (msg.method) {
    return { kind: "notification", method: msg.method as string, params: msg.params };
  }
  return { kind: "parse-error", raw: line, error: new Error("unrecognized JSON-RPC frame") };
}

/**
 * Attach a readline NDJSON listener to a `stdout`-like stream.
 * Returns the Interface so callers can `close()` it.
 *
 * `onLine` receives each raw line (before parsing) — callers typically call
 * `parseLine` inside.
 */
export function attachNdjsonReader(
  stdout: NodeJS.ReadableStream,
  onLine: (line: string) => void,
): Interface {
  const rl = createInterface({ input: stdout as unknown as NodeJS.ReadableStream });
  rl.on("line", onLine);
  return rl;
}
