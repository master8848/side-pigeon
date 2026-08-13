import path from "node:path";

/**
 * Plugin configuration: opencode plugin options merged over environment
 * fallbacks (`PC_BIN`, `PC_PROVIDERS`, `PC_<ID>_TOKEN`, `PC_<ID>_CONFIG`,
 * `PC_CONFIG`).
 *
 * Tokens can live either in the opencode config (`tokens`) or in the
 * environment (`PC_TELEGRAM_TOKEN=...`) — the sidecar only ever sees the
 * environment, so plugin-supplied tokens are injected into the child env.
 */

export interface PluginOptions {
  /** Path to the `pc` sidecar binary. Default: `$PC_BIN`, else `"pc"` on PATH. */
  pcBin?: string;
  /** Extra CLI args for `pc`, e.g. `["-c", "/path/to/pc-config.json"]`. */
  pcArgs?: string[];
  /** Path to a sidecar config file; passed as `pc -c <path>`. */
  pcConfigFile?: string;
  /** Provider ids to load, e.g. `["telegram", "discord"]`. Default: `$PC_PROVIDERS`. */
  providers?: string[];
  /** Per-provider tokens, e.g. `{ telegram: "123:abc" }`. Default: `$PC_<ID>_TOKEN`. */
  tokens?: Record<string, string>;
  /** Per-provider extra config, e.g. `{ telegram: { base_url: "..." } }`. Default: `$PC_<ID>_CONFIG`. */
  providerConfig?: Record<string, Record<string, unknown>>;
  /** Chat allowlist per provider: `rooms[provider] = [chatId, ...]`. Empty = all chats accepted. */
  rooms?: Record<string, string[]>;
  /** Agent to run for inbound messages (default: session default agent). */
  agent?: string;
  /** Model to run for inbound messages. */
  model?: { providerID: string; modelID: string };
  /** State file for the chat→session mapping (default: `~/.local/state/opencode/provider-connect/state.json`). */
  stateFile?: string;
  /** Session title prefix, e.g. `"Messenger "`. Default: `"[<provider>] "`. */
  sessionPrefix?: string;
  /** Wait for the agent reply to inbound messages (`session.prompt`); default false = fire-and-forget (`promptAsync`). */
  awaitReply?: boolean;
  /** Sender ids to ignore (e.g. your own bot id on providers that echo sends). */
  ignoreSenderIds?: string[];
}

export interface ResolvedConfig {
  pcBin: string;
  pcArgs: string[];
  pcConfigFile?: string;
  providers: string[];
  tokens: Record<string, string>;
  providerConfig: Record<string, Record<string, unknown>>;
  rooms: Record<string, string[]>;
  agent?: string;
  model?: { providerID: string; modelID: string };
  stateFile: string;
  sessionPrefix?: string;
  awaitReply: boolean;
  ignoreSenderIds: Set<string>;
}

/** Default state file location for the chat→session mapping. */
export function defaultStateFile(): string {
  if (process.platform === "win32") {
    const base = process.env.LOCALAPPDATA || process.env.USERPROFILE || ".";
    return path.join(base, "opencode", "provider-connect", "state.json");
  }
  const xdg = process.env.XDG_STATE_HOME;
  const home = process.env.HOME ?? ".";
  const base = xdg && xdg.trim() !== "" ? xdg : path.join(home, ".local", "state");
  return path.join(base, "opencode", "provider-connect", "state.json");
}

/** Split a comma-separated env list like `"demo, telegram"`. */
function splitList(value: string | undefined): string[] {
  if (!value) return [];
  return value
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

/** Parse `PC_<UPPER_ID>_CONFIG` JSON env values. */
function envProviderConfig(providers: string[]): Record<string, Record<string, unknown>> {
  const out: Record<string, Record<string, unknown>> = {};
  for (const id of providers) {
    const raw = process.env[`PC_${id.toUpperCase()}_CONFIG`];
    if (!raw || raw.trim() === "") continue;
    try {
      const parsed = JSON.parse(raw) as unknown;
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        out[id] = parsed as Record<string, unknown>;
      }
    } catch {
      // Invalid JSON in an optional env var: ignore rather than break startup.
    }
  }
  return out;
}

/** Resolve plugin options over environment fallbacks. */
export function resolveConfig(options: PluginOptions): ResolvedConfig {
  const providers = options.providers ?? splitList(process.env.PC_PROVIDERS);
  const tokens: Record<string, string> = { ...options.tokens };
  for (const id of providers) {
    const envToken = process.env[`PC_${id.toUpperCase()}_TOKEN`];
    if (envToken && tokens[id] === undefined) tokens[id] = envToken;
  }
  const providerConfig = {
    ...envProviderConfig(providers),
    ...options.providerConfig,
  };
  return {
    pcBin: options.pcBin ?? process.env.PC_BIN ?? "pc",
    pcArgs: options.pcArgs ?? [],
    pcConfigFile: options.pcConfigFile,
    providers,
    tokens,
    providerConfig,
    rooms: options.rooms ?? {},
    agent: options.agent,
    model: options.model,
    stateFile: options.stateFile ?? defaultStateFile(),
    sessionPrefix: options.sessionPrefix,
    awaitReply: options.awaitReply ?? false,
    ignoreSenderIds: new Set(options.ignoreSenderIds ?? []),
  };
}

/**
 * Build the child environment: base env + `PC_PROVIDERS` + per-provider vars.
 * With `-c <file>` the sidecar ignores env config entirely, so PC_* vars are
 * stripped to keep host tokens out of the child environment.
 */
export function childEnv(config: ResolvedConfig): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env };
  delete env.PC_CONFIG; // a stray host PC_CONFIG would shadow the plugin's env config
  if (config.pcConfigFile) {
    for (const key of Object.keys(env)) {
      if (
        key === "PC_PROVIDERS" ||
        (key.startsWith("PC_") && key.endsWith("_TOKEN")) ||
        (key.startsWith("PC_") && key.endsWith("_CONFIG"))
      ) {
        delete env[key];
      }
    }
    return env;
  }
  env.PC_PROVIDERS = config.providers.join(",");
  for (const [id, token] of Object.entries(config.tokens)) {
    env[`PC_${id.toUpperCase()}_TOKEN`] = token;
  }
  for (const [id, extra] of Object.entries(config.providerConfig)) {
    if (Object.keys(extra).length > 0) {
      env[`PC_${id.toUpperCase()}_CONFIG`] = JSON.stringify(extra);
    }
  }
  return env;
}
