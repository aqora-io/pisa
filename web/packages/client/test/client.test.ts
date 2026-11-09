import { describe, expect, it } from "vitest";

import { PisaClient, isCapacityError, isPisaError, type RebuildItem, type Replaces } from "../src/index.ts";

interface Sent {
  url: string;
  method: string;
  body: unknown;
  headers: Record<string, string>;
}

/** A client whose fetch records each request and answers with `reply`. */
function recording(reply: (sent: Sent) => Response | Promise<Response>, url = "/api/") {
  const sent: Sent[] = [];
  const client = new PisaClient({
    url,
    version: "v1",
    fetch: async (input, init) => {
      const s: Sent = {
        url: String(input),
        method: init?.method ?? "GET",
        body: init?.body === undefined ? undefined : JSON.parse(String(init.body)),
        headers: (init?.headers ?? {}) as Record<string, string>,
      };
      sent.push(s);
      return reply(s);
    },
  });
  return { client, sent };
}

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

describe("PisaClient", () => {
  it("joins paths to a relative base and encodes each segment", async () => {
    const { client, sent } = recording(() => json({ version: "v1", declaration: { name: "x" } }));
    await client.getDeclaration("Real.sqrt/x y");
    await client.getDeclaration("double_eq", { module: "PisaSnap.sa1", version: "v2" });
    await client.getSnapshot("v2~abc");
    expect(sent.map((s) => s.url)).toEqual([
      "/api/declarations/Real.sqrt%2Fx%20y?version=v1",
      "/api/declarations/double_eq?version=v2&module=PisaSnap.sa1",
      "/api/snapshots/v2~abc",
    ]);
  });

  it("sends positions only when there are some", async () => {
    const { client, sent } = recording(() => json({ results: [] }));
    await client.check("x", { detail: ["terms"], at: [{ line: 2, column: 4 }] });
    await client.checkBatch(["x"], { at: [] });
    expect(sent[0]?.body).toEqual({ code: "x", version: "v1", detail: ["terms"], at: [{ line: 2, column: 4 }] });
    expect(sent[1]?.body).toEqual({ items: [{ code: "x" }], version: "v1" });
  });

  it("puts the version in the body or the query, as the server reads it", async () => {
    const { client, sent } = recording(() => json({ snapshots: [], results: [] }));
    await client.check("#check Nat", { detail: ["tactics"], timeout: 5 });
    await client.checkBatch(["x"], { version: "v2" });
    await client.listSnapshots({ labels: { run: "a b", "k.x": "1:2" }, limit: 1, after: "c" });
    await client.applyTactic("v1~h~s", "simp");
    expect(sent[0]?.body).toEqual({ code: "#check Nat", version: "v1", timeout: 5, detail: ["tactics"] });
    expect(sent[0]?.headers["content-type"]).toBe("application/json");
    expect(sent[1]?.body).toEqual({ items: [{ code: "x" }], version: "v2" });
    const list = new URL(sent[2]?.url ?? "", "http://h");
    expect(list.searchParams.getAll("label")).toEqual(["run:a b", "k.x:1:2"]);
    expect([list.searchParams.get("version"), list.searchParams.get("limit"), list.searchParams.get("after")]).toEqual([
      "v1",
      "1",
      "c",
    ]);
    // A state's id carries its version, and so does a snapshot's.
    expect(sent[3]).toMatchObject({ url: "/api/states/v1~h~s/tactic", body: { tactic: "simp" } });
  });

  it("sends no version with a snapshot id, which carries its own", async () => {
    const { client, sent } = recording((s) =>
      s.headers["prefer"] ? json({ id: "j", kind: "verify_snapshot", status: "queued" }, 202) : json({}),
    );
    await client.getSnapshot("v2~e1");
    await client.updateSnapshot("v2~e1", { labels: {} });
    await client.deleteSnapshot("v2~e1");
    await client.verifySnapshot("v2~e1");
    await client.submitVerifySnapshot("v2~e1");
    await client.listDependents("v2~e1");
    expect(sent.map((s) => s.url)).toEqual([
      "/api/snapshots/v2~e1",
      "/api/snapshots/v2~e1",
      "/api/snapshots/v2~e1",
      "/api/snapshots/v2~e1/verify",
      "/api/snapshots/v2~e1/verify",
      "/api/snapshots/v2~e1/dependents",
    ]);
  });

  it("sends what a snapshot replaces", async () => {
    const { client, sent } = recording(() => json({ check: {} }));
    await client.createSnapshot("theorem t : 1 = 1 := rfl", { replaces: { snapshot_id: "v1~e1" }, key: "k" });
    await client.createSnapshot("def f := 2", { replaces: { snapshot_id: "v1~e2", allow: { definitions: ["f"] } } });
    expect(sent.map((s) => s.body)).toEqual([
      { code: "theorem t : 1 = 1 := rfl", version: "v1", key: "k", replaces: { snapshot_id: "v1~e1" } },
      { code: "def f := 2", version: "v1", replaces: { snapshot_id: "v1~e2", allow: { definitions: ["f"] } } },
    ]);
  });

  it("updates labels with a merge patch", async () => {
    const { client, sent } = recording(() => json({ id: "v1~e1", labels: { stage: "done" } }));
    await client.updateSnapshot("v1~e1", { labels: { stage: "done", draft: null } });
    expect(sent[0]).toMatchObject({
      url: "/api/snapshots/v1~e1",
      method: "PATCH",
      body: { labels: { stage: "done", draft: null } },
    });
  });

  it("asks for dependents and rebuild plans", async () => {
    const { client, sent } = recording(() => json({ snapshots: [], ok: true, changes: [], plan: [] }));
    await client.listDependents("v1~e1", { labels: { p: "1" } });
    await client.auditSnapshots({ labels: { p: "1" } });
    await client.rebuildSnapshots("v1~e1", "v1~e2", { p: "1" }, { dryRun: true, allowChanges: true });
    expect(sent[0]?.url).toBe("/api/snapshots/v1~e1/dependents?label=p%3A1");
    expect(sent[1]?.url).toBe("/api/snapshots/audit?version=v1&label=p%3A1");
    expect(sent[2]).toMatchObject({
      url: "/api/snapshots/rebuild",
      body: { from: "v1~e1", to: "v1~e2", labels: { p: "1" }, dry_run: true, allow_changes: true },
    });
  });

  it("audits a run against its spec, in the ids' version", async () => {
    const { client, sent } = recording(() =>
      json({ summary: {}, assumptions: [], not_proved: [], clean: false, heads: ["v2~p", "v2~q"], forks: [{ snapshot_id: "v2~s0", replaced_by: ["v2~p", "v2~q"] }] }),
    );
    const audit = await client.auditSnapshots({ against: ["v2~s0", "v2~s1"] });
    expect(sent[0]?.url).toBe("/api/snapshots/audit?against=v2%7Es0&against=v2%7Es1");
    expect(audit.clean).toBe(false);
    expect(audit.forks?.[0]?.replaced_by).toEqual(["v2~p", "v2~q"]);
    // No ids would be a label audit: it isn't sent.
    await expect(client.auditSnapshots({ against: [] })).rejects.toMatchObject({ name: "PisaError", kind: "bad_request" });
    expect(sent).toHaveLength(1);
  });

  it("reads a plan's links, each to the snapshot its item copies, and the runs it left out", async () => {
    const proof: RebuildItem = {
      level: 1,
      snapshot_id: "v1~p1",
      module: "PisaSnap.sa",
      new_module: "PisaSnap.sb",
      new_header: "import PisaSnap.sb",
      code: "theorem s : 2 = 2 := rfl",
      labels: { p: "1" },
      key: "rebuild:a:b:p1",
      replaces: { snapshot_id: "v1~p1", allow: { definitions: ["f"] } },
    };
    const unlinked = { ...proof, snapshot_id: "v1~q1", key: "rebuild:a:b:q1", replaces: undefined };
    const replaced = [{ snapshot_id: "v1~s0", replaced_by: ["v1~r1"] }];
    const { client } = recording(() => json({ ok: true, changes: [], plan: [proof, unlinked], replaced }));
    const r = await client.rebuildSnapshots("v1~e1", "v1~e2", { p: "1" }, { dryRun: true });
    const links: (Replaces | undefined)[] = r.plan.map((i) => i.replaces);
    expect(links).toEqual([{ snapshot_id: "v1~p1", allow: { definitions: ["f"] } }, undefined]);
    expect(r.replaced?.[0]?.replaced_by).toEqual(["v1~r1"]);
  });

  it("runs a rebuild, at once or as a job", async () => {
    const { client, sent } = recording((s) =>
      s.headers["prefer"] ? json({ id: "j", kind: "rebuild_snapshots", status: "running" }, 202) : json({ ok: true, changes: [], plan: [] }),
    );
    await client.rebuildSnapshots("v1~e1", "v1~e2", { p: "1" }, { retire: true, verify: false, timeout: 30 });
    const job = await client.submitRebuildSnapshots("v1~e1", "v1~e2", { p: "1" });
    expect(sent[0]?.body).toEqual({
      from: "v1~e1",
      to: "v1~e2",
      labels: { p: "1" },
      dry_run: false,
      timeout: 30,
      retire: true,
      verify: false,
    });
    expect(sent[1]?.headers["prefer"]).toBe("respond-async");
    expect(job).toMatchObject({ id: "j", kind: "rebuild_snapshots" });
  });

  it("searches the client's version, or the call's", async () => {
    const { client, sent } = recording(() => json({ version: "v1", results: [{ hits: [] }] }));
    expect(await client.search({ description: "x" })).toEqual({ hits: [] });
    await client.resolve(["x"], { version: "w" });
    expect(sent.map((s) => [s.url, s.body])).toEqual([
      ["/api/search", { queries: [{ description: "x" }], version: "v1" }],
      ["/api/resolve", { names: ["x"], version: "w" }],
    ]);
  });

  it("throws the server's error, or bad_response for one that isn't pisa's", async () => {
    let reply = json({ error: { kind: "capacity", message: "No available REPLs" } }, 429);
    const { client } = recording(() => reply);
    const busy = await client.check("x").catch((e: unknown) => e);
    expect(isCapacityError(busy)).toBe(true);
    expect(busy).toMatchObject({ kind: "capacity", status: 429, message: "No available REPLs" });

    reply = new Response("<html>Bad Gateway</html>", { status: 502, statusText: "Bad Gateway" });
    expect(await client.check("x").catch((e: unknown) => e)).toMatchObject({ kind: "bad_response", status: 502 });
    reply = new Response("not json", { status: 200 });
    expect(await client.check("x").catch((e: unknown) => e)).toMatchObject({ kind: "bad_response", status: 200 });
  });

  it("tells a network failure from an abort", async () => {
    const { client } = recording(() => {
      throw new TypeError("fetch failed");
    });
    const down = await client.check("x").catch((e: unknown) => e);
    expect(isPisaError(down, "network")).toBe(true);

    const abort = new AbortController();
    const slow = recording(
      (s) =>
        new Promise((_, reject) => {
          abort.signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
          void s;
        }),
    );
    const pending = slow.client.check("x", { signal: abort.signal }).catch((e: unknown) => e);
    abort.abort();
    expect(isPisaError(await pending, "aborted")).toBe(true);
  });

  it("returns a draining server's health, and throws a proxy's 503", async () => {
    let reply = json({ status: "draining", workers: 2, search: null }, 503);
    const { client } = recording(() => reply);
    expect(await client.health()).toEqual({ status: "draining", workers: 2, search: null });
    reply = new Response("Service Unavailable", { status: 503 });
    expect(await client.health().catch((e: unknown) => e)).toMatchObject({ kind: "bad_response", status: 503 });
  });

  it("submits a request as a job, and waits for its answer", async () => {
    const job = { id: "a".repeat(32), kind: "check", status: "queued" };
    let looks = 0;
    const { client, sent } = recording((s) => {
      if (s.method === "POST") return json(job, 202);
      if (s.method === "DELETE") return json({ ...job, status: "cancelled" });
      looks += 1;
      return looks < 3 ? json({ ...job, status: "running" }, 202) : json({ ok: true });
    });
    const submitted = await client.submitCheck("#check Nat", { detail: ["tactics"] });
    expect(submitted).toEqual(job);
    expect(sent[0]?.headers["prefer"]).toBe("respond-async");
    expect(sent[0]?.body).toEqual({ code: "#check Nat", version: "v1", detail: ["tactics"] });

    expect(await client.pollJob(submitted)).toEqual({ done: false, job: { ...job, status: "running" } });
    expect((await client.waitJob(submitted)).ok).toBe(true);
    expect(sent.slice(1).map((s) => s.url)).toEqual([
      `/api/jobs/${job.id}`,
      `/api/jobs/${job.id}?wait=60`,
      `/api/jobs/${job.id}?wait=60`,
    ]);
    expect((await client.cancelJob(submitted)).status).toBe("cancelled");
  });

  it("throws a job's error as its request would have, and a server that won't queue it", async () => {
    const job = { id: "b".repeat(32), kind: "apply_tactic", status: "queued" };
    const { client } = recording((s) =>
      s.method === "POST" ? json(job, 202) : json({ error: { kind: "lean", message: "no" } }, 422),
    );
    const submitted = await client.submitApplyTactic("v~h~s", "fail");
    const thrown = await client.waitJob(submitted).catch((e: unknown) => e);
    expect(isPisaError(thrown, "lean")).toBe(true);

    const { client: old } = recording(() => json({ ok: true }));
    const refused = await old.submitCheck("#check Nat").catch((e: unknown) => e);
    expect(isPisaError(refused, "bad_response")).toBe(true);
  });
});

