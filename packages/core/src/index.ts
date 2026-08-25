/**
 * @provider-connect/core — headless provider-connect client.
 *
 * 5-line usage (any agent):
 *   import { createProviderClient } from "@provider-connect/core";
 *   const pc = createProviderClient({ providers: [{ id: "telegram", token }], pcBin: "pc" });
 *   await pc.start();
 *   pc.subscribe({}, (msg) => console.log(msg));
 *   await pc.send({ provider: "telegram", channelId: "123", text: "hello" });
 */

// Schema (source of truth — mirrors crates/provider-core/src/schema.rs)
export type {
  ChannelMessage,
  Sender,
  ContentPart,
  MediaAttachment,
  MediaKind,
  SendMessage,
  SendReceipt,
  WireError,
  WireMessage,
} from "./schema.js";
export {
  encodeBytes,
  decodeBytes,
  inlineAttachment,
  partText,
  contentText,
  senderLabel,
  messageText,
} from "./schema.js";

// NDJSON / JSON-RPC line parser (shared; see also plugins/opencode-plugin/src/pc-client.ts:134)
export { parseLine, attachNdjsonReader } from "./ndjson.js";
export type { ParsedLine, JsonRpcResponse } from "./ndjson.js";

// Low-level stdio client (NDJSON over child process)
export { PcClient, RpcError, adaptBunSpawn } from "./client.js";
export type { ChildLike, SpawnFn, PcClientOptions } from "./client.js";

// Transports
export { stdio } from "./transports/stdio.js";
export type { StdioTransport, StdioTransportOptions } from "./transports/stdio.js";

// Plugins
export { dedup, echoDedup } from "./plugins/dedup.js";
export type { DedupOptions } from "./plugins/dedup.js";

// High-level headless client
export { createProviderClient, createAgentAdapter } from "./provider-client.js";
export type {
  ProviderDef,
  Transport,
  EventFilter,
  Plugin,
  SendInput,
  ProviderClientOptions,
  ProviderClient,
  AgentAdapterOptions,
} from "./provider-client.js";

// FFI cdylib binding (Bun-first via bun:ffi, Node via koffi/ffi-napi, stdio fallback)
export {
  tryLoadFfi,
  createFfiTransport,
  MAX_POLL,
} from "./ffi.js";
export type { FfiLib, FfiHandle, FfiTransport, FfiTransportOptions } from "./ffi.js";
