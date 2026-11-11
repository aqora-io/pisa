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
    expect(root?.goals).toEqual(["⊢ True"]);
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
    // Not re-checked, so that `verifySnapshot` has it to do.
    const created = await client.createSnapshot("import Foo\ntheorem base : 1 = 1 := rfl", {
      labels: { suite: "web" },
      verify: false,
    });
    const base = created.snapshot;
    if (base === undefined) throw new Error(`not kept: ${JSON.stringify(created)}`);
    expect(base).toMatchObject({ status: "proved", verified: false, labels: { suite: "web" }, version: "v" });
    expect(base.id.startsWith("v~")).toBe(true);
    expect(base.header).toBe(`import Foo\nimport ${base.module}`);
    expect((await client.check(`${base.header}\nneeds base`)).ok).toBe(true);

    const detail = await client.getSnapshot(base.id);
    expect(detail).toMatchObject({ ...base, imports: [], check: { ok: true } });

    const verified = await client.verifySnapshot(base.id);
    expect(verified).toMatchObject({ snapshot: { id: base.id, verified: true }, checked: [base.module] });
    expect(verified.failure).toBeUndefined();

    const code = "import Foo\ntheorem keyed : 1 = 1 := rfl";
    const keyed = await client.createSnapshot(code, { key: "web-1" });
    expect((await client.createSnapshot(code, { key: "web-1" })).snapshot?.id).toBe(keyed.snapshot?.id);

    const rejected = await client.createSnapshot("theorem t : 1 = 1 := error_here");
    expect(rejected.snapshot).toBeUndefined();
    expect(rejected.check.ok).toBe(false);

    const updated = await client.updateSnapshot(base.id, { labels: { run: "a b", suite: null } });
    expect(updated.labels).toEqual({ run: "a b" });
    const page = await client.listSnapshots({ labels: { run: "a b" }, limit: 1 });
    expect(page.snapshots.map((s) => s.id)).toEqual([base.id]);
    expect(page.next).toBeDefined();
    expect((await client.listSnapshots({ labels: { run: "a b" }, limit: 1, after: page.next ?? "" })).snapshots).toEqual([]);

    expect((await client.deleteSnapshot(base.id)).removed).toEqual([base.module]);
    expect(await client.getSnapshot(base.id).catch((e: unknown) => e)).toMatchObject({ kind: "not_found" });
    expect(await client.getSnapshot(base.module).catch((e: unknown) => e)).toMatchObject({
      kind: "bad_request",
    });
  });
});

describe("documents", () => {
  it("opens, asks, edits and closes a document", async () => {
    const docs = new PisaClient({ url, version: "v", timeout: 10 });
    const code = "import Foo\n\ntheorem t : 𝓝 = 𝓝 := by\n  sorry\n";
    const d = await docs.openDocument(code);
    expect([d.revision, d.sorries.length]).toEqual([1, 1]);
    const goals = await docs.documentGoals(d.id, [{ line: 4 }]);
    expect(goals.at[0]?.goals).toEqual(["⊢ sorry"]);
    const probes = await docs.probeDocument(d.id, { sorry: 0 }, ["rfl", "omega"]);
    expect([probes.results[0]?.closed, probes.results[1]?.ok]).toEqual([true, false]);

    const edited = await docs.editDocument(d.id, 1, code.replace("sorry", "rfl"));
    expect([edited.revision, edited.sorries]).toEqual([2, []]);
    const stale = await docs.documentGoals(d.id, [{ line: 4 }], { revision: 1 }).catch((e: unknown) => e);
    expect(isPisaError(stale, "conflict")).toBe(true);

    await docs.closeDocument(d.id);
    const closed = await docs.documentGoals(d.id, [{ line: 4 }]).catch((e: unknown) => e);
    expect(isPisaError(closed, "not_found")).toBe(true);
  });
});

