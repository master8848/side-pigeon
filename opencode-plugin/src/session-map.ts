/**
 * Persisted chat→session mapping for the plugin.
 *
 * Key: `"<provider>:<chatId>"`; value: the opencode session id + title the
 * plugin created for that chat, plus timestamps. Stored as a JSON file so a
 * plugin restart keeps routing each chat to the same session.
 */

import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import path from "node:path";

export interface ChatMapping {
  sessionID: string;
  title: string;
  createdAt: number;
  lastMessageAt: number;
}

export interface SessionMapData {
  version: 1;
  chats: Record<string, ChatMapping>;
}

export function chatKey(provider: string, chatId: string): string {
  return `${provider}:${chatId}`;
}

export class SessionMap {
  private readonly file: string;
  private chats: Record<string, ChatMapping> = {};
  private writeChain: Promise<void> = Promise.resolve();

  private constructor(file: string) {
    this.file = file;
  }

  /** Load the mapping from `file` (missing/corrupt file → empty mapping). */
  static async load(file: string): Promise<SessionMap> {
    const map = new SessionMap(file);
    try {
      const raw = await readFile(file, "utf8");
      const data = JSON.parse(raw) as Partial<SessionMapData>;
      if (data && typeof data === "object" && data.chats && typeof data.chats === "object") {
        map.chats = data.chats as Record<string, ChatMapping>;
      }
    } catch {
      // First run or corrupt state: start empty. Corrupt state is overwritten
      // on the next successful save.
    }
    return map;
  }

  get(provider: string, chatId: string): ChatMapping | undefined {
    return this.chats[chatKey(provider, chatId)];
  }

  /** Find the chat routed to a session (reverse lookup for tool defaults). */
  bySessionID(sessionID: string): { provider: string; chatId: string } | undefined {
    for (const [key, mapping] of Object.entries(this.chats)) {
      if (mapping.sessionID === sessionID) {
        const sep = key.indexOf(":");
        return { provider: key.slice(0, sep), chatId: key.slice(sep + 1) };
      }
    }
    return undefined;
  }

  set(provider: string, chatId: string, mapping: ChatMapping): Promise<void> {
    this.chats[chatKey(provider, chatId)] = mapping;
    return this.save();
  }

  delete(provider: string, chatId: string): Promise<void> {
    if (delete this.chats[chatKey(provider, chatId)]) return this.save();
    return Promise.resolve();
  }

  get size(): number {
    return Object.keys(this.chats).length;
  }

  /** Persist to disk (serialized; concurrent saves never interleave). */
  save(): Promise<void> {
    const data: SessionMapData = { version: 1, chats: { ...this.chats } };
    const serialized = JSON.stringify(data, null, 2);
    this.writeChain = this.writeChain.then(async () => {
      await mkdir(path.dirname(this.file), { recursive: true });
      const tmp = `${this.file}.tmp`;
      await writeFile(tmp, serialized, "utf8");
      await rename(tmp, this.file);
    });
    return this.writeChain;
  }
}
