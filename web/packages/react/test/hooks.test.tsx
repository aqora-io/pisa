import { PisaError } from "@aqora/pisa-client";
import { QueryClient } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  documentReducer,
  initialDocument,
  initialProof,
  proofReducer,
  snapshotOptions,
  useAcceptRun,
  useAcceptances,
  useAudit,
  useCheckBatch,
  useDeleteSnapshot,
  useDependents,
  useDocument,
  useDocumentGoals,
  useExportRun,
  useLiveCheck,
  useLongJob,
  useProbeDocument,
  useProofSession,
  useRebuildPlan,
  useRebuildSnapshots,
  useSnapshot,
  useSnapshots,
  useUpdateSnapshot,
  useVerifySnapshot,
} from "../src/index.ts";
import { type Call, deferred, fakeClient, wrapper } from "./fake.tsx";

const checked = (code: string) => ({ ok: true, time: 0, messages: [], sorries: [], reasons: [], declarations: [], code });

describe("useLiveCheck", () => {
  it("keeps one check in flight, and then sends the newest code", async () => {
    const pending: Array<{ call: Call; done: ReturnType<typeof deferred<unknown>> }> = [];
    const { client, calls } = fakeClient((call) => {
      const done = deferred<unknown>();
      pending.push({ call, done });
      return done.promise;
    });
    const { result, rerender } = renderHook(({ code }) => useLiveCheck(code, { delayMs: 5 }), {
      initialProps: { code: "a" },
      wrapper: wrapper(client),
    });
    await waitFor(() => expect(calls).toHaveLength(1));
    rerender({ code: "b" });
    rerender({ code: "c" });
    await new Promise((r) => setTimeout(r, 30));
    expect(calls).toHaveLength(1);
    expect(result.current.stale).toBe(true);

    act(() => pending[0]?.done.resolve(checked("a")));
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls[1]?.body?.["code"]).toBe("c");
    act(() => pending[1]?.done.resolve(checked("c")));
    await waitFor(() => expect(result.current.data?.code).toBe("c"));
    expect(result.current.stale).toBe(false);
  });
});

