#!/usr/bin/env node
/**
 * Cron / CLI example using @provider-connect/core.
 *
 * Two modes (one-shot, no daemon):
 *  --send  -> pc.send() and exit    (like `pc-connect send` but via core)
 *  --poll  -> subscribe + timeout    (like `pc-connect listen --once`)
 *
 * Env: PC_PROVIDERS=demo | PC_TELEGRAM_TOKEN=...  (same contract as pc)
 *      PC_BIN=pc  PC_TELEGRAM_CONFIG=...
 *
 * Examples:
 *  node notify.mjs --send --provider demo --chat room --text "deploy done"
 *  node notify.mjs --poll --timeout 10 --provider demo
 */
import { createProviderClient } from "../../../packages/core/src/index.ts";
import { dedup } from "../../../packages/core/src/plugins/dedup.ts";
import { messageText } from "../../../packages/core/src/schema.ts";

const args = Object.fromEntries(
  [...process.argv.slice(2).reduce((m, a, i, arr) => {
    if (a.startsWith("--")) m.push([a.slice(2), arr[i+1]?.startsWith("--") ? "true" : arr[i+1] ?? "true"]);
    return m;
  }, [])]
);
const provider = args.provider || (process.env.PC_TELEGRAM_TOKEN ? "telegram" : "demo");
const chat = args.chat || "my-room";
const text = args.text || args.message || "hello from cron";
const timeoutMs = Number(args.timeout || 10) * 1000;
const pcBin = process.env.PC_BIN || "pc";
const providers = [{ id: provider, token: process.env[`PC_${provider.toUpperCase()}_TOKEN`] }];

const pc = createProviderClient({ providers, plugins: [dedup()], pcBin });
await pc.start();

if (args.send) {
  // one-shot send, parse receipt/error (same vocabulary as sidecar: -32001..-32603)
  try {
    const receipt = await pc.send({ provider, channelId: chat, text });
    console.log(JSON.stringify(receipt));
  } catch (e) {
    console.error(JSON.stringify({ error: { code: e.code ?? -32603, message: e.message } }));
    process.exitCode = 1;
  } finally { await pc.shutdown(); }
} else {
  // one-shot poll with dedup — exit after first message or timeout
  let done = false;
  const timer = setTimeout(async () => {
    if (!done) { done = true; console.log(JSON.stringify({ event: "timeout" })); await pc.shutdown(); }
  }, timeoutMs);
  pc.subscribe({ provider }, async (msg) => {
    if (done) return; done = true; clearTimeout(timer);
    console.log(JSON.stringify({ event: "message", message: msg, text: messageText(msg) }));
    await pc.shutdown();
  });
  pc.pc?.on("provider-error", async (e) => {
    if (done) return; done = true; clearTimeout(timer);
    console.log(JSON.stringify({ event: "error", error: e }));
    await pc.shutdown();
  });
  // keep process alive until timer or message resolves
}
