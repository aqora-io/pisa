import {
  type CheckResult,
  type Message,
  PisaError,
  type SessionId,
  type State,
  type StateId,
  isPisaError,
  proofText,
} from "@aqora/pisa-client";
import { useEffect, useMemo, useReducer, useRef } from "react";

import { usePisaClient } from "../provider.tsx";

/** A state of a proof session. Each `sorry` is a root, and each tactic applied
 *  is a node under the node it was applied to. Nodes have ids of their own:
 *  the same tactic applied twice to a state gives the same state, with the
 *  same id. */
export interface ProofNode {
  id: number;
  /** The node the tactic was applied to; `null` for a root. */
  parent: number | null;
  /** `null` for a root. */
  tactic: string | null;
  status: "pending" | "ok" | "failed";
  /** Once known. */
  state?: StateId;
  goals: string[];
  /** No goals are left for this `sorry`. */
  complete: boolean;
  messages: Message[];
  /** Why the tactic failed: `lean` or `policy` when Lean rejected it,
   *  `not_found` when the state it was applied to has expired. */
  error?: PisaError;
  /** For a root, the `sorry` it stands for. */
  sorry?: State;
}

export interface ProofState {
  /** Counts starts and resets; answers for an earlier one are dropped. */
  generation: number;
  status: "idle" | "starting" | "ready" | "failed";
  code?: string;
  session?: SessionId;
  messages: Message[];
  error?: PisaError;
  nodes: Readonly<Record<number, ProofNode>>;
  /** Node ids, oldest first. */
  order: number[];
  roots: number[];
  /** The session check of the last `check`. */
  verdict: {
    status: "idle" | "checking" | "done" | "failed";
    code?: string;
    result?: CheckResult;
    error?: PisaError;
  };
}

export type ProofAction =
  | { type: "start"; generation: number; code: string }
  | { type: "reset"; generation: number }
  | { type: "started"; generation: number; session: SessionId; states: State[]; messages: Message[]; ids: number[] }
  | { type: "startFailed"; generation: number; error: PisaError }
  | { type: "apply"; generation: number; id: number; parent: number; tactic: string }
  | {
      type: "applied";
      generation: number;
      id: number;
      state: StateId;
      goals: string[];
      complete: boolean;
      messages: Message[];
    }
  | { type: "applyFailed"; generation: number; id: number; error: PisaError }
  | { type: "check"; generation: number; code: string }
  | { type: "checked"; generation: number; result: CheckResult }
  | { type: "checkFailed"; generation: number; error: PisaError };

export const initialProof: ProofState = {
  generation: 0,
  status: "idle",
  messages: [],
  nodes: {},
  order: [],
  roots: [],
  verdict: { status: "idle" },
};

function update(s: ProofState, id: number, change: (n: ProofNode) => ProofNode): ProofState {
  const node = s.nodes[id];
  return node === undefined ? s : { ...s, nodes: { ...s.nodes, [id]: change(node) } };
}

export function proofReducer(s: ProofState, a: ProofAction): ProofState {
  if (a.type === "start") return { ...initialProof, generation: a.generation, status: "starting", code: a.code };
  if (a.type === "reset") return { ...initialProof, generation: a.generation };
  if (a.generation !== s.generation) return s;
  switch (a.type) {
    case "started": {
      const nodes: Record<number, ProofNode> = {};
      a.states.forEach((sorry, i) => {
        const id = a.ids[i] as number;
        nodes[id] = {
          id,
          parent: null,
          tactic: null,
          status: "ok",
          state: sorry.id,
          goals: sorry.goals,
          complete: false,
          messages: [],
          sorry,
        };
      });
      return { ...s, status: "ready", session: a.session, messages: a.messages, nodes, order: [...a.ids], roots: [...a.ids] };
    }
    case "startFailed":
      return { ...s, status: "failed", error: a.error };
    case "apply": {
      if (s.nodes[a.parent] === undefined) return s;
      const node: ProofNode = {
        id: a.id,
        parent: a.parent,
        tactic: a.tactic,
        status: "pending",
        goals: [],
        complete: false,
        messages: [],
      };
      return { ...s, nodes: { ...s.nodes, [a.id]: node }, order: [...s.order, a.id] };
    }
    case "applied":
      return update(s, a.id, (n) => ({
        ...n,
        status: "ok",
        state: a.state,
        goals: a.goals,
        complete: a.complete,
        messages: a.messages,
      }));
    case "applyFailed":
      return update(s, a.id, (n) => ({ ...n, status: "failed", error: a.error }));
    case "check":
      return { ...s, verdict: { status: "checking", code: a.code } };
    case "checked":
      return { ...s, verdict: { ...s.verdict, status: "done", result: a.result } };
    case "checkFailed":
      return { ...s, verdict: { ...s.verdict, status: "failed", error: a.error } };
  }
}

export interface ProofSessionOptions {
  version?: string;
  timeout?: number;
}

