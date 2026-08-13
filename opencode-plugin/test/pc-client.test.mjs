import { test } from "node:test";
import assert from "node:assert/strict";
import { PcClient, RpcError } from "../dist/pc-client.js";
import { MockPc, demoMessage } from "./helpers.mjs";

function startMock(handlers) {
  const mock = new MockPc({ handlers });
  const client = PcClient.start("pc", [], {}, { spawnFn: () => mock, requestTimeoutMs: 500 });
  return { mock, client };
}

test("request/response roundtrip matches by id (out-of-order)", async () => {
  const { mock, client } = startMock();
  const p1 = client.request("initialize");
  const p2 = client.request("capabilities");
  // Respond out of order.
  mock.respond(2, { caps: true });
  mock.respond(1, { init: true });
  assert.deepEqual(await p1, { init: true });
  assert.deepEqual(await p2, { caps: true });
  assert.equal(mock.requests.length, 2);
  assert.equal(mock.requests[0].jsonrpc, "2.0");
  assert.equal(mock.requests[0].method, "initialize");
});

test("request params are framed as JSON", async () => {
  const { mock, client } = startMock();
  const p = client.request("listen", { providers: ["demo"] });
  mock.respond(1, { started: ["demo"] });
  await p;
  assert.equal(mock.requests[0].jsonrpc, "2.0");
  assert.deepEqual(mock.requests[0].params, { providers: ["demo"] });
});

test("error response rejects with RpcError carrying code", async () => {
  const { mock, client } = startMock();
  const p = client.request("send", {});
  mock.respondError(1, -32002, "auth error", { kind: "Auth" });
  await assert.rejects(p, (err) => {
    assert.ok(err instanceof RpcError);
    assert.equal(err.code, -32002);
    assert.match(err.message, /auth error/);
    return true;
  });
});

test("event.message notification emits parsed message", async () => {
  const { mock, client } = startMock();
  const seen = [];
  client.on("message", (msg) => seen.push(msg));
  mock.notify("event.message", { message: demoMessage({ id: "m-7" }) });
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(seen.length, 1);
  assert.equal(seen[0].id, "m-7");
  assert.equal(seen[0].channel, "demo");
});

test("event.error notification emits provider-error", async () => {
  const { mock, client } = startMock();
  const seen = [];
  client.on("provider-error", (err) => seen.push(err));
  mock.notify("event.error", {
    provider: "telegram",
    code: -32003,
    message: "rate limited",
    data: { kind: "RateLimit" },
  });
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(seen.length, 1);
  assert.equal(seen[0].code, -32003);
});

test("request times out when sidecar never responds", async () => {
  const { mock, client } = startMock();
  mock.handlers.initialize = () => new Promise(() => {}); // never respond
  await assert.rejects(client.request("initialize"), /timed out/);
});

test("unparseable stdout line emits protocol-error", async () => {
  const { mock, client } = startMock();
  const seen = [];
  client.on("protocol-error", (err) => seen.push(err));
  mock.stdout.write("not json\n");
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(seen.length, 1);
  assert.match(seen[0].message, /unparseable/);
});

test("shutdown closes stdin and resolves after exit", async () => {
  const { mock, client } = startMock();
  const shutdown = client.shutdown();
  // The client sends shutdown then ends stdin; mock exits on stdin end.
  await shutdown;
  assert.equal(mock.requests.at(-1).method, "shutdown");
  assert.equal(mock.stdin.writableEnded, true); // stdin was ended by the client
  assert.equal(client.isRunning, false);
});

test("kill() force-terminates a stuck sidecar", async () => {
  const mock = new MockPc({ exitOnStdinEnd: false });
  const client = PcClient.start("pc", [], {}, { spawnFn: () => mock, requestTimeoutMs: 50 });
  mock.handlers.shutdown = () => new Promise(() => {}); // never respond
  await client.shutdown(50);
  assert.equal(mock.killed, true);
});
