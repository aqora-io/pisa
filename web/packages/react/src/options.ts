// Query options for every read, for `useQuery`, `useQueries` or
// prefetching. Keys start with `["pisa", url]` and end
// with what the request sends, so that a different version never reuses
// another's results.

import type {
  AuditParams,
  AuditSnapshotsResponse,
  CheckOptions,
  CheckResult,
  DeclarationDetail,
  Detail,
  DocumentGoals,
  DocumentId,
  GoalsMode,
  Health,
  Labels,
  LinePos,
  ListDependentsResponse,
  ListSnapshotsParams,
  ListSnapshotsResponse,
  ModuleName,
  PisaClient,
  PisaError,
  Query,
  QueryResult,
  RebuildSnapshotsResponse,
  Resolution,
  SnapshotDetail,
  SnapshotId,
} from "@aqora/pisa-client";
import type { InfiniteData, QueryKey, UseInfiniteQueryOptions, UseQueryOptions } from "@tanstack/react-query";

import { retryQuery } from "./retry.ts";

export type PisaQueryOptions<T> = UseQueryOptions<T, PisaError, T, QueryKey>;

export type PisaInfiniteOptions<T, P> = UseInfiniteQueryOptions<T, PisaError, InfiniteData<T, P>, QueryKey, P>;

/** A check's result, with the code it is about: an editor draws its
 *  diagnostics only while its text is that code. */
export interface Checked {
  code: string;
  result: CheckResult;
}

export interface CheckInput extends Omit<CheckOptions, "signal"> {
  code: string;
}

/** In-band failures that a later try may not have. */
const TRANSIENT: ReadonlySet<string> = new Set(["timeout", "out_of_memory", "crash", "internal", "capacity", "unavailable"]);

/** Whether a check's result could differ if it ran again. */
export function isTransient(result: CheckResult | undefined): boolean {
  return result?.error !== undefined && TRANSIENT.has(result.error.kind);
}

export const pisaKeys = {
  all: (c: PisaClient): QueryKey => ["pisa", c.url],
  health: (c: PisaClient): QueryKey => ["pisa", c.url, "health"],
  checks: (c: PisaClient): QueryKey => ["pisa", c.url, "check"],
  /** Every snapshot query, which a change to any snapshot may affect: a
   *  module's `verified` and `status` are shared by its snapshots. */
  snapshots: (c: PisaClient): QueryKey => ["pisa", c.url, "snapshots"],
  /** One snapshot's queries, summarised and in full. */
  snapshot: (c: PisaClient, id: SnapshotId): QueryKey => ["pisa", c.url, "snapshots", "snapshot", id],
  search: (c: PisaClient): QueryKey => ["pisa", c.url, "search"],
  /** Queries about open documents' code, by id and revision. */
  documents: (c: PisaClient): QueryKey => ["pisa", c.url, "documents"],
};

export function healthOptions(client: PisaClient): PisaQueryOptions<Health> {
  return {
    queryKey: pisaKeys.health(client),
    queryFn: ({ signal }) => client.health({ signal }),
    refetchInterval: 5000,
    retry: false,
  };
}

export function checkOptions(client: PisaClient, input: CheckInput): PisaQueryOptions<Checked> {
  const request = {
    code: input.code,
    version: input.version ?? client.version ?? null,
    timeout: input.timeout ?? client.timeout ?? null,
    detail: input.detail ?? [],
    at: input.at ?? [],
  };
  return {
    queryKey: [...pisaKeys.checks(client), request] as QueryKey,
    queryFn: async ({ signal }) => {
      const { code, ...options } = input;
      return { code, result: await client.check(code, { ...options, signal }) };
    },
    staleTime: (q) => (isTransient(q.state.data?.result) ? 0 : Infinity),
    retry: retryQuery,
  };
}

export interface CheckBatchInput extends Omit<CheckOptions, "signal"> {
  /** At most 128; `detail` and `at` apply to each. */
  codes: string[];
}

/** A batch of checks, each code with its result, in order: kept unless an
 *  item failed in a way that may not recur. */
