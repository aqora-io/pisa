import {
  type DocumentGoals,
  type DocumentId,
  type DocumentProbes,
  type LeanDocument,
  type LinePos,
  type PisaClient,
  PisaError,
  type ProbeAt,
  isPisaError,
} from "@aqora/pisa-client";
import {
  type UseMutationResult,
  type UseQueryResult,
  keepPreviousData,
  useMutation,
  useQuery,
} from "@tanstack/react-query";
import { useEffect, useMemo, useReducer, useRef } from "react";

import { type DocumentGoalsInput, type DocumentRef, documentGoalsOptions } from "../options.ts";
import { usePisaClient } from "../provider.tsx";

export interface DocumentState {
  /** Counts opens and closes; answers for an earlier one are dropped. */
  generation: number;
  status: "idle" | "opening" | "open" | "failed";
  /** The document as of the newest revision known. */
  document?: LeanDocument;
  /** The code at `document.revision`. */
  code?: string;
  /** An edit is in flight, or waiting for one. */
  editing: boolean;
  /** Why the last open or edit failed; a failed edit leaves the revision as
   *  it was. */
  error?: PisaError;
}

export type DocumentAction =
  | { type: "open"; generation: number }
  | { type: "opened"; generation: number; document: LeanDocument; code: string }
  | { type: "openFailed"; generation: number; error: PisaError }
  | { type: "edit"; generation: number }
  | { type: "edited"; generation: number; document: LeanDocument; code: string }
  | { type: "editFailed"; generation: number; error: PisaError }
  | { type: "close"; generation: number };

export const initialDocument: DocumentState = { generation: 0, status: "idle", editing: false };

export function documentReducer(s: DocumentState, a: DocumentAction): DocumentState {
  switch (a.type) {
    case "open":
      return { generation: a.generation, status: "opening", editing: false };
    case "close":
      return { generation: a.generation, status: "idle", editing: false };
  }
  if (a.generation !== s.generation) return s;
  switch (a.type) {
    case "opened":
      return { generation: s.generation, status: "open", document: a.document, code: a.code, editing: false };
    case "openFailed":
      return { generation: s.generation, status: "failed", editing: false, error: a.error };
    case "edit":
      return { ...s, editing: true };
    case "edited": {
      const { error: _, ...rest } = s;
      return { ...rest, document: a.document, code: a.code, editing: false };
    }
    case "editFailed":
      return { ...s, editing: false, error: a.error };
  }
}

export interface DocumentOptions {
  version?: string;
  timeout?: number;
}

export interface DocumentHandle extends DocumentState {
  /** Opens `code` as a new document, closing the one there was; call it
   *  from an event, not an effect, which StrictMode would run twice. */
  open(code: string): void;
  /** Makes `code` the document's next revision, or opens it if nothing is
   *  open. Edits go a revision at a time: while one is in flight, only the
   *  newest code waits to follow it. A document that expired is opened
   *  again with the code. */
  edit(code: string): void;
  /** Closes the document and forgets it. */
  close(): void;
}

/** Closes a document, without waiting: one already gone is fine. */
function closeDocument(client: PisaClient, id: DocumentId): void {
  void client.closeDocument(id).catch(() => undefined);
}

/** Code kept open on Lean Beam, edited a revision at a time. When it is
 *  opened again, closed, or unmounted, its requests are aborted and its
 *  document closed. Edits are never aborted for newer ones, as the server
 *  may apply one anyway and the next would name the wrong revision. */
