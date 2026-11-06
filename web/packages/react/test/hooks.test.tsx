import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { initialProof, proofReducer, useDeleteSnapshot, useLiveCheck, useProofSession, useSnapshots } from "../src/index.ts";
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
        session: "s",
        states: [{ state: "k1", goal: "⊢ g", pos: { line: 1, column: 23 }, end_pos: { line: 1, column: 28 } }],
        messages: [],
      };
    }
    const tactic = call.body?.["tactic"];
    if (tactic === "fail") {
      return new Response(JSON.stringify({ error: { kind: "lean", message: "failed" } }), { status: 422 });
    }
    // Lean recovers from an elaboration error: the step succeeds, with the error.
    if (tactic === "exact nope") {
      const error = { severity: "error", pos: { line: 0, column: 0 }, text: "Unknown identifier `nope`" };
      return { state: "k-nope", goals: [], complete: false, messages: [error] };
    }
    // `skip` leaves the state as it was, with the same key.
    if (tactic === "skip") return { state: "k1", goals: ["⊢ g"], complete: false, messages: [] };
    return { state: `k-${String(tactic)}`, goals: [], complete: true, messages: [] };
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
    let nope: number | undefined;
    act(() => {
      skip = result.current.apply(root?.id ?? -1, "skip");
      fail = result.current.apply(root?.id ?? -1, "fail");
      nope = result.current.apply(root?.id ?? -1, "exact nope");
    });
    await waitFor(() => expect(result.current.nodes.every((n) => n.status !== "pending")).toBe(true));
    expect(result.current.node(skip ?? -1)).toMatchObject({ status: "ok", state: "k1", parent: root?.id });
    expect(result.current.node(fail ?? -1)).toMatchObject({ status: "failed", error: { kind: "lean" } });
    expect(result.current.node(nope ?? -1)).toMatchObject({ status: "failed", messages: [{ severity: "error" }] });
    expect(result.current.apply(nope ?? -1, "rfl")).toBeUndefined();

    // Branching from a node with the root's key goes on from that node.
    let trivial: number | undefined;
    act(() => {
      trivial = result.current.apply(skip ?? -1, "trivial");
    });
    await waitFor(() => expect(result.current.node(trivial ?? -1)?.status).toBe("ok"));
    expect(result.current.path(trivial ?? -1)).toEqual(["skip", "trivial"]);
    expect(result.current.children(root?.id ?? -1)).toHaveLength(3);
    expect(result.current.proofText([trivial ?? -1])).toBe(`theorem t : True := by skip\n${" ".repeat(23)}trivial`);
  });

  it("drops what answers after a reset", async () => {
    const slow = deferred<unknown>();
    const { client } = fakeClient((call) => (call.path === "/sessions" ? slow.promise : route(call)));
    const { result } = renderHook(() => useProofSession(), { wrapper: wrapper(client) });
    act(() => result.current.start("theorem t : True := by sorry"));
    act(() => result.current.reset());
    slow.resolve(route({ method: "POST", path: "/sessions", body: undefined }));
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
      call.method === "DELETE" ? { entry: "e", removed: [] } : { entries: [] },
    );
    const { result } = renderHook(() => ({ list: useSnapshots(), remove: useDeleteSnapshot() }), {
      wrapper: wrapper(client),
    });
    await waitFor(() => expect(result.current.list.isSuccess).toBe(true));
    await act(() => result.current.remove.mutateAsync("e"));
    await waitFor(() => expect(calls.filter((c) => c.method === "GET")).toHaveLength(2));
  });
});
