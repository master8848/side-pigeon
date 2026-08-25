import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import os from "node:os";

/** Locate the `pc` sidecar binary: $PC_BIN, repo target/, common install spots, PATH. */
export function resolvePcBinary(): string {
  const envBin = process.env.PC_BIN;
  if (envBin) return envBin;
  const candidates: string[] = [];
  // Repo-relative lookup works when the extension runs from the repo
  // (plugins/pi-plugin/extension/provider-connect/ -> <repo>/target/...).
  let here: string | undefined;
  try {
    here = path.dirname(fileURLToPath(import.meta.url));
  } catch {
    // jiti/CJS fallback
    here = __dirname;
  }
  if (here) {
    const repo = path.resolve(here, "..", "..", "..");
    candidates.push(path.join(repo, "target", "release", "pc"));
    candidates.push(path.join(repo, "target", "debug", "pc"));
  }
  // Common install spots for the sidecar binary.
  const home = os.homedir();
  candidates.push(
    path.join(home, ".local", "bin", "pc"),
    path.join(home, ".cargo", "bin", "pc"),
    "/opt/homebrew/bin/pc",
    "/usr/local/bin/pc",
  );
  for (const candidate of candidates) {
    try {
      if (existsSync(candidate)) return candidate;
    } catch {
      /* ignore */
    }
  }
  return "pc"; // fall back to PATH
}
