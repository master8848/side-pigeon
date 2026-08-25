/**
 * Mock `pc` sidecar: an EventEmitter with PassThrough stdio that speaks the
 * NDJSON JSON-RPC 2.0 wire contract. Tests inject it via `spawnFn`.
 */
import { EventEmitter } from "node:events";
import { PassThrough } from "node:stream";

export class MockPc extends EventEmitter {
  constructor({ handlers = {}, onExit, exitOnStdinEnd = true } = {}) {
    super();
    this.exitOnStdinEnd = exitOnStdinEnd;
    this.stdin = new PassThrough();
    this.stdout = new PassThrough();
    this.pid = 4242;
    this.requests = [];
    this.killed = false;
    this.handlers = {
      initialize: () => ({
        protocolVersion: "0.1.0",
        methods: ["initialize", "capabilities", "listen", "send", "shutdown"],
        notifications: ["event.message", "event.error"],
        features: ["send"],
        transport: ["stdio"],
        providers: ["demo", "telegram", "discord"],
      }),
      capabilities: () => ({
        protocolVersion: "0.1.0",
        methods: ["initialize", "capabilities", "listen", "send", "shutdown"],
        notifications: ["event.message", "event.error"],
        features: ["send"],
        transport: ["stdio"],
        providers: ["demo"],
      }),
      listen: (params) => ({ started: params?.providers ?? [] }),
      send: () => ({ message_id: "sent-1", ts: Date.now() }),
      shutdown: () => null,
      ...handlers,
    };
    this.onExit = onExit;
    let buf = "";
    this.stdin.on("data", (chunk) => {
      buf += chunk.toString();
      let idx;
      while ((idx = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, idx);
        buf = buf.slice(idx + 1);
        if (!line.trim()) continue;
        let req;
        try {
          req = JSON.parse(line);
        } catch {
          this.respondError(null, -32700, "parse error");
          continue;
        }
        this.requests.push(req);
        if (req.id === undefined || req.id === null) continue; // notification
        const handler = this.handlers[req.method];
        if (!handler) {
          this.respondError(req.id, -32601, `method not found: ${req.method}`);
          continue;
        }
        try {
          const result = handler(req.params);
          Promise.resolve(result).then((v) => this.respond(req.id, v));
        } catch (err) {
          this.respondError(req.id, -32603, err instanceof Error ? err.message : String(err));
        }
      }
    });
    this.stdin.on("finish", () => {
      // Real pc exits when its stdin hits EOF; "finish" is the writable-side
      // end signal on a PassThrough.
      if (this.exitOnStdinEnd) queueMicrotask(() => this.emitExit(0, null));
    });
  }

  emitExit(code, signal) {
    if (this.exited) return;
    this.exited = true;
    this.emit("exit", code, signal);
    if (this.onExit) this.onExit(code, signal);
  }

  respond(id, result) {
    this.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`);
  }

  respondError(id, code, message, data) {
    const error = { code, message };
    if (data !== undefined) error.data = data;
    this.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, error })}\n`);
  }

  notify(method, params) {
    this.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  }

  kill() {
    this.killed = true;
    this.emitExit(0, null);
  }
}

export function demoMessage(overrides = {}) {
  return {
    id: "demo-1",
    channel: "demo",
    channel_id: "demo-room",
    sender: { id: "user-1", name: "Alice", username: null, avatar_url: null },
    reply_target: null,
    content: [{ Text: "hello from the demo channel" }],
    attachments: [],
    explicitly_addressed: false,
    ts: Date.now(),
    raw: null,
    ...overrides,
  };
}