describe("runs", () => {
  it("reviews, exports and accepts a run against its spec", async () => {
    const runs = new PisaClient({ url, version: "v", timeout: 30 });
    const made = await runs.createSnapshot("import Foo\ndef ts_cost : Nat := sorry\ntheorem ts_c : ts_cost = ts_cost := sorry", {
      holes: ["ts_cost"],
    });
    const spec = made.snapshot?.id ?? "";
    const head = await runs.createSnapshot("import Foo\ndef ts_cost : Nat := 3\ntheorem ts_c : ts_cost = ts_cost := rfl", {
      replaces: { snapshot_id: spec },
    });

    const audit = await runs.auditSnapshots({ against: [spec], review: true });
    const cost = audit.review?.[0]?.declarations.find((d) => d.name === "ts_cost");
    expect([cost?.hole, cost?.value]).toEqual([true, "3"]);

    const tarball = new Uint8Array(await runs.exportRun([spec]));
    expect([tarball[0], tarball[1]]).toEqual([0x1f, 0x8b]);

    const accepted = await runs.acceptRun([spec]);
    expect(accepted).toMatchObject({ accepted: true, clean: true, heads: [head.snapshot?.id] });
    expect((await runs.getAcceptance(accepted.id)).pairs).toEqual(accepted.pairs);
    const job = await runs.submitAcceptRun([spec], { paranoid: false });
    expect(job.kind).toBe("accept_run");
    expect((await runs.waitJob(job)).accepted).toBe(true);
  });
});

describe("amendments and refutations", () => {
  it("amends a spec, carries its run over, and refutes a target", async () => {
    const runs = new PisaClient({ url, version: "v", timeout: 30 });
    const spec = (
      await runs.createSnapshot(
        "import Foo\ndef ts_fee : Nat := sorry\ntheorem ts_f : ts_fee = ts_fee := sorry\ntheorem ts_w : 1 = 2 := sorry",
        { holes: ["ts_fee"] },
      )
    ).snapshot;
    if (spec === undefined) throw new Error("no spec");
    const head = await runs.createSnapshot(
      "import Foo\ndef ts_fee : Nat := 1\ntheorem ts_f : ts_fee = ts_fee := rfl\ntheorem ts_w : 1 = 2 := sorry",
      { replaces: { snapshot_id: spec.id } },
    );
    const refuted = await runs.createSnapshot(`${spec.header}\ntheorem ts_r : ¬ (1 = 2) := by decide`, {
      refutes: { snapshot_id: spec.id, target: "ts_w" },
    });
    expect(refuted.refutation?.theorem).toBe("ts_r");
    expect((await runs.auditSnapshots({ against: [spec.id] })).refutations).toEqual([refuted.refutation]);

    const amended = await runs.createSnapshot(
      "import Foo\ndef ts_fee : Nat := sorry\ntheorem ts_f : ts_fee = ts_fee := sorry",
      { holes: ["ts_fee"], amends: { snapshot_id: spec.id, reason: "ts_w is false" } },
    );
    expect(amended.amendment?.changes[0]?.name).toBe("ts_w");
    const to = amended.snapshot?.id ?? "";
    const plan = await runs.resubmitSnapshots(spec.id, to, { dryRun: true });
    expect([plan.ok, plan.items[0]?.snapshot_id]).toEqual([true, head.snapshot?.id]);
    const job = await runs.submitResubmitSnapshots(spec.id, to);
    expect(job.kind).toBe("resubmit_snapshots");
    const done = await runs.waitJob(job);
    expect(done.ok).toBe(true);
    expect(done.items[0]?.new_snapshot_id).toBeDefined();
  });
});

describe("the server", () => {
  it("is healthy, with its worker, and doesn't search", async () => {
    expect(await client.health()).toEqual({ status: "ok", workers: 1, search: null });
    const search = await client.search({ description: "x" }).catch((e: unknown) => e);
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

    const made = await client.waitJob(await client.submitCreateSnapshot("def jobbed := 1"));
    const verified = await client.waitJob(await client.submitVerifySnapshot(made.snapshot?.id ?? ""));
    expect(verified.snapshot.verified).toBe(true);
    // Done: it stays as it is.
    expect((await client.cancelJob(job)).status).toBe("done");
  });
});
