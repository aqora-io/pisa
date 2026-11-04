// Starts pisa-server and workers running fake-repl from `target/debug` (or
// `PISA_BIN_DIR`), as crates/pisa-python/tests/test_client.py does. Build them
// first: `cargo build -p pisa-server -p pisa-worker -p pisa-tests`.

import { type ChildProcess, spawn } from "node:child_process";
import { existsSync, mkdtempSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const BIN: string = process.env["PISA_BIN_DIR"] ?? join(import.meta.dirname, "../../../../../target/debug");

export function binary(name: string): string {
  const path = join(BIN, name);
  if (!existsSync(path)) {
    throw new Error(`${path} is missing: run \`cargo build -p pisa-server -p pisa-worker -p pisa-tests -p pisa-search\``);
  }
  return path;
}

export async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => resolve(typeof address === "object" && address !== null ? address.port : 0));
    });
  });
}

export function tempDir(): string {
  return mkdtempSync(join(tmpdir(), "pisa-web-"));
}

export function start(name: string, args: string[]): ChildProcess {
  return spawn(binary(name), args, {
    env: { ...process.env, RUST_LOG: "warn" },
    stdio: ["ignore", "inherit", "inherit"],
  });
}

/** Polls `ready` until it returns true, for up to `ms`. */
export async function until(what: string, ready: () => Promise<boolean>, ms = 15_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    try {
      if (await ready()) return;
    } catch {
      // Not up yet.
    }
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error(`timed out waiting for ${what}`);
}

export async function health(url: string): Promise<{ workers: number | null; search: { ready: boolean } | null }> {
  const r = await fetch(`${url}/health`);
  return (await r.json()) as { workers: number | null; search: { ready: boolean } | null };
}