describe("useProofSession", () => {
  const route = (call: Call): unknown => {
    if (call.path === "/sessions") {
      return {
        id: "s",
        states: [{ id: "k1", goals: ["⊢ g"], pos: { line: 1, column: 23 }, end_pos: { line: 1, column: 28 } }],
        messages: [],
      };
    }
    const tactic = call.body?.["tactic"];
    if (tactic === "fail") {
      return new Response(JSON.stringify({ error: { kind: "lean", message: "failed" } }), { status: 422 });
    }
    // `skip` leaves the state as it was, with the same key.
    if (tactic === "skip") return { state: { id: "k1", goals: ["⊢ g"] }, complete: false, messages: [] };
    return { state: { id: `k-${String(tactic)}`, goals: [] }, complete: true, messages: [] };
  };

  it("branches, keeps nodes that share a state apart, and marks failed tactics", async () => {
    const { client } = fakeClient(route);
    const { result } = renderHook(() => useProofSession(), { wrapper: wrapper(client) });
    act(() => result.current.start("theorem t : True := by sorry"));
    await waitFor(() => expect(result.current.status).toBe("ready"));
    const root = result.current.roots[0];
    expect(root?.goals).toEqual(["⊢ g"]);

    let skip: number | undefined;
    let fail: number | undefined;
    act(() => {
      skip = result.current.apply(root?.id ?? -1, "skip");
      fail = result.current.apply(root?.id ?? -1, "fail");
    });
    await waitFor(() => expect(result.current.nodes.every((n) => n.status !== "pending")).toBe(true));
    expect(result.current.node(skip ?? -1)).toMatchObject({ status: "ok", state: "k1", parent: root?.id });
    expect(result.current.node(fail ?? -1)).toMatchObject({ status: "failed", error: { kind: "lean" } });
    expect(result.current.apply(fail ?? -1, "rfl")).toBeUndefined();

    // Branching from a node with the root's key goes on from that node.
    let trivial: number | undefined;
    act(() => {
      trivial = result.current.apply(skip ?? -1, "trivial");
    });
    await waitFor(() => expect(result.current.node(trivial ?? -1)?.status).toBe("ok"));
    expect(result.current.path(trivial ?? -1)).toEqual(["skip", "trivial"]);
    expect(result.current.children(root?.id ?? -1)).toHaveLength(2);
    expect(result.current.proofText([trivial ?? -1])).toBe(`theorem t : True := by skip\n${" ".repeat(23)}trivial`);
  });

  it("closes its session's document on starting again, on reset, and on unmount", async () => {
    let started = 0;
    const { client, calls } = fakeClient((call) => {
      if (call.path === "/sessions") return { id: `v~s${++started}`, states: [], messages: [] };
      // A document that is gone already: nothing to show for it.
      if (call.method === "DELETE") return new Response(JSON.stringify({ error: { kind: "not_found", message: "gone" } }), { status: 404 });
      return route(call);
    });
    const deletes = () => calls.filter((c) => c.method === "DELETE").map((c) => c.path);
    const { result, unmount } = renderHook(() => useProofSession(), { wrapper: wrapper(client) });
    act(() => result.current.start("a"));
    await waitFor(() => expect(result.current.session).toBe("v~s1"));
    act(() => result.current.start("b"));
    await waitFor(() => expect(result.current.session).toBe("v~s2"));
    expect(deletes()).toEqual(["/documents/v~s1"]);
    act(() => result.current.reset());
    expect(deletes()).toEqual(["/documents/v~s1", "/documents/v~s2"]);
    expect(result.current.error).toBeUndefined();
    act(() => result.current.start("c"));
    await waitFor(() => expect(result.current.session).toBe("v~s3"));
    unmount();
    expect(deletes()).toEqual(["/documents/v~s1", "/documents/v~s2", "/documents/v~s3"]);
  });

  it("checks a proof with its declarations in full when asked", async () => {
    const { client, calls } = fakeClient((call) => (call.path.endsWith("/check") ? { ok: true } : route(call)));
    const { result } = renderHook(() => useProofSession(), { wrapper: wrapper(client) });
    act(() => result.current.start("theorem t : True := by sorry"));
    await waitFor(() => expect(result.current.status).toBe("ready"));
    act(() => result.current.check("theorem t : True := by trivial", { detail: ["declarations"] }));
    await waitFor(() => expect(calls.some((c) => c.path === "/sessions/s/check")).toBe(true));
    expect(calls.find((c) => c.path === "/sessions/s/check")?.body?.["detail"]).toEqual(["declarations"]);
  });

  it("drops what answers after a reset", async () => {
    const slow = deferred<unknown>();
    const { client } = fakeClient((call) => (call.path === "/sessions" ? slow.promise : route(call)));
    const { result } = renderHook(() => useProofSession(), { wrapper: wrapper(client) });
    act(() => result.current.start("theorem t : True := by sorry"));
    act(() => result.current.reset());
    slow.resolve(route({ method: "POST", path: "/sessions", body: undefined, headers: {} }));
    await new Promise((r) => setTimeout(r, 20));
    expect(result.current.status).toBe("idle");
    expect(result.current.nodes).toEqual([]);
  });

  it("ignores answers for an earlier session in the reducer", () => {
    const started = proofReducer(initialProof, { type: "start", generation: 1, code: "x" });
    const late = proofReducer(started, { type: "startFailed", generation: 0, error: undefined as never });
    expect(late).toBe(started);
  });
});

