import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { ProviderConnectRuntime } from "../dist/runtime.js";
import { resolveConfig } from "../dist/config.js";
import { MockPc, demoMessage } from "./helpers.mjs";

/** Fake opencode client: records session calls, returns canned sessions. */
class FakeClient {
  constructor() {
    this.sessions = [];
    this.created = [];
    this.prompts = [];
    this.promptAsyncs = [];
    this.failPromptAsync = null; // fn(opts) => error to throw, or null
  }
  session = {
    list: async () => this.sessions.map(({ id, title }) => ({ id, title })),
    create: async ({ body }) => {
      const session = { id: `sess-${this.sessions.length + 1}`, title: body.title };
      this.sessions.push(session);
      this.created.push(body.title);
      return session;
    },
    prompt: async (opts) => {
      this.prompts.push(opts);
      return { info: {}, parts: [] };
    },
    promptAsync: async (opts) => {
      if (this.failPromptAsync) {
        const err = this.failPromptAsync(opts);
        if (err) throw err;
      }
      this.promptAsyncs.push(opts);
    },
  };
}

function makeRuntime({
  options = {},
  client = new FakeClient(),
  handlers = {},
  exitOnStdinEnd = true,
} = {}) {
  const mock = new MockPc({ handlers, exitOnStdinEnd });
  // Unique temp state file per runtime so tests never share mapping state.
  const defaultState = path.join(
    mkdtempSync(path.join(os.tmpdir(), "pc-plugin-test-")),
    "state.json",
  );
  const config = resolveConfig({
    pcBin: "pc",
    providers: ["demo"],
    stateFile: defaultState,
    ...options,
  });
  const runtime = new ProviderConnectRuntime(client, config, {
    spawnFn: () => mock,
    log: { info: () => {}, warn: () => {}, error: () => {} },
  });
  return { mock, client, runtime };
}

async function waitFor(fn, timeoutMs = 1000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const value = fn();
    if (value) return value;
    await new Promise((r) => setTimeout(r, 5));
  }
  throw new Error(`waitFor timed out after ${timeoutMs}ms`);
}

function withTempState(fn) {
  return async (t) => {
    const dir = await mkdtemp(path.join(os.tmpdir(), "pc-plugin-test-"));
    t.after(() => rm(dir, { recursive: true, force: true }));
    return fn(path.join(dir, "state.json"));
  };
}

test("start() spawns pc and runs initialize + listen handshake", async () => {
  const { mock, runtime } = makeRuntime();
  await runtime.start();
  assert.equal(mock.requests.length, 2);
  assert.equal(mock.requests[0].method, "initialize");
  assert.equal(mock.requests[1].method, "listen");
  assert.deepEqual(mock.requests[1].params, { providers: ["demo"] });
  const status = runtime.status();
  assert.equal(status.running, true);
  assert.deepEqual(status.startedProviders, ["demo"]);
});

test("start() without providers is a documented no-op, not a crash", async () => {
  const { runtime } = makeRuntime({ options: { providers: [] } });
  await runtime.start();
  const status = runtime.status();
  assert.equal(status.running, false);
  assert.match(status.startError, /no providers configured/);
});

test(
  "inbound message creates a session and hands the text to promptAsync",
  withTempState(async (stateFile) => {
    const { mock, client, runtime } = makeRuntime({ options: { stateFile } });
    await runtime.start();
    mock.notify("event.message", { message: demoMessage({ id: "m-1", channel_id: "chat-1" }) });
    await waitFor(() => client.promptAsyncs.length === 1);
    assert.deepEqual(client.created, ["[demo] chat-1"]);
    const call = client.promptAsyncs[0];
    assert.equal(call.path.id, "sess-1");
    assert.deepEqual(call.body.parts, [
      { type: "text", text: "Alice: hello from the demo channel" },
    ]);
    // Mapping persisted.
    const raw = JSON.parse(await readFile(stateFile, "utf8"));
    assert.equal(raw.chats["demo:chat-1"].sessionID, "sess-1");
  }),
);

test(
  "second message in the same chat reuses the mapped session",
  withTempState(async (stateFile) => {
    const { mock, client, runtime } = makeRuntime({ options: { stateFile } });
    await runtime.start();
    mock.notify("event.message", { message: demoMessage({ id: "m-1", channel_id: "chat-1" }) });
    await waitFor(() => client.promptAsyncs.length === 1);
    mock.notify("event.message", { message: demoMessage({ id: "m-2", channel_id: "chat-1" }) });
    await waitFor(() => client.promptAsyncs.length === 2);
    assert.equal(client.created.length, 1);
    assert.equal(client.promptAsyncs[1].path.id, "sess-1");
  }),
);

test(
  "mapping survives restart (loaded from state file)",
  withTempState(async (stateFile) => {
    const { mock, client, runtime } = makeRuntime({ options: { stateFile } });
    await runtime.start();
    mock.notify("event.message", { message: demoMessage({ id: "m-1", channel_id: "chat-1" }) });
    await waitFor(() => client.promptAsyncs.length === 1);
    await runtime.dispose();

    // New runtime instance, same state file, same fake client.
    const client2 = new FakeClient();
    const { mock: mock2, runtime: runtime2 } = makeRuntime({
      options: { stateFile },
      client: client2,
    });
    await runtime2.start();
    mock2.notify("event.message", { message: demoMessage({ id: "m-2", channel_id: "chat-1" }) });
    await waitFor(() => client2.promptAsyncs.length === 1);
    assert.equal(client2.created.length, 0, "no new session created after restart");
    assert.equal(client2.promptAsyncs[0].path.id, "sess-1");
    await runtime2.dispose();
  }),
);