export function checkBatchOptions(client: PisaClient, input: CheckBatchInput): PisaQueryOptions<Checked[]> {
  const request = {
    codes: input.codes,
    version: input.version ?? client.version ?? null,
    timeout: input.timeout ?? client.timeout ?? null,
    detail: input.detail ?? [],
    at: input.at ?? [],
  };
  return {
    queryKey: [...pisaKeys.checks(client), "batch", request] as QueryKey,
    queryFn: async ({ signal }) => {
      const { codes, ...options } = input;
      const results = await client.checkBatch(codes, { ...options, signal });
      return codes.flatMap((code, i) => {
        const result = results[i];
        return result === undefined ? [] : [{ code, result }];
      });
    },
    staleTime: (q) => (q.state.data?.some((c) => isTransient(c.result)) === true ? 0 : Infinity),
    retry: retryQuery,
  };
}

/** `query` without its blank fields, which the server ignores anyway. */
export function compactQuery(query: Query): Query {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(query)) {
    if (v === undefined || v === null) continue;
    if (typeof v === "string" && v.trim() === "") continue;
    if (Array.isArray(v) && v.length === 0) continue;
    if (k === "snapshots" && v === "none") continue;
    out[k] = v;
  }
  return out as Query;
}

const TERMS = ["text", "description", "latex", "pattern", "goal", "name"] as const;

/** Whether `query` asks for anything. */
export function hasTerms(query: Query): boolean {
  return TERMS.some((t) => (query[t] ?? "").trim() !== "");
}

export function searchOptions(client: PisaClient, query: Query, input: VersionInput = {}): PisaQueryOptions<QueryResult> {
  const compact = compactQuery(query);
  return {
    queryKey: [...pisaKeys.search(client), input.version ?? client.version ?? null, compact] as QueryKey,
    queryFn: ({ signal }) => client.search(compact, { ...input, signal }),
    // Snapshots keep being indexed; the libraries don't change. A query still
    // being embedded gets the dense lane if asked again.
    staleTime: (q) => (q.state.data?.dense_pending === true ? 0 : compact.snapshots === undefined ? Infinity : 30_000),
    enabled: hasTerms(compact),
    retry: retryQuery,
  };
}

export function resolveOptions(client: PisaClient, names: string[], input: VersionInput = {}): PisaQueryOptions<Resolution[]> {
  return {
    queryKey: [...pisaKeys.search(client), input.version ?? client.version ?? null, "resolve", names] as QueryKey,
    queryFn: ({ signal }) => client.resolve(names, { ...input, signal }),
    staleTime: Infinity,
    enabled: names.length > 0,
    retry: retryQuery,
  };
}

/** Declaration `name`: the one in snapshot module `module` if it has it, else
 *  the bundle's. */
export function declarationOptions(
  client: PisaClient,
  name: string,
  module?: ModuleName,
  input: VersionInput = {},
): PisaQueryOptions<DeclarationDetail> {
  const version = input.version ?? client.version ?? null;
  return {
    queryKey: [...pisaKeys.search(client), version, "declaration", name, module ?? null] as QueryKey,
    queryFn: ({ signal }) =>
      client.getDeclaration(name, { ...input, ...(module !== undefined && { module }), signal }),
    staleTime: Infinity,
    retry: retryQuery,
  };
}

export interface VersionInput {
  version?: string;
}

/** What a snapshot query asks for: its check's declarations summarised, or
 *  with `detail: ["declarations"]` in full. */
export interface SnapshotInput {
  detail?: Detail[];
}

/** One snapshot, keyed by its id, which carries its version, and by what
 *  it asks for. */
export function snapshotOptions(
  client: PisaClient,
  id: SnapshotId,
  input: SnapshotInput = {},
): PisaQueryOptions<SnapshotDetail> {
  // The server reads only `declarations`.
  const detail: Detail[] = input.detail?.includes("declarations") ? ["declarations"] : [];
  return {
    queryKey: [...pisaKeys.snapshot(client, id), { detail }] as QueryKey,
    queryFn: ({ signal }) => client.getSnapshot(id, { ...(detail.length > 0 && { detail }), signal }),
    retry: retryQuery,
  };
}