export interface ProofSession extends Omit<ProofState, "nodes" | "order" | "roots"> {
  /** Every node, oldest first. */
  nodes: ProofNode[];
  roots: ProofNode[];
  node(id: number): ProofNode | undefined;
  children(id: number): ProofNode[];
  /** The tactics from the node's root to it. */
  path(id: number): string[];
  /** The session's code with each of `leaves`' sorries replaced by the
   *  tactics that lead to it, for `check`. */
  proofText(leaves: number[]): string;
  /** Starts a new session on code with sorries; call it from an event, not
   *  an effect, which StrictMode would run twice. */
  start(code: string): void;
  /** Applies a tactic to a node that is `ok`, branching from it; returns the
   *  new node's id. Nodes run in parallel. */
  apply(id: number, tactic: string): number | undefined;
  /** Checks completed code against the session. */
  check(code: string): void;
  reset(): void;
}

function asPisaError(e: unknown): PisaError {
  return isPisaError(e) ? e : new PisaError("bad_response", String(e));
}

/** A proof session as a tree of states, kept in the component. Its requests
 *  are aborted when it is reset, started again, or unmounted. */
export function useProofSession(options: ProofSessionOptions = {}): ProofSession {
  const client = usePisaClient();
  const [state, dispatch] = useReducer(proofReducer, initialProof);
  const generation = useRef(0);
  const nextId = useRef(0);
  const latest = useRef(state);
  const inFlight = useRef(new Set<AbortController>());
  useEffect(() => {
    latest.current = state;
  }, [state]);
  useEffect(() => {
    const requests = inFlight.current;
    return () => requests.forEach((c) => c.abort());
  }, []);

  const { version, timeout } = options;
  return useMemo(() => {
    const run = { ...(version !== undefined && { version }), ...(timeout !== undefined && { timeout }) };
    const abortAll = () => {
      inFlight.current.forEach((c) => c.abort());
      inFlight.current.clear();
    };
    async function request<T>(go: (signal: AbortSignal) => Promise<T>, ok: (v: T) => void, fail: (e: PisaError) => void) {
      const controller = new AbortController();
      inFlight.current.add(controller);
      try {
        ok(await go(controller.signal));
      } catch (e) {
        if (!isPisaError(e, "aborted")) fail(asPisaError(e));
      } finally {
        inFlight.current.delete(controller);
      }
    }
    const node = (id: number) => state.nodes[id];
    const path = (id: number): string[] => {
      const tactics: string[] = [];
      for (let n = node(id); n !== undefined && n.tactic !== null; n = n.parent === null ? undefined : node(n.parent)) {
        tactics.push(n.tactic);
      }
      return tactics.reverse();
    };
    const rootOf = (id: number): ProofNode | undefined => {
      let n = node(id);
      while (n !== undefined && n.parent !== null) n = node(n.parent);
      return n;
    };
    return {
      ...state,
      nodes: state.order.map((id) => state.nodes[id] as ProofNode),
      roots: state.roots.map((id) => state.nodes[id] as ProofNode),
      node,
      children: (id) => state.order.map((c) => state.nodes[c] as ProofNode).filter((n) => n.parent === id),
      path,
      proofText: (leaves) =>
        proofText(
          state.code ?? "",
          leaves.flatMap((leaf) => {
            const sorry = rootOf(leaf)?.sorry;
            return sorry === undefined ? [] : [{ sorry, tactics: path(leaf) }];
          }),
        ),
      start(code) {
        abortAll();
        const gen = ++generation.current;
        dispatch({ type: "start", generation: gen, code });
        void request(
          (signal) => client.startSession(code, { ...run, signal }),
          (r) =>
            dispatch({
              type: "started",
              generation: gen,
              session: r.id,
              states: r.states,
              messages: r.messages,
              ids: r.states.map(() => nextId.current++),
            }),
          (error) => dispatch({ type: "startFailed", generation: gen, error }),
        );
      },
      apply(parent, tactic) {
        const from = latest.current.nodes[parent];
        if (from?.status !== "ok" || from.state === undefined) return undefined;
        const state = from.state;
        const gen = generation.current;
        const id = nextId.current++;
        dispatch({ type: "apply", generation: gen, id, parent, tactic });
        void request(
          (signal) => client.applyTactic(state, tactic, { ...(timeout !== undefined && { timeout }), signal }),
          (r) =>
            dispatch({
              type: "applied",
              generation: gen,
              id,
              state: r.state.id,
              goals: r.state.goals,
              complete: r.complete,
              messages: r.messages,
            }),
          (error) => dispatch({ type: "applyFailed", generation: gen, id, error }),
        );
        return id;
      },
      check(code) {
        const session = latest.current.session;
        if (session === undefined) return;
        const gen = generation.current;
        dispatch({ type: "check", generation: gen, code });
        void request(
          (signal) => client.checkSession(session, code, { ...(timeout !== undefined && { timeout }), signal }),
          (result) => dispatch({ type: "checked", generation: gen, result }),
          (error) => dispatch({ type: "checkFailed", generation: gen, error }),
        );
      },
      reset() {
        abortAll();
        dispatch({ type: "reset", generation: ++generation.current });
      },
    };
  }, [state, client, version, timeout]);
}