describe("snapshot mutations", () => {
  it("refetch the listings", async () => {
    const { client, calls } = fakeClient((call) =>
      call.method === "DELETE" ? { removed: [], deleted: ["e"] } : { snapshots: [] },
    );
    const { result } = renderHook(() => ({ list: useSnapshots(), remove: useDeleteSnapshot() }), {
      wrapper: wrapper(client),
    });
    await waitFor(() => expect(result.current.list.isSuccess).toBe(true));
    await act(() => result.current.remove.mutateAsync("e"));
    await waitFor(() => expect(calls.filter((c) => c.method === "GET")).toHaveLength(2));
  });
});

describe("snapshots", () => {
  const detail = (id: string) => ({ id, labels: {}, check: { declarations: [] } });

  it("asks for a snapshot's declarations in full", async () => {
    const { client, calls } = fakeClient((call) => detail(call.path));
    const { result } = renderHook(() => useSnapshot("v~e", { detail: ["declarations"] }), { wrapper: wrapper(client) });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(calls[0]?.path).toBe("/snapshots/v~e?detail=declarations");
  });

  it("updates the labels of a snapshot cached in full and summarised", async () => {
    const { client } = fakeClient(() => ({ id: "v~e", labels: { p: "1" } }));
    const queryClient = new QueryClient();
    for (const options of [{}, { detail: ["declarations" as const] }]) {
      queryClient.setQueryData(snapshotOptions(client, "v~e", options).queryKey, detail("v~e"));
    }
    const { result } = renderHook(() => useUpdateSnapshot(), { wrapper: wrapper(client, queryClient) });
    await act(() => result.current.mutateAsync({ id: "v~e", labels: { p: "1" } }));
    for (const options of [{}, { detail: ["declarations" as const] }]) {
      expect(queryClient.getQueryData(snapshotOptions(client, "v~e", options).queryKey)).toMatchObject({ labels: { p: "1" } });
    }
  });

  it("deletes a snapshot with what replaces it, and forgets each one deleted", async () => {
    const { client, calls } = fakeClient(() => ({ removed: [], deleted: ["v~e", "v~f"] }));
    const queryClient = new QueryClient();
    const keys = [snapshotOptions(client, "v~e").queryKey, snapshotOptions(client, "v~f", { detail: ["declarations"] }).queryKey];
    for (const key of keys) queryClient.setQueryData(key, detail("x"));
    const { result } = renderHook(() => useDeleteSnapshot(), { wrapper: wrapper(client, queryClient) });
    await act(() => result.current.mutateAsync({ id: "v~e", withReplacers: true }));
    expect(calls[0]).toMatchObject({ method: "DELETE", path: "/snapshots/v~e?with_replacers=true" });
    for (const key of keys) expect(queryClient.getQueryData(key)).toBeUndefined();
  });

  it("verifies with a timeout", async () => {
    const { client, calls } = fakeClient(() => ({ snapshot: detail("v~e"), checked: [] }));
    const { result } = renderHook(() => useVerifySnapshot(), { wrapper: wrapper(client) });
    await act(() => result.current.mutateAsync({ id: "v~e", timeout: 30 }));
    expect(calls[0]).toMatchObject({ method: "POST", path: "/snapshots/v~e/verify?timeout=30" });
  });
});