test("messages outside the rooms allowlist are ignored", async () => {
  const { mock, client, runtime } = makeRuntime({ options: { rooms: { demo: ["allowed-room"] } } });
  await runtime.start();
  mock.notify("event.message", { message: demoMessage({ id: "m-1", channel_id: "other-room" }) });
  await new Promise((r) => setTimeout(r, 50));
  assert.equal(client.promptAsyncs.length, 0);
  assert.equal(client.created.length, 0);
});

test("senders in ignoreSenderIds are ignored", async () => {
  const { mock, client, runtime } = makeRuntime({ options: { ignoreSenderIds: ["demo-bot"] } });
  await runtime.start();
  mock.notify("event.message", {
    message: demoMessage({ id: "m-1", sender: { id: "demo-bot", name: "demo" } }),
  });
  await new Promise((r) => setTimeout(r, 50));
  assert.equal(client.promptAsyncs.length, 0);
});

test("duplicate message ids are delivered once", async () => {
  const { mock, client, runtime } = makeRuntime();
  await runtime.start();
  mock.notify("event.message", { message: demoMessage({ id: "dup-1" }) });
  await waitFor(() => client.promptAsyncs.length === 1);
  mock.notify("event.message", { message: demoMessage({ id: "dup-1" }) });
  await new Promise((r) => setTimeout(r, 50));
  assert.equal(client.promptAsyncs.length, 1);
});

test("own echoes are suppressed via send receipt ids", async () => {
  const { mock, client, runtime } = makeRuntime();
  await runtime.start();
  // Agent sends through the tool; sidecar returns receipt "sent-1".
  const receipt = await runtime.sendMessage({ provider: "demo", chat: "demo-room", text: "hi" });
  assert.equal(receipt.message_id, "sent-1");
  const before = client.promptAsyncs.length;
  // Inbound with the same id (discord) or the id suffix (telegram update/msg).
  mock.notify("event.message", { message: demoMessage({ id: "sent-1", channel_id: "demo-room" }) });
  mock.notify("event.message", {
    message: demoMessage({ id: "99/sent-1", channel_id: "demo-room" }),
  });
  await new Promise((r) => setTimeout(r, 50));
  assert.equal(client.promptAsyncs.length, before, "own echoes must not be delivered");
});

test("send_message tool resolves provider/chat from the session mapping", async () => {
  const { mock, client, runtime } = makeRuntime();
  await runtime.start();
  // Bridge chat-2 to a session first.
  mock.notify("event.message", { message: demoMessage({ id: "m-1", channel_id: "chat-2" }) });
  await waitFor(() => client.promptAsyncs.length === 1);
  const tools = runtime.tools();
  const result = await tools.send_message.execute({ text: "reply!" }, { sessionID: "sess-1" });
  assert.match(result.output, /message_id=sent-1/);
  const sendReq = mock.requests.find((r) => r.method === "send");
  assert.deepEqual(sendReq.params, {
    provider: "demo",
    message: { channel_id: "chat-2", text: "reply!" },
  });
});

test("send_message tool reports failure when sidecar is down", async () => {
  const { runtime } = makeRuntime();
  const tools = runtime.tools();
  const result = await tools.send_message.execute({ text: "hi" }, { sessionID: "sess-x" });
  assert.match(result.output, /sidecar is not running/);
});

test("provider_status surfaces event.error and start errors", async () => {
  const { mock, runtime } = makeRuntime();
  await runtime.start();
  mock.notify("event.error", {
    provider: "telegram",
    code: -32003,
    message: "rate limited",
    data: { kind: "RateLimit" },
  });
  await waitFor(() => runtime.status().lastErrors.length === 1);
  const tools = runtime.tools();
  const result = await tools.provider_status.execute({}, { sessionID: "sess-1" });
  const parsed = JSON.parse(result.output);
  assert.equal(parsed.running, true);
  assert.equal(parsed.lastErrors[0].message, "rate limited");
});

test("dispose sends shutdown, ends stdin, and waits for exit", async () => {
  const { mock, runtime } = makeRuntime();
  await runtime.start();
  await runtime.dispose();
  assert.equal(mock.requests.at(-1).method, "shutdown");
  assert.equal(runtime.status().running, false);
});

test("missing session is recreated once and the message redelivered", async () => {
  const { mock, client, runtime } = makeRuntime();
  await runtime.start();
  mock.notify("event.message", { message: demoMessage({ id: "m-1", channel_id: "chat-1" }) });
  await waitFor(() => client.promptAsyncs.length === 1);
  // Session deleted server-side: next delivery 404s once, then succeeds.
  let failures = 1;
  client.failPromptAsync = () => {
    if (failures-- > 0) {
      const err = new Error("session not found");
      err.status = 404;
      return err;
    }
    return null;
  };
  mock.notify("event.message", { message: demoMessage({ id: "m-2", channel_id: "chat-1" }) });
  // The failed attempt is not recorded, so after the retry there are 2 records.
  await waitFor(() => client.promptAsyncs.length === 2);
  assert.equal(client.created.length, 2, "session recreated after 404");
  assert.equal(client.promptAsyncs[1].path.id, "sess-2");
});
