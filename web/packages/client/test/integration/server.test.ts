// The client against a real server and a worker running fake-repl. Serde
// ignores fields it doesn't know, so each request field is checked by its
// effect: a misspelled one would pass silently otherwise.

import { describe, expect, inject, it } from "vitest";

import { PisaClient, isPisaError } from "../../src/index.ts";

const url = inject("pisaUrl");
const client = new PisaClient({ url, timeout: 5 });

describe("checks", () => {
  it("returns messages, reasons and, on request, tactics", async () => {
    const ok = await client.check("import Foo\ntheorem t : 1 = 1 := rfl");
    expect(ok.ok).toBe(true);
    expect(ok.declarations[0]?.name).toBe("t");

    const [nat, cheat] = await client.checkBatch([
      { id: "a", code: "#check Nat" },
      { id: "b", code: "theorem t : False := sorry" },
    ]);
    expect(nat).toMatchObject({ id: "a", messages: [{ data: "Nat : Type", pos: { line: 1, column: 0 } }] });
    expect(cheat).toMatchObject({ id: "b", reasons: ["axiom: t uses sorryAx"] });

    const code = "import Foo\ntheorem t : True := by\n  trivial";
    expect((await client.check(code)).tactics).toBeUndefined();
    const detailed = await client.check(code, { detail: ["tactics"] });
    expect(detailed.tactics).toEqual([
      { tactic: "trivial", goals_before: ["⊢ goal 2"], goals_after: [], pos: { line: 3, column: 2 }, end_pos: expect.anything() },
    ]);
  });

  it("sends the timeout and the version", async () => {
    const slow = await client.check("SLEEP 3000", { timeout: 1 });
    expect(slow.ok).toBe(false);
    expect(slow.error?.kind).toBe("timeout");
    // No worker serves "nope": the server waits its second, then gives up.
    const lost = await client.check("#check Nat", { version: "nope" }).catch((e: unknown) => e);
    expect(lost).toMatchObject({ kind: "capacity", status: 429 });
    expect(await client.check("#check Nat", { version: "v" })).toMatchObject({ ok: true });
  });
});

describe("sessions", () => {
  it("branches from any state, and checks the proof against the session", async () => {
    const session = await client.startSession("theorem t : 1 = 1 := by sorry");
    const root = session.states[0];
    expect(root?.goal).toBe("⊢ goal 1");
    expect(root?.pos).toEqual({ line: 1, column: 24 });

    const step = await client.tactic(root?.state ?? "", "simp");
    expect(step.complete).toBe(false);
    const done = await client.tactic(step.state, "rfl");
    expect(done).toMatchObject({ complete: true, goals: [] });
    // The root is still there to branch from.
    const failed = await client.tactic(root?.state ?? "", "fail").catch((e: unknown) => e);
    expect(failed).toMatchObject({ kind: "lean", status: 422 });
    const forbidden = await client.tactic(root?.state ?? "", "run_tac pure ()").catch((e: unknown) => e);
    expect(isPisaError(forbidden, "policy")).toBe(true);

    expect((await client.checkSession(session.session, "theorem t : 1 = 1 := by rfl")).ok).toBe(true);
    const weaker = await client.checkSession(session.session, "theorem t : 2 = 2 := by rfl");
    expect(weaker.reasons).toEqual(["statement: t has a different type"]);
  });

  it("raises bad and unknown keys", async () => {
    expect(await client.tactic("not-a-key", "rfl").catch((e: unknown) => e)).toMatchObject({
      kind: "bad_request",
      status: 400,
    });
    expect(await client.tactic("v~abc~0123", "rfl").catch((e: unknown) => e)).toMatchObject({ kind: "not_found" });
  });
});

describe("snapshots", () => {
  it("keeps code that later code imports, with labels and keys", async () => {
    const base = await client.createSnapshot("import Foo\ntheorem base : 1 = 1 := rfl", { labels: { suite: "web" } });
    expect(base).toMatchObject({ dirty: false, verified: false, labels: { suite: "web" }, version: "v" });
    expect(base.header).toBe(`import Foo\nimport ${base.snapshot}`);
    expect((await client.check(`${base.header}\nneeds base`)).ok).toBe(true);
    const entry = base.entry ?? "";

    const info = await client.getSnapshot(entry);
    expect(info).toMatchObject({ entry, snapshot: base.snapshot, imports: [], labels: { suite: "web" } });

    const verified = await client.verifySnapshot(entry);
    expect(verified).toMatchObject({ verified: true, checked: [base.snapshot] });
    expect(verified.failure).toBeUndefined();

    const code = "import Foo\ntheorem keyed : 1 = 1 := rfl";
    const keyed = await client.createSnapshot(code, { key: "web-1" });
    expect((await client.createSnapshot(code, { key: "web-1" })).entry).toBe(keyed.entry);

    const rejected = await client.createSnapshot("theorem t : 1 = 1 := error_here");
    expect(rejected.entry).toBeUndefined();
    expect(rejected.result.ok).toBe(false);

    const relabelled = await client.setSnapshotLabels(entry, { set: { run: "a b" }, remove: ["suite"] });
    expect(relabelled.labels).toEqual({ run: "a b" });
    const page = await client.listSnapshots({ labels: { run: "a b" }, limit: 1 });
    expect(page.entries.map((e) => e.entry)).toEqual([entry]);
    expect(page.next).toBeDefined();
    expect((await client.listSnapshots({ labels: { run: "a b" }, limit: 1, after: page.next ?? "" })).entries).toEqual([]);

    expect((await client.deleteSnapshot(entry)).removed).toEqual([base.snapshot]);
    expect(await client.getSnapshot(entry).catch((e: unknown) => e)).toMatchObject({ kind: "not_found" });
    expect(await client.getSnapshot(base.snapshot ?? "").catch((e: unknown) => e)).toMatchObject({
      kind: "bad_request",
    });
  });
});

describe("the server", () => {
  it("is healthy, with its worker, and doesn't search", async () => {
    expect(await client.health()).toEqual({ status: "ok", workers: 1, search: null });
    const search = await client.search({ nl: "x" }).catch((e: unknown) => e);
    expect(search).toMatchObject({ kind: "not_found", status: 404 });
  });

  it("lets the configured browser origin call it", async () => {
    const preflight = await fetch(`${url}/check`, {
      method: "OPTIONS",
      headers: {
        origin: "http://app.example",
        "access-control-request-method": "POST",
        "access-control-request-headers": "content-type",
      },
    });
    expect(preflight.headers.get("access-control-allow-origin")).toBe("http://app.example");
  });
});
