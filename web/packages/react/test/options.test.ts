import { PisaClient, PisaError, type Query, type QueryResult } from "@aqora/pisa-client";
import { describe, expect, it } from "vitest";

import { checkOptions, compactQuery, declOptions, hasTerms, isTransient, retryQuery, searchOptions } from "../src/index.ts";

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
      { code: "x", version: "v", timeout: null, detail: [] },
    ]);
    expect(checkOptions(client, { code: "x", version: "w" }).queryKey).not.toEqual(
      checkOptions(client, { code: "x" }).queryKey,
    );
  });

  it("drops a query's blank fields, and searches only with a term", () => {
    expect(compactQuery({ nl: "  ", name: "x", libraries: [], snapshots: "none", k: 3 })).toEqual({ name: "x", k: 3 });
    expect(hasTerms({ nl: " ", libraries: ["Mathlib"] })).toBe(false);
    expect(searchOptions(client, { nl: "" }).enabled).toBe(false);
    expect(staleTime({ nl: "trace" })).toBe(Infinity);
    expect(staleTime({ nl: "trace", snapshots: "all" })).toBe(30_000);
  });

  it("searches again for a result that left out the dense lane while it was embedded", () => {
    expect(staleTime({ nl: "trace" }, { hits: [], dense_pending: true })).toBe(0);
    expect(staleTime({ nl: "trace" }, { hits: [], warnings: ["dense lane unavailable"] })).toBe(Infinity);
  });

  it("keys a declaration on the snapshot module it's looked up in", () => {
    expect(declOptions(client, "x").queryKey).toEqual(["pisa", "http://pisa.test", "search", "decl", "x", null]);
    expect(declOptions(client, "x", "PisaSnap.s1").queryKey).not.toEqual(declOptions(client, "x").queryKey);
  });

  it("retries once after a 503, twice after a network error, never after a 429", () => {
    const tries = (e: unknown) => [0, 1, 2].filter((n) => retryQuery(n, e)).length;
    expect(tries(new PisaError("unavailable", "", 503))).toBe(1);
    expect(tries(new PisaError("network", ""))).toBe(2);
    expect(tries(new PisaError("capacity", "", 429))).toBe(0);
    expect(tries(new PisaError("lean", "", 422))).toBe(0);
  });
});
