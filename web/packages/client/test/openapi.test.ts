import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

import { expect, it } from "vitest";

const WEB = resolve(import.meta.dirname, "../../..");

it("generates its types from the committed openapi.json", () => {
  const scripts = JSON.parse(readFileSync(join(WEB, "package.json"), "utf8")).scripts as Record<string, string>;
  const dir = mkdtempSync(join(tmpdir(), "pisa-openapi-"));
  try {
    // The workspace's `generate`, into a file of its own.
    const generated = join(dir, "openapi.ts");
    const [bin = "", ...args] = (scripts["generate"] ?? "").split(" ");
    args[args.indexOf("-o") + 1] = generated;
    execFileSync(join(WEB, "node_modules/.bin", bin), args, { cwd: WEB, stdio: "ignore" });
    const committed = readFileSync(join(import.meta.dirname, "../src/openapi.ts"), "utf8");
    expect(readFileSync(generated, "utf8"), "src/openapi.ts is out of date: run `pnpm -C web generate`").toBe(committed);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

it("names every schema the API has", () => {
  const generated = readFileSync(join(import.meta.dirname, "../src/openapi.ts"), "utf8");
  const types = readFileSync(join(import.meta.dirname, "../src/types.ts"), "utf8");
  const schemas = [...generated.matchAll(/^export type (\w+) = components\['schemas'\]/gm)].map((m) => m[1] ?? "");
  // Re-exported under another name where the schema's would hide a global.
  const renamed: Record<string, string> = { Error: "ApiError", Document: "LeanDocument" };
  const missing = schemas.filter((s) => !new RegExp(`\\b${renamed[s] ?? s}\\b`).test(types));
  expect(missing).toEqual([]);
});
