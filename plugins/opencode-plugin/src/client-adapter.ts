/**
 * Adapter from the opencode SDK client to the minimal `ClientLike` surface
 * the runtime needs. The SDK returns `RequestResult` wrappers
 * (`{ data, error, request, response }`); this unwraps them and throws on
 * error so the runtime can treat session calls as plain promises.
 */

import type { ClientLike } from "./runtime.js";

/**
 * The SDK client's session surface. The generic method signatures don't
 * structurally line up with the runtime's narrow interface, so the adapter
 * boundary is loosely typed and re-checked at the seam (one place).
 */
interface SdkClientSession {
  list(...args: unknown[]): Promise<unknown>;
  create(...args: unknown[]): Promise<unknown>;
  prompt(...args: unknown[]): Promise<unknown>;
  promptAsync(...args: unknown[]): Promise<unknown>;
}

interface SdkResult {
  data?: unknown;
  error?: unknown;
}

/** Wrap an SDK client so its session methods satisfy `ClientLike`. */
export function adaptClient(client: { session: SdkClientSession }): ClientLike {
  const unwrap = async (result: Promise<unknown>): Promise<unknown> => {
    const res = (await result) as SdkResult;
    if (res && typeof res === "object" && "error" in res && res.error) {
      throw res.error;
    }
    return res && typeof res === "object" && "data" in res ? res.data : res;
  };
  return {
    session: {
      list: async () =>
        (await unwrap(client.session.list())) as Array<{ id: string; title: string }>,
      create: async (options) =>
        (await unwrap(client.session.create(options))) as { id: string; title: string },
      prompt: async (options) => unwrap(client.session.prompt(options)),
      promptAsync: async (options) => unwrap(client.session.promptAsync(options)),
    },
  };
}
