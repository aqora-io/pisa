import { PisaClient, PisaError, type Query, type QueryResult } from "@aqora/pisa-client";
import { describe, expect, it } from "vitest";

import {
  checkOptions,
  compactQuery,
  declarationOptions,
  hasTerms,
  isTransient,
  pisaKeys,
  retryQuery,
  searchOptions,
  snapshotOptions,
  snapshotsOptions,
} from "../src/index.ts";

const client = new PisaClient({ url: "http://pisa.test", version: "v" });
const result = (kind?: string) => ({
  ok: false,
  time: 1,
  messages: [],
  sorries: [],
  reasons: [],
  declarations: [],
  ...(kind !== undefined && { error: { kind: kind as "timeout", message: "" } }),
});

/** How long a search like `query` stays fresh with `data` cached. */
const staleTime = (query: Query, data?: QueryResult) => {
  const s = searchOptions(client, query).staleTime;
  return typeof s === "function" ? s({ state: { data } } as never) : s;
};

describe("query options", () => {
  it("keeps a check's result unless it may not recur", () => {
    expect(isTransient(result())).toBe(false);
    expect(isTransient(result("lean"))).toBe(false);
    expect(isTransient(result("timeout"))).toBe(true);
    expect(isTransient(result("crash"))).toBe(true);
  });

  it("keys checks on what they send, client defaults included", () => {
    expect(checkOptions(client, { code: "x" }).queryKey).toEqual([
      "pisa",
      "http://pisa.test",
      "check",
      { code: "x", version: "v", timeout: null, detail: [], at: [] },
    ]);
    expect(checkOptions(client, { code: "x", version: "w" }).queryKey).not.toEqual(
      checkOptions(client, { code: "x" }).queryKey,
    );
    expect(checkOptions(client, { code: "x", at: [{ line: 1, column: 0 }] }).queryKey).not.toEqual(
      checkOptions(client, { code: "x" }).queryKey,
    );
  });

  it("drops a query's blank fields, and searches only with a term", () => {
    expect(compactQuery({ description: "  ", name: "x", libraries: [], snapshots: "none", limit: 3 })).toEqual({
      name: "x",
      limit: 3,
    });
    expect(hasTerms({ description: " ", libraries: ["Mathlib"] })).toBe(false);
    expect(hasTerms({ text: "x" })).toBe(true);
    expect(searchOptions(client, { description: "" }).enabled).toBe(false);
    expect(staleTime({ description: "trace" })).toBe(Infinity);
    expect(staleTime({ description: "trace", snapshots: "all" })).toBe(30_000);
  });

  it("searches again for a result that left out the dense lane while it was embedded", () => {
    expect(staleTime({ description: "trace" }, { hits: [], dense_pending: true })).toBe(0);
    expect(staleTime({ description: "trace" }, { hits: [], warnings: ["dense lane unavailable"] })).toBe(Infinity);
  });

  it("keys a search on the version it asks for", () => {
    expect(searchOptions(client, { text: "x" }).queryKey).toEqual(["pisa", "http://pisa.test", "search", "v", { text: "x" }]);
    expect(searchOptions(client, { text: "x" }, { version: "w" }).queryKey).not.toEqual(
      searchOptions(client, { text: "x" }).queryKey,
    );
  });

  it("keys a declaration on the version and the snapshot module it's looked up in", () => {
    const bare = new PisaClient({ url: "http://pisa.test" });
    const key = (version: string | null) => ["pisa", "http://pisa.test", "search", version, "declaration", "x", null];
    expect(declarationOptions(bare, "x").queryKey).toEqual(key(null));
    expect(declarationOptions(client, "x").queryKey).toEqual(key("v"));
    expect(declarationOptions(client, "x", "PisaSnap.s1").queryKey).not.toEqual(declarationOptions(client, "x").queryKey);
  });

  it("keys a snapshot by its id alone, and a listing by its version, under every snapshot query", () => {
    expect(snapshotOptions(client, "w~e").queryKey).toEqual(["pisa", "http://pisa.test", "snapshots", "snapshot", "w~e"]);
    expect(snapshotsOptions(client).queryKey).toEqual(["pisa", "http://pisa.test", "snapshots", "list", "v", {}]);
    expect(snapshotsOptions(client, {}, { version: "w" }).queryKey).not.toEqual(snapshotsOptions(client).queryKey);
    expect(pisaKeys.snapshots(client)).toEqual(snapshotOptions(client, "w~e").queryKey.slice(0, 3));
  });

  it("retries once after a 503, twice after a network error, never after a 429", () => {
    const tries = (e: unknown) => [0, 1, 2].filter((n) => retryQuery(n, e)).length;
    expect(tries(new PisaError("unavailable", "", 503))).toBe(1);
    expect(tries(new PisaError("network", ""))).toBe(2);
    expect(tries(new PisaError("capacity", "", 429))).toBe(0);
    expect(tries(new PisaError("lean", "", 422))).toBe(0);
  });
});
