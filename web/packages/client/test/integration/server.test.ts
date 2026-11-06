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

    const [nat, cheat, ...rest] = await client.checkBatch(["#check Nat", "theorem t : False := sorry"]);
    expect(rest).toEqual([]);
    expect(nat).toMatchObject({ messages: [{ text: "Nat : Type", pos: { line: 1, column: 0 } }] });
    expect(nat).not.toHaveProperty("id");
    expect(cheat).toMatchObject({ reasons: ["axiom: t uses sorryAx"] });

    const code = "import Foo\ntheorem t : True := by\n  trivial";
    expect((await client.check(code)).tactics).toBeUndefined();
    const detailed = await client.check(code, { detail: ["tactics"] });
    expect(detailed.tactics).toEqual([
      { tactic: "trivial", goals_before: ["⊢ goal 2"], goals_after: [], pos: { line: 3, column: 2 }, end_pos: expect.anything() },
    ]);

    const exact = "import Foo\ntheorem t : True := by\n  exact foo";
    const at = await client.check(exact, { detail: ["terms"], at: [{ line: 3, column: 9 }] });
    expect(at.at).toEqual([
      {
        pos: { line: 3, column: 9 },
        tactic: expect.objectContaining({ tactic: "exact foo", goals_before: ["⊢ goal 2"] }),
        term: expect.objectContaining({ term: "foo", expected_type: "goal 2" }),
      },
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
    expect(root?.goals).toEqual(["⊢ goal 1"]);
    expect(root?.pos).toEqual({ line: 1, column: 24 });

    const step = await client.applyTactic(root?.id ?? "", "simp");
    expect(step.complete).toBe(false);
    expect(step.state.pos).toBeUndefined();
    const done = await client.applyTactic(step.state.id, "rfl");
    expect(done).toMatchObject({ complete: true, state: { goals: [] } });
    // The root is still there to branch from.
    const failed = await client.applyTactic(root?.id ?? "", "fail").catch((e: unknown) => e);
    expect(failed).toMatchObject({ kind: "lean", status: 422 });
    const forbidden = await client.applyTactic(root?.id ?? "", "run_tac pure ()").catch((e: unknown) => e);
    expect(isPisaError(forbidden, "policy")).toBe(true);

    expect((await client.checkSession(session.id, "theorem t : 1 = 1 := by rfl")).ok).toBe(true);
    const weaker = await client.checkSession(session.id, "theorem t : 2 = 2 := by rfl");
    expect(weaker.reasons).toEqual(["statement: t has a different type"]);
  });

  it("raises bad and unknown keys", async () => {
    expect(await client.applyTactic("not-a-key", "rfl").catch((e: unknown) => e)).toMatchObject({
      kind: "bad_request",
      status: 400,
    });
    expect(await client.applyTactic("v~abc~0123", "rfl").catch((e: unknown) => e)).toMatchObject({ kind: "not_found" });
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

  it("lets it ask for a job, and read the job's id", async () => {
    const preflight = await fetch(`${url}/check`, {
      method: "OPTIONS",
      headers: {
        origin: "http://app.example",
        "access-control-request-method": "POST",
        "access-control-request-headers": "content-type, prefer",
      },
    });
    expect(preflight.headers.get("access-control-allow-headers")).toContain("prefer");
    const r = await fetch(`${url}/check`, {
      method: "POST",
      headers: { origin: "http://app.example", "content-type": "application/json", prefer: "respond-async" },
      body: JSON.stringify({ code: "#check Nat" }),
    });
    expect(r.status).toBe(202);
    expect(r.headers.get("access-control-expose-headers")).toContain("location");
  });
});

describe("jobs", () => {
  it("answers later as the request would have", async () => {
    const job = await client.submitCheck("#check Nat");
    expect(job).toMatchObject({ kind: "check", status: "queued" });
    expect((await client.waitJob(job)).ok).toBe(true);

    const session = await client.waitJob(await client.submitStartSession("theorem t : 1 = 1 := by sorry"));
    const state = session.states[0]?.id ?? "";
    const applied = await client.submitApplyTactic(state, "rfl");
    expect(applied.kind).toBe("apply_tactic");
    expect((await client.waitJob(applied)).complete).toBe(true);
    const failed = await client.waitJob(await client.submitApplyTactic(state, "fail")).catch((e: unknown) => e);
    expect(isPisaError(failed, "lean")).toBe(true);
    const proved = await client.submitCheckSession(session.id, "theorem t : 1 = 1 := by rfl");
    expect((await client.waitJob(proved)).ok).toBe(true);

    const made = await client.waitJob(await client.submitSnapshot("def jobbed := 1"));
    const verified = await client.waitJob(await client.submitVerify(made.entry ?? ""));
    expect(verified.verified).toBe(true);
    // Done: it stays as it is.
    expect((await client.cancelJob(job)).status).toBe("done");
  });
});