describe("batches", () => {
  const ok = { ok: true, time: 0, messages: [], sorries: [], reasons: [], declarations: [] };

  it("checks a batch, and gives each code its result", async () => {
    const { client, calls } = fakeClient(() => ({ results: [ok, { ...ok, ok: false }] }));
    const { result } = renderHook(() => useCheckBatch({ codes: ["a", "b"], detail: ["declarations"] }), {
      wrapper: wrapper(client),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.map((c) => [c.code, c.result.ok])).toEqual([
      ["a", true],
      ["b", false],
    ]);
    expect(calls[0]).toMatchObject({
      method: "POST",
      path: "/check/batch",
      body: { items: [{ code: "a" }, { code: "b" }], detail: ["declarations"] },
    });
  });

  it("checks nothing until there is a batch", async () => {
    const { client, calls } = fakeClient(() => ({ results: [] }));
    renderHook(() => useCheckBatch(null), { wrapper: wrapper(client) });
    await new Promise((r) => setTimeout(r, 20));
    expect(calls).toHaveLength(0);
  });
});

describe("runs", () => {
  const audit = { summary: {}, assumptions: [], not_proved: [] };

  it("lists what builds on a snapshot", async () => {
    const { client, calls } = fakeClient(() => ({ snapshots: [] }));
    const { result } = renderHook(() => useDependents("v~e", { labels: { p: "1" } }), { wrapper: wrapper(client) });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(calls[0]?.path).toBe("/snapshots/v~e/dependents?label=p%3A1");
  });

  it("audits a run against its spec, with its review, and nothing without spec snapshots", async () => {
    const { client, calls } = fakeClient(() => audit);
    renderHook(() => useAudit({ against: [] }), { wrapper: wrapper(client) });
    const { result } = renderHook(() => useAudit({ against: ["v~s"], review: true }), { wrapper: wrapper(client) });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(calls.map((c) => c.path)).toEqual(["/snapshots/audit?against=v%7Es&review=true"]);
  });

  it("audits again once a snapshot is deleted", async () => {
    const { client, calls } = fakeClient((call) => (call.method === "DELETE" ? { removed: [], deleted: ["v~e"] } : audit));
    const { result } = renderHook(() => ({ audit: useAudit({ labels: { p: "1" } }), remove: useDeleteSnapshot() }), {
      wrapper: wrapper(client),
    });
    await waitFor(() => expect(result.current.audit.isSuccess).toBe(true));
    await act(() => result.current.remove.mutateAsync("v~e"));
    await waitFor(() => expect(calls.filter((c) => c.path.startsWith("/snapshots/audit"))).toHaveLength(2));
  });
});

/** What `GET /jobs/{id}` answers for a done `job`: its request's status and
 *  answer. */
const envelope = (job: { id: string; kind: string }, body: unknown, http_status = 200) => ({
  ...job,
  status: "done",
  http_status,
  body,
});

describe("long jobs", () => {
  const job = { id: "a".repeat(32), kind: "check" as const, status: "queued" as const };
  const answer = { ok: true, time: 0, messages: [], sorries: [], reasons: [], declarations: [] };
  const respond = (body: unknown, status: number) => new Response(JSON.stringify(body), { status });
  const submitCheck = (client: Parameters<Parameters<typeof useLongJob>[0]>[0], code: string, options: { signal: AbortSignal }) =>
    client.submitCheck(code, options);

  it("reports a job's progress and its answer", async () => {
    const running = deferred<Response>();
    let polls = 0;
    const { client, calls } = fakeClient((call) => {
      if (call.method === "POST") return respond(job, 202);
      polls += 1;
      return polls === 1 ? running.promise : envelope(job, answer);
    });
    const { result } = renderHook(() => useLongJob(submitCheck), { wrapper: wrapper(client) });
    act(() => result.current.mutate("#check Nat"));
    await waitFor(() => expect(result.current.job?.status).toBe("queued"));
    running.resolve(respond({ ...job, status: "running" }, 202));
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.ok).toBe(true);
    expect(result.current.job?.status).toBe("done");
    expect(calls[0]?.headers["prefer"]).toBe("respond-async");
    expect(calls.slice(1).map((c) => c.path)).toEqual([`/jobs/${job.id}?wait=10`, `/jobs/${job.id}?wait=10`]);
  });

  it("cancels a job no worker has started", async () => {
    const { client, calls } = fakeClient((call) => {
      if (call.method === "POST") return respond(job, 202);
      if (call.method === "DELETE") return { ...job, status: "cancelled" };
      return new Promise<never>(() => undefined);
    });
    const { result } = renderHook(() => useLongJob(submitCheck), { wrapper: wrapper(client) });
    act(() => result.current.mutate("#check Nat"));
    await waitFor(() => expect(result.current.job?.status).toBe("queued"));
    let status: string | undefined;
    await act(async () => {
      status = await result.current.cancel();
    });
    expect(status).toBe("cancelled");
    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error?.kind).toBe("aborted");
    expect(result.current.job?.status).toBe("cancelled");
    expect(calls.find((c) => c.method === "DELETE")?.path).toBe(`/jobs/${job.id}`);
  });

  it("waits on for a job a worker has started", async () => {
    const done = deferred<unknown>();
    const { client } = fakeClient((call) => {
      if (call.method === "POST") return respond(job, 202);
      if (call.method === "DELETE") return { ...job, status: "running" };
      return done.promise;
    });
    const { result } = renderHook(() => useLongJob(submitCheck), { wrapper: wrapper(client) });
    act(() => result.current.mutate("#check Nat"));
    await waitFor(() => expect(result.current.job?.status).toBe("queued"));
    await act(async () => {
      await result.current.cancel();
    });
    expect(result.current.job?.status).toBe("running");
    done.resolve(envelope(job, answer));
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
  });

  it("cancels a job whose 202 hadn't arrived", async () => {
    const queued = deferred<Response>();
    const { client, calls } = fakeClient((call) => {
      if (call.method === "POST") return queued.promise;
      if (call.method === "DELETE") return { ...job, status: "cancelled" };
      return new Promise<never>(() => undefined);
    });
    const { result } = renderHook(() => useLongJob(submitCheck), { wrapper: wrapper(client) });
    act(() => result.current.mutate("#check Nat"));
    await waitFor(() => expect(calls).toHaveLength(1));
    await act(async () => {
      await result.current.cancel();
    });
    queued.resolve(respond(job, 202));
    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(result.current.error?.kind).toBe("aborted");
    expect(calls.some((c) => c.method === "DELETE")).toBe(true);
  });

  it("forgets a job on reset, and stops waiting for it", async () => {
    let signal: AbortSignal | undefined;
    const { client } = fakeClient((call) => {
      if (call.method === "POST") return respond(job, 202);
      signal = call.signal;
      return new Promise<never>(() => undefined);
    });
    const { result } = renderHook(() => useLongJob(submitCheck), { wrapper: wrapper(client) });
    act(() => result.current.mutate("#check Nat"));
    await waitFor(() => expect(result.current.job?.status).toBe("queued"));
    act(() => result.current.reset());
    await waitFor(() => expect(result.current.isIdle).toBe(true));
    expect(result.current.job).toBeUndefined();
    expect(signal?.aborted).toBe(true);
  });
});

