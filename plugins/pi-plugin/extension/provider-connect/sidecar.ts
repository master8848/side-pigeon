import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

export interface PcFrame {
  jsonrpc: string;
  id?: number | string;
  method?: string;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
  params?: unknown;
}

export class PcSidecar {
  private child;
  private nextId = 1;
  private pending = new Map<
    number,
    { resolve: (v: unknown) => void; reject: (e: Error) => void }
  >();
  private notifications: PcFrame[] = [];
  private readerDone: Promise<void>;

  constructor(
    private args: string[],
    private env: Record<string, string | undefined>,
  ) {
    this.child = spawn(args[0], args.slice(1), {
      stdio: ["pipe", "pipe", "inherit"], // pc logs go to stderr
      env: { ...process.env, ...env },
    });
    // A spawn failure (binary missing) must reject pending requests loudly
    // instead of hanging the tool call.
    this.child.on("error", (err) => {
      for (const entry of this.pending.values()) entry.reject(err);
      this.pending.clear();
    });
    this.child.stdin.on("error", () => {
      /* EPIPE after sidecar exit: pending requests are rejected via 'error'
         or the response timeout; do not crash the host. */
    });
    const rl = createInterface({ input: this.child.stdout });
    this.readerDone = new Promise((resolve) => {
      rl.on("line", (line) => {
        if (!line.trim()) return;
        let msg: PcFrame;
        try {
          msg = JSON.parse(line);
        } catch {
          return; // stdout must be NDJSON; ignore junk
        }
        if (msg.id !== undefined && msg.id !== null && msg.method === undefined) {
          const entry = this.pending.get(msg.id as number);
          if (entry) {
            this.pending.delete(msg.id as number);
            if (msg.error) {
              entry.reject(new Error(`${msg.error.code} ${msg.error.message}`));
            } else {
              entry.resolve(msg.result);
            }
          }
        } else if (msg.method) {
          this.notifications.push(msg);
        }
      });
      rl.on("close", () => resolve());
    });
  }

  request(method: string, params?: unknown, timeoutMs = 30_000): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const id = this.nextId++;
      this.pending.set(id, { resolve, reject });
      const frame: PcFrame = { jsonrpc: "2.0", id, method };
      if (params !== undefined) frame.params = params;
      this.child.stdin.write(`${JSON.stringify(frame)}\n`);
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`timeout waiting for '${method}' response from pc`));
      }, timeoutMs);
      // clear the timer when settled
      const origResolve = resolve;
      const origReject = reject;
      this.pending.set(id, {
        resolve: (v) => {
          clearTimeout(timer);
          origResolve(v);
        },
        reject: (e) => {
          clearTimeout(timer);
          origReject(e);
        },
      });
    });
  }

  notificationsSince(last: number): PcFrame[] {
    return this.notifications.slice(last);
  }

  countNotifications(): number {
    return this.notifications.length;
  }

  async shutdown(): Promise<void> {
    try {
      await this.request("shutdown", undefined, 5_000);
    } catch {
      // best effort
    }
    try {
      this.child.stdin.end();
    } catch {
      /* ignore */
    }
    await this.readerDone;
    if (this.child.exitCode === null) {
      const killer = setTimeout(() => this.child.kill(), 5_000);
      await new Promise<void>((resolve) => {
        this.child.once("exit", () => {
          clearTimeout(killer);
          resolve();
        });
        if (this.child.exitCode !== null) resolve();
      });
    }
  }
}
