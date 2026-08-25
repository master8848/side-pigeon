#!/usr/bin/env node
/**
 * Normal app example — HTTP server using @provider-connect/core.
 *
 * Unlike examples/node (raw NDJSON over stdio), this uses the high-level
 * headless client: createProviderClient + subscribe/send + dedup plugin.
 * Zero extra deps — node:http only (no express needed).
 *
 *  PC_PROVIDERS=demo node server.mjs          # demo (no token)
 *  PC_TELEGRAM_TOKEN=123:abc node server.mjs  # real telegram
 *  PC_BIN=pc node server.mjs                  # custom binary path
 */

import http from "node:http";
// Dev: relative import so it runs without `bun add` (use `bun --bun server.mjs` for .ts).
// Published: `bun add @provider-connect/core` then `from "@provider-connect/core"`.
import { createProviderClient, dedup, messageText } from "../../../packages/core/src/index.ts";

const PORT = Number(process.env.PORT || 3000);
const pcBin = process.env.PC_BIN || "pc";

// Pick provider from env — demo by default, telegram if token present
const providers = process.env.PC_TELEGRAM_TOKEN
  ? [{ id: "telegram", token: process.env.PC_TELEGRAM_TOKEN }]
  : [{ id: "demo" }];

const pc = createProviderClient({ providers, plugins: [dedup()], pcBin });
await pc.start();
console.log(`[app] provider-connect started providers=${providers.map((p) => p.id)} rss=${(process.memoryUsage().rss / 1048576).toFixed(1)}MB`);

// SSE fan-out — subscribe once, broadcast to all /events clients
const sseClients = new Set();
pc.subscribe({}, (msg) => {
  const line = `data: ${JSON.stringify(msg)}\n\n`;
  for (const res of sseClients) try { res.write(line); } catch {}
  console.log(`[app] message ${msg.channel}/${msg.channel_id} ${messageText(msg)}`);
});

const server = http.createServer(async (req, res) => {
  if (req.url === "/health") {
    // proxy to sidecar capabilities (proves the process is live)
    try {
      const caps = await pc.pc.request("capabilities");
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: true, caps }));
    } catch (e) { res.writeHead(500); res.end(JSON.stringify({ error: String(e) })); }
    return;
  }
  if (req.url === "/events") {
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
    sseClients.add(res);
    req.on("close", () => sseClients.delete(res));
    return;
  }
  if (req.url === "/send" && req.method === "POST") {
    let body = ""; for await (const c of req) body += c;
    try {
      const { provider, channelId, text } = JSON.parse(body || "{}");
      const receipt = await pc.send({ provider: provider || providers[0].id, channelId, text });
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify(receipt));
    } catch (e) { res.writeHead(400); res.end(JSON.stringify({ error: String(e.message || e) })); }
    return;
  }
  res.writeHead(404); res.end("not found");
});

server.listen(PORT, () => console.log(`[app] listening http://localhost:${PORT}  POST /send  GET /events  GET /health`));

for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, async () => {
  console.log(`[app] ${sig} — shutting down`);
  server.close();
  for (const c of sseClients) try { c.end(); } catch {}
  await pc.shutdown();
  process.exit(0);
});