export function useDocument(options: DocumentOptions = {}): DocumentHandle {
  const client = usePisaClient();
  const [state, dispatch] = useReducer(documentReducer, initialDocument);
  const generation = useRef(0);
  /** The document as the server has it, for edits to name its revision. */
  const current = useRef<{ id: DocumentId; revision: number; code: string } | null>(null);
  const opening = useRef(false);
  const sending = useRef(false);
  /** The newest code edited while an open or an edit was in flight. */
  const pending = useRef<string | null>(null);
  const inFlight = useRef(new Set<AbortController>());
  const clientRef = useRef(client);
  useEffect(() => {
    clientRef.current = client;
  }, [client]);
  useEffect(() => {
    const requests = inFlight.current;
    return () => {
      requests.forEach((c) => c.abort());
      if (current.current !== null) closeDocument(clientRef.current, current.current.id);
    };
  }, []);

  const { version, timeout } = options;
  return useMemo(() => {
    const limit = { ...(timeout !== undefined && { timeout }) };
    async function request<T>(go: (signal: AbortSignal) => Promise<T>, ok: (v: T) => void, fail: (e: PisaError) => void) {
      const controller = new AbortController();
      inFlight.current.add(controller);
      try {
        ok(await go(controller.signal));
      } catch (e) {
        if (!isPisaError(e, "aborted")) fail(isPisaError(e) ? e : new PisaError("bad_response", String(e)));
      } finally {
        inFlight.current.delete(controller);
      }
    }
    /** Aborts what is in flight and closes the document there is. */
    const end = () => {
      inFlight.current.forEach((c) => c.abort());
      inFlight.current.clear();
      if (current.current !== null) closeDocument(client, current.current.id);
      current.current = null;
      opening.current = false;
      sending.current = false;
      pending.current = null;
    };
    /** Sends the code edited meanwhile, if any. */
    const flush = (gen: number) => {
      const code = pending.current;
      pending.current = null;
      if (code !== null) send(code, gen);
    };
    const open = (code: string) => {
      end();
      const gen = ++generation.current;
      opening.current = true;
      dispatch({ type: "open", generation: gen });
      void request(
        (signal) => client.openDocument(code, { ...(version !== undefined && { version }), ...limit, signal }),
        (document) => {
          // Opened after all, for an open since given up: close it.
          if (gen !== generation.current) return closeDocument(client, document.id);
          opening.current = false;
          current.current = { id: document.id, revision: document.revision, code };
          dispatch({ type: "opened", generation: gen, document, code });
          flush(gen);
        },
        (error) => {
          opening.current = false;
          dispatch({ type: "openFailed", generation: gen, error });
        },
      );
    };
    const send = (code: string, gen: number) => {
      const doc = current.current;
      if (doc === null || code === doc.code) return;
      sending.current = true;
      dispatch({ type: "edit", generation: gen });
      void request(
        (signal) => client.editDocument(doc.id, doc.revision, code, { ...limit, signal }),
        (document) => {
          if (gen !== generation.current) return;
          sending.current = false;
          current.current = { id: document.id, revision: document.revision, code };
          dispatch({ type: "edited", generation: gen, document, code });
          flush(gen);
        },
        (error) => {
          if (gen !== generation.current) return;
          sending.current = false;
          // Closed or expired: open the newest code as a new document.
          if (error.kind === "not_found") return open(pending.current ?? code);
          dispatch({ type: "editFailed", generation: gen, error });
          flush(gen);
        },
      );
    };
    return {
      ...state,
      open,
      edit(code) {
        if (opening.current || sending.current) {
          pending.current = code;
        } else if (current.current === null) {
          open(code);
        } else {
          send(code, generation.current);
        }
      },
      close() {
        end();
        dispatch({ type: "close", generation: ++generation.current });
      },
    };
  }, [state, client, version, timeout]);
}

/** The goals at `at` in a document, or a session's code (`{ id: session,
 *  revision: 1 }`), or nothing while `doc` is null or `at` empty. A
 *  revision's goals are kept for good. While an edit is in flight, a
 *  request naming the old revision can come back a `conflict`. */
export function useDocumentGoals(
  doc: DocumentRef | null,
  at: LinePos[],
  input: DocumentGoalsInput = {},
): UseQueryResult<DocumentGoals, PisaError> {
  const options = documentGoalsOptions(usePisaClient(), doc ?? { id: "" }, at, input);
  return useQuery({ ...options, enabled: doc !== null && at.length > 0, placeholderData: keepPreviousData });
}

/** Candidate tactics to try at a sorry or a position of a document. */
export interface ProbeInput {
  id: DocumentId;
  at: ProbeAt;
  /** At most 64, each tried on its own. */
  candidates: string[];
  revision?: number;
  /** Seconds for all the candidates together. */
  timeout?: number;
}

/** Tries candidate tactics without changing the document. */
export function useProbeDocument(): UseMutationResult<DocumentProbes, PisaError, ProbeInput> {
  const client = usePisaClient();
  return useMutation({
    mutationFn: ({ id, at, candidates, revision, timeout }) =>
      client.probeDocument(
        id,
        at,
        candidates,
        { ...(revision !== undefined && { revision }) },
        { ...(timeout !== undefined && { timeout }) },
      ),
  });
}