describe("rebuilds", () => {
  const job = { id: "b".repeat(32), kind: "rebuild_snapshots" as const, status: "queued" as const };
  const plan = { ok: true, changes: [], plan: [] };
  const respond = (body: unknown, status: number) => new Response(JSON.stringify(body), { status });

  it("plans a rebuild at once, as a dry run", async () => {
    const { client, calls } = fakeClient(() => plan);
    const { result } = renderHook(() => useRebuildPlan({ from: "v~a", to: "v~b", labels: { p: "1" } }), {
      wrapper: wrapper(client),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(calls[0]).toMatchObject({ method: "POST", path: "/snapshots/rebuild", body: { dry_run: true } });
    expect(calls[0]?.headers["prefer"]).toBeUndefined();
  });

  it("runs a rebuild as a job, and refetches snapshots once it is done or has failed", async () => {
    let answered = false;
    const { client, calls } = fakeClient((call) => {
      if (call.path.startsWith("/snapshots?") || call.path === "/snapshots") return { snapshots: [] };
      if (call.method === "POST") return respond(job, 202);
      if (answered) return envelope(job, { error: { kind: "unavailable", message: "lost" } }, 503);
      answered = true;
      return envelope(job, plan);
    });
    const { result } = renderHook(() => ({ list: useSnapshots(), run: useRebuildSnapshots() }), { wrapper: wrapper(client) });
    await waitFor(() => expect(result.current.list.isSuccess).toBe(true));
    const lists = () => calls.filter((c) => c.method === "GET" && c.path.startsWith("/snapshots")).length;
    const input = { from: "v~a", to: "v~b", labels: { p: "1" }, retire: true };
    await act(() => result.current.run.mutateAsync(input));
    expect(calls.find((c) => c.method === "POST")).toMatchObject({ body: { retire: true, dry_run: false } });
    await waitFor(() => expect(lists()).toBe(2));
    await act(() => result.current.run.mutateAsync(input).catch(() => undefined));
    await waitFor(() => expect(lists()).toBe(3));
  });
});

describe("accepting a run", () => {
  it("lists a spec's acceptances", async () => {
    const { client, calls } = fakeClient(() => ({ acceptances: [{ id: "v~a", accepted: true, stale: true }] }));
    const { result } = renderHook(() => useAcceptances("v~s"), { wrapper: wrapper(client) });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(result.current.data?.acceptances[0]?.stale).toBe(true);
    expect(calls[0]?.path).toBe("/snapshots/accept?spec=v%7Es");
    const idle = renderHook(() => useAcceptances(null), { wrapper: wrapper(client) });
    expect(idle.result.current.fetchStatus).toBe("idle");
  });

  it("accepts a run as a job", async () => {
    const job = { id: "c".repeat(32), kind: "accept_run" as const, status: "queued" as const };
    const { client, calls } = fakeClient((call) =>
      call.method === "POST" ? new Response(JSON.stringify(job), { status: 202 }) : envelope(job, { accepted: true, pairs: [] }),
    );
    const { result } = renderHook(() => useAcceptRun(), { wrapper: wrapper(client) });
    await act(() => result.current.mutateAsync({ against: ["v~s"], paranoid: false, timeout: 900 }));
    expect(result.current.data?.accepted).toBe(true);
    expect(calls[0]).toMatchObject({
      path: "/snapshots/accept",
      body: { against: ["v~s"], paranoid: false, timeout: 900 },
    });
    expect(calls[0]?.headers["prefer"]).toBe("respond-async");
  });

  it("exports a run as a tarball", async () => {
    const { client, calls } = fakeClient(
      () => new Response(new Uint8Array([0x1f, 0x8b]), { status: 200, headers: { "content-type": "application/gzip" } }),
    );
    const { result } = renderHook(() => useExportRun(), { wrapper: wrapper(client) });
    let run: Blob | undefined;
    await act(async () => {
      run = await result.current.mutateAsync({ against: ["v~s"], allow: ["f"] });
    });
    expect(run?.size).toBe(2);
    expect(calls[0]?.path).toBe("/snapshots/export?against=v%7Es&allow=f");
  });
});

describe("documents", () => {
  const timing = { queue: 0, load: 0, execution: 0, tracing: 0, verification: 0, total: 0 };
  const doc = (revision: number, id = "v~d") => ({ id, revision, messages: [], sorries: [], timing });
  const refused = (kind: string, status: number) =>
    new Response(JSON.stringify({ error: { kind, message: kind } }), { status });

  it("drops answers to an earlier open, and keeps the revision when an edit fails", () => {
    let s = documentReducer(initialDocument, { type: "open", generation: 1 });
    s = documentReducer(s, { type: "opened", generation: 0, document: doc(1), code: "x" });
    expect(s.status).toBe("opening");
    s = documentReducer(s, { type: "opened", generation: 1, document: doc(1), code: "a" });
    s = documentReducer(s, { type: "edit", generation: 1 });
    expect(s.editing).toBe(true);
    s = documentReducer(s, { type: "editFailed", generation: 1, error: new PisaError("policy", "no") });
    expect([s.status, s.document?.revision, s.code, s.editing, s.error?.kind]).toEqual(["open", 1, "a", false, "policy"]);
  });

  it("edits a revision at a time, with the newest code, and closes on unmount", async () => {
    const first = deferred<unknown>();
    let patches = 0;
    const { client, calls } = fakeClient((call) => {
      if (call.method === "POST") return doc(1);
      if (call.method === "DELETE") return new Response(null, { status: 204 });
      patches += 1;
      return patches === 1 ? first.promise : doc(3);
    });
    const { result, unmount } = renderHook(() => useDocument(), { wrapper: wrapper(client) });
    act(() => result.current.open("a"));
    await waitFor(() => expect(result.current.document?.revision).toBe(1));
    act(() => {
      result.current.edit("b");
      result.current.edit("c");
      result.current.edit("d");
    });
    first.resolve(doc(2));
    await waitFor(() => expect(result.current.document?.revision).toBe(3));
    expect(calls.filter((c) => c.method === "PATCH").map((c) => c.body)).toEqual([
      { code: "b", revision: 1 },
      { code: "d", revision: 2 },
    ]);
    expect([result.current.code, result.current.editing]).toEqual(["d", false]);
    unmount();
    expect(calls.at(-1)).toMatchObject({ method: "DELETE", path: "/documents/v~d" });
  });

  it("opens the code again when its document has expired", async () => {
    let opened = 0;
    const { client, calls } = fakeClient((call) => {
      if (call.method === "POST") return doc(1, `v~d${++opened}`);
      return refused("not_found", 404);
    });
    const { result } = renderHook(() => useDocument(), { wrapper: wrapper(client) });
    act(() => result.current.open("a"));
    await waitFor(() => expect(result.current.document?.id).toBe("v~d1"));
    act(() => result.current.edit("b"));
    await waitFor(() => expect(result.current.document?.id).toBe("v~d2"));
    expect(calls.filter((c) => c.method === "POST").map((c) => c.body?.["code"])).toEqual(["a", "b"]);
    expect(result.current.code).toBe("b");
  });

  it("keeps the revision when an edit is refused, and edits it next", async () => {
    let patches = 0;
    const { client, calls } = fakeClient((call) => {
      if (call.method === "POST") return doc(1);
      patches += 1;
      return patches === 1 ? refused("policy", 422) : doc(2);
    });
    const { result } = renderHook(() => useDocument(), { wrapper: wrapper(client) });
    act(() => result.current.open("a"));
    await waitFor(() => expect(result.current.document?.revision).toBe(1));
    act(() => result.current.edit("#eval 1"));
    await waitFor(() => expect(result.current.error?.kind).toBe("policy"));
    expect(result.current.document?.revision).toBe(1);
    act(() => result.current.edit("c"));
    await waitFor(() => expect(result.current.document?.revision).toBe(2));
    expect(calls.filter((c) => c.method === "PATCH").map((c) => c.body)).toEqual([
      { code: "#eval 1", revision: 1 },
      { code: "c", revision: 1 },
    ]);
    expect(result.current.error).toBeUndefined();
  });

  it("asks a revision's goals, and probes a sorry", async () => {
    const { client, calls } = fakeClient((call) =>
      call.path.endsWith("/goals")
        ? { revision: 1, at: [{ pos: { line: 2, column: 2 }, goals: ["⊢ p"] }] }
        : { revision: 1, pos: { line: 2, column: 2 }, results: [] },
    );
    const { result } = renderHook(
      () => ({ goals: useDocumentGoals({ id: "v~d", revision: 1 }, [{ line: 2 }]), probe: useProbeDocument() }),
      { wrapper: wrapper(client) },
    );
    await waitFor(() => expect(result.current.goals.isSuccess).toBe(true));
    expect(result.current.goals.data?.at[0]?.goals).toEqual(["⊢ p"]);
    await act(() => result.current.probe.mutateAsync({ id: "v~d", at: { sorry: 0 }, candidates: ["rfl"] }));
    expect(calls.map((c) => [c.path, c.body])).toEqual([
      ["/documents/v~d/goals", { at: [{ line: 2 }], revision: 1 }],
      ["/documents/v~d/probes", { at: { sorry: 0 }, candidates: ["rfl"] }],
    ]);
  });
});
