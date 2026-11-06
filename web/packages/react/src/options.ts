// Query options for every read, for `useQuery`, `useQueries` or
// prefetching. Keys start with `["pisa", url]` and end
// with what the request sends, so that a different version or environment
// never reuses another's results.

import type {
  CheckOptions,
  CheckResult,
  DeclDetail,
  Health,
  ListSnapshotsParams,
  ListSnapshotsResponse,
  PisaClient,
  PisaError,
  Query,
  QueryResult,
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
  search: (c: PisaClient): QueryKey => ["pisa", c.url, "search"],
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

export interface SearchInput {
  envId?: string;
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

const TERMS = ["q", "nl", "latex", "pattern", "goal", "name"] as const;

/** Whether `query` asks for anything. */
export function hasTerms(query: Query): boolean {
  return TERMS.some((t) => (query[t] ?? "").trim() !== "");
}

export function searchOptions(client: PisaClient, query: Query, input: SearchInput = {}): PisaQueryOptions<QueryResult> {
  const compact = compactQuery(query);
  const envId = input.envId ?? client.envId;
  return {
    queryKey: [...pisaKeys.search(client), envId ?? null, compact] as QueryKey,
    queryFn: ({ signal }) => client.search(compact, { ...(envId !== undefined && { envId }), signal }),
    // Snapshots keep being indexed; the libraries don't change. A query still
    // being embedded gets the dense lane if asked again.
    staleTime: (q) => (q.state.data?.dense_pending === true ? 0 : compact.snapshots === undefined ? Infinity : 30_000),
    enabled: hasTerms(compact),
    retry: retryQuery,
  };
}

export function resolveOptions(client: PisaClient, names: string[], input: SearchInput = {}): PisaQueryOptions<Resolution[]> {
  const envId = input.envId ?? client.envId;
  return {
    queryKey: [...pisaKeys.search(client), envId ?? null, "resolve", names] as QueryKey,
    queryFn: ({ signal }) => client.resolve(names, { ...(envId !== undefined && { envId }), signal }),
    staleTime: Infinity,
    enabled: names.length > 0,
    retry: retryQuery,
  };
}

/** Declaration `name`: the one in snapshot module `module` if it has it, else
 *  the bundle's. */
export function declOptions(client: PisaClient, name: string, module?: string): PisaQueryOptions<DeclDetail> {
  return {
    queryKey: [...pisaKeys.search(client), "decl", name, module ?? null] as QueryKey,
    queryFn: ({ signal }) => client.decl(name, { ...(module !== undefined && { module }), signal }),
    staleTime: Infinity,
    retry: retryQuery,
  };
}

export interface VersionInput {
  version?: string;
}

/** One snapshot, keyed by its id alone: the id carries its version. */
export function snapshotOptions(client: PisaClient, id: SnapshotId): PisaQueryOptions<SnapshotDetail> {
  return {
    queryKey: [...pisaKeys.snapshots(client), "snapshot", id] as QueryKey,
    queryFn: ({ signal }) => client.getSnapshot(id, { signal }),
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
