/**
 * provider-connect opencode plugin entrypoint.
 *
 * Loads `pc` (the provider-connect Rust sidecar) as a child process and
 * bridges messaging-provider chats to opencode sessions. See README.md for
 * install/config, and the sibling `cli/` + `agent-skill/` for the
 * out-of-process alternative that does not depend on opencode staying alive.
 *
 * Export shape (opencode plugin loader):
 *   export default { id: "provider-connect", server: async (input, options) => Hooks }
 */

import type { Hooks, PluginInput, PluginModule } from "@opencode-ai/plugin";
import { adaptClient } from "./client-adapter.js";
import { resolveConfig, type PluginOptions } from "./config.js";
import { ProviderConnectRuntime } from "./runtime.js";

export const ProviderConnectServer = async (
  input: PluginInput,
  options?: PluginOptions,
): Promise<Hooks> => {
  const config = resolveConfig(options ?? {});
  const runtime = new ProviderConnectRuntime(adaptClient(input.client), config);
  await runtime.start();

  // Best-effort child cleanup if opencode dies without calling dispose().
  process.once("exit", () => runtime.killOnExit());

  return {
    tool: runtime.tools(),
    dispose: async () => {
      process.removeListener("exit", runtime.killOnExit);
      await runtime.dispose();
    },
  };
};

const plugin: PluginModule = {
  id: "provider-connect",
  server: ProviderConnectServer,
};

export default plugin;
