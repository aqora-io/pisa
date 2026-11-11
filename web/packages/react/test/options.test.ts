import { PisaClient, PisaError, type Query, type QueryResult } from "@aqora/pisa-client";
import { describe, expect, it } from "vitest";

import {
  auditOptions,
  checkBatchOptions,
  checkOptions,
  compactQuery,
  declarationOptions,
  dependentsOptions,
  documentGoalsOptions,
  hasTerms,
  isTransient,
  pisaKeys,
  rebuildPlanOptions,
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

  it("keys a batch on its codes and what it sends, and keeps it unless an item may not recur", () => {
    expect(checkBatchOptions(client, { codes: ["a", "b"] }).queryKey).toEqual([
      "pisa",
      "http://pisa.test",
      "check",
      "batch",
      { codes: ["a", "b"], version: "v", timeout: null, detail: [], at: [] },
    ]);
    const s = checkBatchOptions(client, { codes: ["a"] }).staleTime;
    const stale = (data: unknown) => (typeof s === "function" ? s({ state: { data } } as never) : s);
    expect(stale([{ code: "a", result: result() }])).toBe(Infinity);
    expect(stale([{ code: "a", result: result() }, { code: "b", result: result("timeout") }])).toBe(0);
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
    expect(declarationOptions(client, "x", undefined, { version: "w" }).queryKey).toEqual(key("w"));
  });

  it("keys a snapshot by its id alone, and a listing by its version, under every snapshot query", () => {
    expect(snapshotOptions(client, "w~e").queryKey).toEqual(["pisa", "http://pisa.test", "snapshots", "snapshot", "w~e", { detail: [] }]);
    const full = snapshotOptions(client, "w~e", { detail: ["declarations"] }).queryKey;
    expect(full).not.toEqual(snapshotOptions(client, "w~e").queryKey);
    expect(full.slice(0, 5)).toEqual(pisaKeys.snapshot(client, "w~e"));
    expect(snapshotsOptions(client).queryKey).toEqual(["pisa", "http://pisa.test", "snapshots", "list", "v", {}]);
    expect(snapshotsOptions(client, {}, { version: "w" }).queryKey).not.toEqual(snapshotsOptions(client).queryKey);
    expect(pisaKeys.snapshots(client)).toEqual(snapshotOptions(client, "w~e").queryKey.slice(0, 3));
  });

  it("keys what builds on a snapshot, and audits, under every snapshot query", () => {
    const snapshots = pisaKeys.snapshots(client);
    const dependents = dependentsOptions(client, "w~e", { labels: { p: "1" } }).queryKey;
    expect(dependents).toEqual([...snapshots, "dependents", "w~e", { p: "1" }]);
    const byLabels = auditOptions(client, { labels: { p: "1" } }).queryKey;
    expect(byLabels.slice(0, 5)).toEqual([...snapshots, "audit", "v"]);
    // Spec ids carry their version: an audit against them names none.
    const against = auditOptions(client, { against: ["w~s"] }).queryKey;
    expect(against.slice(0, 5)).toEqual([...snapshots, "audit", null]);
    expect(auditOptions(client, { against: ["w~s"], review: true }).queryKey).not.toEqual(against);
  });

  it("keys a rebuild's plan on what it plans, under every snapshot query", () => {
    const key = rebuildPlanOptions(client, { from: "w~a", to: "w~b", labels: { p: "1" } }).queryKey;
    expect(key).toEqual([...pisaKeys.snapshots(client), "rebuild", { from: "w~a", to: "w~b", labels: { p: "1" }, allow_changes: false }]);
  });

  it("keys a document's goals on its revision, whose goals never change", () => {
    const at = [{ line: 2 }];
    const first = documentGoalsOptions(client, { id: "w~d", revision: 1 }, at);
    expect(first.queryKey).toEqual([...pisaKeys.documents(client), "w~d", 1, "goals", { at, mode: "before", timeout: null }]);
    expect(documentGoalsOptions(client, { id: "w~d", revision: 2 }, at).queryKey).not.toEqual(first.queryKey);
    expect(first.staleTime).toBe(Infinity);
    expect(documentGoalsOptions(client, { id: "w~d" }, at).staleTime).toBe(0);
  });

  it("retries once after a 503, twice after a network error, never after a 429", () => {
    const tries = (e: unknown) => [0, 1, 2].filter((n) => retryQuery(n, e)).length;
    expect(tries(new PisaError("unavailable", "", 503))).toBe(1);
    expect(tries(new PisaError("network", ""))).toBe(2);
    expect(tries(new PisaError("capacity", "", 429))).toBe(0);
    expect(tries(new PisaError("lean", "", 422))).toBe(0);
  });
});
