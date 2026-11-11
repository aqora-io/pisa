import { rmSync } from "node:fs";
import { join } from "node:path";

import type { TestProject } from "vitest/node";

import { BIN, freePort, health, start, tempDir, until } from "./cluster.ts";

declare module "vitest" {
  export interface ProvidedContext {
    pisaUrl: string;
    binDir: string;
  }
}

/** A server and one 2-slot worker sharing the job queue in a SQLite file. A
 *  job waits at most a second for a worker, and the browser origin
 *  `http://app.example` may call it. */
export default async function setup(project: TestProject): Promise<() => void> {
  const dir = tempDir();
  const db = join(dir, "pisa.db");
  const port = await freePort();
  const url = `http://127.0.0.1:${port}`;
  const server = start("pisa-server", [
    "--http",
    `127.0.0.1:${port}`,
    "--database-url",
    db,
    "--max-wait-secs",
    "1",
    "--cors-origin",
    "http://app.example",
  ]);
  // The server creates the tables, so the worker doesn't race it to.
  await until("the server", async () => (await fetch(`${url}/health`)).ok);
  const worker = start("pisa-worker", [
    "--database-url",
    db,
    "--version",
    "v",
    "--slots",
    "2",
    "--repl",
    join(BIN, "fake-repl"),
    "--lean",
    join(BIN, "fake-lean"),
    // Snapshots are built and re-checked by the `pisa` tool, here its stand-in.
    "--tool",
    join(BIN, "fake-pisa"),
    // Sessions run on Lean Beam, here its stand-in.
    "--beam-slots",
    "1",
    "--beam-bin",
    join(BIN, "fake-beam"),
  ]);
  await until("the worker", async () => (await health(url)).workers === 1);
  project.provide("pisaUrl", url);
  project.provide("binDir", BIN);
  return () => {
    worker.kill();
    server.kill();
    rmSync(dir, { recursive: true, force: true });
  };
}
