import { describe, expect, it } from "vitest";

import { PisaClient, isCapacityError, isPisaError } from "../src/index.ts";

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
    const { client, sent } = recording(() => json({ decl: { name: "x" } }));
    await client.decl("Real.sqrt/x y");
    await client.decl("double_eq", { module: "PisaSnap.sa1" });
    await client.getSnapshot("abc");
    expect(sent.map((s) => s.url)).toEqual([
      "/api/v1/decl/Real.sqrt%2Fx%20y",
      "/api/v1/decl/double_eq?module=PisaSnap.sa1",
      "/api/snapshots/abc?version=v1",
    ]);
  });

  it("puts the version in the body or the query, as the server reads it", async () => {
    const { client, sent } = recording(() => json({ entries: [], results: [] }));
    await client.check("#check Nat", { detail: ["tactics"], timeout: 5 });
    await client.checkBatch([{ id: "a", code: "x" }], { version: "v2" });
    await client.listSnapshots({ labels: { run: "a b", "k.x": "1:2" }, limit: 1, after: "c" });
    await client.tactic("v1~h~s", "simp");
    expect(sent[0]?.body).toEqual({ code: "#check Nat", version: "v1", timeout: 5, detail: ["tactics"] });
    expect(sent[0]?.headers["content-type"]).toBe("application/json");
    expect(sent[1]?.body).toEqual({ items: [{ id: "a", code: "x" }], version: "v2" });
    const list = new URL(sent[2]?.url ?? "", "http://h");
    expect(list.searchParams.getAll("label")).toEqual(["run:a b", "k.x:1:2"]);
    expect([list.searchParams.get("version"), list.searchParams.get("limit"), list.searchParams.get("after")]).toEqual([
      "v1",
      "1",
      "c",
    ]);
    // A state's key carries its version.
    expect(sent[3]).toMatchObject({ url: "/api/states/v1~h~s/tactic", body: { tactic: "simp" } });
  });

  it("sends what a snapshot replaces", async () => {
    const { client, sent } = recording(() => json({ version: "v1", dirty: false, verified: false, result: {} }));
    await client.createSnapshot("theorem t : 1 = 1 := rfl", { replaces: "e1", key: "k" });
    await client.createSnapshot("def f := 2", { replaces: { entry: "e2", allow: "definitions" } });
    expect(sent.map((s) => s.body)).toEqual([
      { code: "theorem t : 1 = 1 := rfl", version: "v1", key: "k", replaces: "e1" },
      { code: "def f := 2", version: "v1", replaces: { entry: "e2", allow: "definitions" } },
    ]);
  });

  it("pins search to its environment", async () => {
    const { client, sent } = recording(() => json({ env_id: "e", results: [{ hits: [] }] }));
    expect(await client.search({ nl: "x" }, { envId: "e" })).toEqual({ hits: [] });
    expect(sent[0]?.body).toEqual({ queries: [{ nl: "x" }], env_id: "e" });
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
    const job = { job: "a".repeat(32), kind: "check", status: "queued" };
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

    expect(await client.pollJob(submitted)).toEqual({ done: false, status: { ...job, status: "running" } });
    expect((await client.waitJob(submitted)).ok).toBe(true);
    expect(sent.slice(1).map((s) => s.url)).toEqual([
      `/api/jobs/${job.job}`,
      `/api/jobs/${job.job}?wait=60`,
      `/api/jobs/${job.job}?wait=60`,
    ]);
    expect((await client.cancelJob(submitted)).status).toBe("cancelled");
  });

  it("throws a job's error as its request would have, and a server that won't queue it", async () => {
    const job = { job: "b".repeat(32), kind: "tactic", status: "queued" };
    const { client } = recording((s) =>
      s.method === "POST" ? json(job, 202) : json({ error: { kind: "lean", message: "no" } }, 422),
    );
    const submitted = await client.submitTactic("v~h~s", "fail");
    const thrown = await client.waitJob(submitted).catch((e: unknown) => e);
    expect(isPisaError(thrown, "lean")).toBe(true);

    const { client: old } = recording(() => json({ ok: true }));
    const refused = await old.submitCheck("#check Nat").catch((e: unknown) => e);
    expect(isPisaError(refused, "bad_response")).toBe(true);
  });
});

