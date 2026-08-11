#!/usr/bin/env node
/**
 * provider-connect Node.js example — low-RAM sidecar pattern.
 *
 * Spawns the compiled `pc` sidecar binary (source at bin/pc, built to
 * target/release/pc under the repo root), drives it over stdio JSON-RPC 2.0
 * (newline-delimited), logs `event.message` notifications, and prints
 * process.memoryUsage() after startup and on every event — demonstrating that
 * the Rust sidecar, not Node, holds the provider connections.
 *
 * Usage:
 *   PROVIDER=telegram|discord TOKEN=<bot token> CHANNEL_ID=<chat id> node index.mjs
 *   PC_BIN=/path/to/pc ... node index.mjs      # skip the build step
 *   RUN_SECONDS=60 ... node index.mjs          # how long to listen (default 30)
 *
 * Zero dependencies — plain Node (child_process, readline, fs, path).
 */
import { spawn, spawnSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, '..', '..'); // examples/node -> repo root
const provider = process.env.PROVIDER || 'telegram';
const token = process.env.TOKEN;
const channelId = process.env.CHANNEL_ID;
const runSeconds = Number(process.env.RUN_SECONDS || 30);

if (!token) {
  console.error('[example] missing TOKEN env var');
  process.exit(2);
}
if (!channelId) {
  console.error('[example] missing CHANNEL_ID env var');
  process.exit(2);
}

/** Resolve (and if needed build) the `pc` sidecar binary. */
function resolvePcBinary() {
  const candidates = [
    process.env.PC_BIN,
    path.join(repoRoot, 'target', 'release', 'pc'),
    path.join(repoRoot, 'target', 'debug', 'pc'),
  ].filter(Boolean);
  for (const c of candidates) {
    if (c && existsSync(c)) return c;
  }
  console.error(
    '[example] pc binary not found; building: cargo build --release -p pc --features telegram,discord',
  );
  const build = spawnSync('cargo', ['build', '--release', '-p', 'pc', '--features', 'telegram,discord'], {
    cwd: repoRoot,
    stdio: 'inherit',
  });
  if (build.status !== 0) {
    throw new Error(`cargo build failed (status ${build.status})`);
  }
  return path.join(repoRoot, 'target', 'release', 'pc');
}

/** Minimal JSON-RPC 2.0 client over a child's stdio (NDJSON). */
class JsonRpcClient {
  constructor(child) {
    this.child = child;
    this.nextId = 1;
    this.pending = new Map();
    this.rl = createInterface({ input: child.stdout });
    this.rl.on('line', (line) => {
      if (!line.trim()) return;
      let msg;
      try {
        msg = JSON.parse(line);
      } catch (err) {
        console.error('[example] unparseable line:', line);
        return;
      }
      if (msg.id !== undefined && msg.id !== null && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.error) {
          reject(new Error(`${msg.error.code} ${msg.error.message}`));
        } else {
          resolve(msg.result);
        }
      } else if (msg.method) {
        this.emit('notification', msg);
      }
    });
    this.listeners = new Map();
  }

  on(event, fn) {
    if (!this.listeners.has(event)) this.listeners.set(event, []);
    this.listeners.get(event).push(fn);
  }

  emit(event, payload) {
    for (const fn of this.listeners.get(event) || []) fn(payload);
  }

  request(method, params) {
    return new Promise((resolve, reject) => {
      const id = this.nextId++;
      this.pending.set(id, { resolve, reject });
      const frame = { jsonrpc: '2.0', id, method };
      if (params !== undefined) frame.params = params;
      this.child.stdin.write(`${JSON.stringify(frame)}\n`);
    });
  }

  notify(method, params) {
    const frame = { jsonrpc: '2.0', method };
    if (params !== undefined) frame.params = params;
    this.child.stdin.write(`${JSON.stringify(frame)}\n`);
  }
}

function mem() {
  const m = process.memoryUsage();
  return `rss=${(m.rss / 1048576).toFixed(1)}MB heapUsed=${(m.heapUsed / 1048576).toFixed(1)}MB`;
}

async function main() {
  const pcBin = resolvePcBinary();
  console.error(`[example] spawning ${pcBin}`);
  const child = spawn(pcBin, [], {
    stdio: ['pipe', 'pipe', 'inherit'], // stderr -> our stderr (tracing logs)
    env: {
      ...process.env,
      PC_PROVIDERS: provider,
      [`PC_${provider.toUpperCase()}_TOKEN`]: token,
    },
  });

  const client = new JsonRpcClient(child);
  let eventsSeen = 0;

  client.on('notification', (msg) => {
    if (msg.method === 'event.message') {
      eventsSeen += 1;
      const m = msg.params?.message || {};
      const text = Array.isArray(m.content)
        ? m.content.map((p) => (typeof p === 'string' ? p : p.Text || '[media]')).join(' ')
        : m.content;
      console.log(
        `[example] event.message #${eventsSeen} channel=${m.channel} channelId=${m.channel_id} ` +
          `sender=${m.sender?.name || m.sender?.username || m.sender?.id} ` +
          `ts=${m.ts} text=${JSON.stringify(text)}`,
      );
      console.log(`[example] node memory on event: ${mem()}`);
    } else if (msg.method === 'event.error') {
      console.error(`[example] event.error: ${JSON.stringify(msg.params)}`);
    } else {
      console.log(`[example] notification ${msg.method}: ${JSON.stringify(msg.params)}`);
    }
  });

  child.on('exit', (code, signal) => {
    console.error(`[example] pc exited (code=${code} signal=${signal})`);
    process.exit(code ?? 1);
  });

  // --- protocol drive ---
  const init = await client.request('initialize');
  console.log(`[example] initialize -> protocolVersion=${init.protocolVersion} providers=${JSON.stringify(init.providers)}`);

  const caps = await client.request('capabilities');
  console.log(`[example] capabilities -> methods=${JSON.stringify(caps.methods)}`);

  const started = await client.request('listen', { providers: [provider] });
  console.log(`[example] listen -> started=${JSON.stringify(started.started)}`);

  console.log(`[example] node memory after startup: ${mem()}`);
  console.log(`[example] listening on ${provider}; sending a test message to ${channelId}`);

  const receipt = await client.request('send', {
    provider,
    // Full SendMessage wire shape (reply_to/attachments are required fields).
    message: {
      channel_id: channelId,
      text: `hello from provider-connect example (node pid ${process.pid})`,
      reply_to: null,
      attachments: [],
    },
  });
  console.log(`[example] send -> message_id=${receipt.message_id} ts=${receipt.ts}`);

  // Listen for RUN_SECONDS (or Ctrl-C), then shut down cleanly.
  const done = new Promise((resolve) => {
    const timer = setTimeout(resolve, runSeconds * 1000);
    const onSignal = () => {
      clearTimeout(timer);
      resolve();
    };
    process.once('SIGINT', onSignal);
    process.once('SIGTERM', onSignal);
  });
  await done;

  console.log(`[example] shutting down after ${runSeconds}s (events seen: ${eventsSeen})`);
  try {
    await client.request('shutdown');
  } catch (err) {
    console.error(`[example] shutdown request failed: ${err.message}`);
  }
  child.stdin.end();
  console.log(`[example] final node memory: ${mem()}`);
}

main().catch((err) => {
  console.error(`[example] fatal: ${err.message}`);
  process.exit(1);
});