/** The snapshots of every module that builds on snapshot `id`'s, parents
 *  first, with every label in `params.labels`. */
export function dependentsOptions(
  client: PisaClient,
  id: SnapshotId,
  params: { labels?: Labels } = {},
): PisaQueryOptions<ListDependentsResponse> {
  return {
    queryKey: [...pisaKeys.snapshots(client), "dependents", id, params.labels ?? {}] as QueryKey,
    queryFn: ({ signal }) => client.listDependents(id, params, { signal }),
    retry: retryQuery,
  };
}

/** An audit of a project by its labels, in a version, or of a run against
 *  its spec snapshots, whose ids carry theirs. */
export function auditOptions(
  client: PisaClient,
  params: AuditParams,
  input: VersionInput = {},
): PisaQueryOptions<AuditSnapshotsResponse> {
  const version = params.against ? null : (input.version ?? client.version ?? null);
  const asked = {
    labels: params.labels ?? {},
    against: params.against ?? [],
    allow: params.allow ?? [],
    review: params.review ?? false,
  };
  return {
    queryKey: [...pisaKeys.snapshots(client), "audit", version, asked] as QueryKey,
    queryFn: ({ signal }) => client.auditSnapshots(params, { ...input, signal }),
    retry: retryQuery,
  };
}

/** A rebuild to plan: the items that would be created, without creating
 *  them. */
export interface RebuildPlanInput {
  from: SnapshotId;
  to: SnapshotId;
  labels: Labels;
  allowChanges?: boolean;
}

/** A rebuild's plan, a dry run the server answers at once. */
export function rebuildPlanOptions(client: PisaClient, input: RebuildPlanInput): PisaQueryOptions<RebuildSnapshotsResponse> {
  const { from, to, labels, allowChanges = false } = input;
  return {
    queryKey: [...pisaKeys.snapshots(client), "rebuild", { from, to, labels, allow_changes: allowChanges }] as QueryKey,
    queryFn: ({ signal }) =>
      client.rebuildSnapshots(from, to, labels, { dryRun: true, ...(allowChanges && { allowChanges }), signal }),
    retry: retryQuery,
  };
}

/** An open document, or a session's code, at a revision: its latest if
 *  none is named. */
export interface DocumentRef {
  id: DocumentId;
  revision?: number;
}

export interface DocumentGoalsInput {
  /** The goals `before` the tactic at each position, by default, or
   *  `after` it. */
  mode?: GoalsMode;
  timeout?: number;
}

/** The goals at positions of a document. A revision's goals never change,
 *  so they are kept for good when the revision is named. */
export function documentGoalsOptions(
  client: PisaClient,
  doc: DocumentRef,
  at: LinePos[],
  input: DocumentGoalsInput = {},
): PisaQueryOptions<DocumentGoals> {
  const { id, revision } = doc;
  const asked = { at, mode: input.mode ?? "before", timeout: input.timeout ?? null };
  return {
    queryKey: [...pisaKeys.documents(client), id, revision ?? null, "goals", asked] as QueryKey,
    queryFn: ({ signal }) =>
      client.documentGoals(
        id,
        at,
        { ...(revision !== undefined && { revision }), ...(input.mode !== undefined && { mode: input.mode }) },
        { ...(input.timeout !== undefined && { timeout: input.timeout }), signal },
      ),
    staleTime: revision !== undefined ? Infinity : 0,
    retry: retryQuery,
  };
}

/** Pages of snapshots, newest first. The last page may be empty. */
export function snapshotsOptions(
  client: PisaClient,
  params: Omit<ListSnapshotsParams, "after"> = {},
  input: VersionInput = {},
): PisaInfiniteOptions<ListSnapshotsResponse, string | undefined> {
  return {
    queryKey: [...pisaKeys.snapshots(client), "list", input.version ?? client.version ?? null, params] as QueryKey,
    queryFn: ({ pageParam, signal }) =>
      client.listSnapshots({ ...params, ...(pageParam !== undefined && { after: pageParam }) }, { ...input, signal }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next,
    retry: retryQuery,
  };
}
