// Search over a tiny corpus, loaded into the Qdrant at PISA_TEST_QDRANT_URL
// (skipped without one), as crates/pisa-python/tests/test_search.py does.

import { execFileSync } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { PisaClient } from "../../src/index.ts";
import { binary, freePort, health, start, tempDir, until } from "./cluster.ts";

const qdrant = process.env["PISA_TEST_QDRANT_URL"];
const ROOT = join(import.meta.dirname, "../../../../..");

const DECLS = [
  { name: "Matrix", kind: "def", signature: "Matrix (m n α : Type) : Type", type_refs: [] },
  {
    name: "Matrix.trace",
    kind: "def",
    signature: "Matrix.trace {n R} [Fintype n] [AddCommMonoid R] (A : Matrix n n R) : R",
    type_refs: ["Matrix"],
    docstring: "The trace of a square matrix: the sum of its diagonal entries.",
  },
  {
    name: "Matrix.trace_one",
    kind: "theorem",
    signature: "Matrix.trace_one [Fintype n] [DecidableEq n] : Matrix.trace (1 : Matrix n n R) = Fintype.card n",
    type_refs: ["Matrix.trace", "Matrix"],
    docstring: "The trace of the identity matrix is the dimension.",
  },
  {
    name: "Real.sqrt_mul_self",
    kind: "theorem",
    signature: "Real.sqrt_mul_self {a : ℝ} (h : 0 ≤ a) : √(a * a) = a",
    type_refs: [],
  },
];

describe.skipIf(qdrant === undefined)("search", () => {
  const envId = `vitest-${process.pid}`;
  let dir: string | undefined;
  let server: ChildProcess | undefined;
  let client: PisaClient;

  beforeAll(async () => {
    dir = tempDir();
    const exe = binary("pisa-search");
    const run = (...args: string[]) => execFileSync(exe, args, { env: { ...process.env, RUST_LOG: "warn" } });
    const exportFile = join(dir, "export.ndjson");
    const records = DECLS.map((d) =>
      JSON.stringify({ record: "decl", module: "Mathlib.Fixture", library: "Mathlib", type_pp_full: d.signature, ...d }),
    );
    writeFileSync(exportFile, `${records.join("\n")}\n`);
    run("ingest", "--export", exportFile, "--project", join(ROOT, "docker/lean-project"), "--out", join(dir, "corpus"));
    run("build", "--corpus", join(dir, "corpus"), "--out", join(dir, "bundle"));
    // An environment of the test's own, so loading touches nothing else.
    const manifestFile = join(dir, "bundle/manifest.json");
    const manifest = JSON.parse(readFileSync(manifestFile, "utf8")) as Record<string, unknown>;
    writeFileSync(manifestFile, JSON.stringify({ ...manifest, env_id: envId }));
    run("load", "--bundle", join(dir, "bundle"), "--qdrant", qdrant ?? "", "--min-coverage", "0");

    const port = await freePort();
    const url = `http://127.0.0.1:${port}`;
    server = start("pisa-server", [
      "--http",
      `127.0.0.1:${port}`,
      "--database-url",
      join(dir, "pisa.db"),
      "--qdrant",
      qdrant ?? "",
      "--env-id",
      envId,
      "--no-snapshots",
    ]);
    await until("search", async () => (await health(url)).search?.ready === true);
    client = new PisaClient({ url });
  });

  afterAll(async () => {
    server?.kill();
    for (const collection of [`pisa-${envId}`, `pisa-${envId}-entries`]) {
      await fetch(`${qdrant}/collections/${collection}`, { method: "DELETE" }).catch(() => undefined);
    }
    if (dir !== undefined) rmSync(dir, { recursive: true, force: true });
  });

  it("finds declarations by description, name and batch", async () => {
    const { hits } = await client.search({ nl: "trace of the identity matrix" });
    expect(hits[0]?.name).toBe("Matrix.trace_one");
    const byName = await client.search({ name: "Real.sqrt_mul_sefl", k: 1 });
    expect(byName.hits.map((h) => h.name)).toEqual(["Real.sqrt_mul_self"]);
    const [trace, sqrt] = await client.searchBatch([{ q: "Matrix.trace" }, { nl: "sqrt of a product", kinds: ["theorem"] }]);
    expect(trace?.hits[0]?.name).toBe("Matrix.trace");
    expect(sqrt?.hits[0]?.name).toBe("Real.sqrt_mul_self");
  });

  it("resolves names and details declarations", async () => {
    const [missing, found] = await client.resolve(["Real.sqrt_mul_sefl", "Matrix.trace"]);
    expect(missing).toMatchObject({ exists: false, suggestions: [{ name: "Real.sqrt_mul_self" }] });
    expect(found?.exists).toBe(true);
    const decl = await client.decl("Matrix.trace");
    expect(decl.deps).toEqual(["Matrix"]);
    expect(decl.used_by).toContain("Matrix.trace_one");
    expect(await client.decl("Real.sqrt_mul_sefl").catch((e: unknown) => e)).toMatchObject({ kind: "not_found" });
  });

  it("pins the environment, and refuses snapshots it doesn't search", async () => {
    expect(await client.search({ nl: "trace" }, { envId: "other" }).catch((e: unknown) => e)).toMatchObject({
      kind: "not_found",
    });
    expect(await client.search({ nl: "trace", snapshots: "all" }).catch((e: unknown) => e)).toMatchObject({
      kind: "bad_request",
      status: 400,
    });
  });
});
