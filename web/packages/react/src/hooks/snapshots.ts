import type {
  AuditParams,
  AuditSnapshotsResponse,
  CreateSnapshotOptions,
  CreateSnapshotResponse,
  DeleteSnapshotOptions,
  DeleteSnapshotResponse,
  Labels,
  ListDependentsResponse,
  ListSnapshotsParams,
  ListSnapshotsResponse,
  PisaError,
  Snapshot,
  SnapshotDetail,
  SnapshotId,
  UpdateSnapshotRequest,
  VerifySnapshotResponse,
} from "@aqora/pisa-client";
import {
  type InfiniteData,
  type UseInfiniteQueryResult,
  type UseMutationResult,
  type UseQueryResult,
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";

import {
  type SnapshotInput,
  type VersionInput,
  auditOptions,
  dependentsOptions,
  pisaKeys,
  snapshotOptions,
  snapshotsOptions,
} from "../options.ts";
import { usePisaClient } from "../provider.tsx";

/** Snapshots with every label in `params.labels`, newest first, a page at a
 *  time: `fetchNextPage` while `hasNextPage`. */
export function useSnapshots(
  params: Omit<ListSnapshotsParams, "after"> = {},
  input: VersionInput = {},
): UseInfiniteQueryResult<InfiniteData<ListSnapshotsResponse, string | undefined>, PisaError> {
  return useInfiniteQuery(snapshotsOptions(usePisaClient(), params, input));
}

/** One snapshot, or nothing while `id` is null; its check's declarations in
 *  full with `detail: ["declarations"]`. */
export function useSnapshot(id: SnapshotId | null, input: SnapshotInput = {}): UseQueryResult<SnapshotDetail, PisaError> {
  return useQuery({ ...snapshotOptions(usePisaClient(), id ?? "", input), enabled: id !== null });
}

/** What builds on snapshot `id`, parents first, or nothing while `id` is
 *  null. Refetched after any snapshot changes. */
export function useDependents(
  id: SnapshotId | null,
  params: { labels?: Labels } = {},
): UseQueryResult<ListDependentsResponse, PisaError> {
  return useQuery({ ...dependentsOptions(usePisaClient(), id ?? "", params), enabled: id !== null });
}

/** An audit by labels or against spec snapshots, or nothing while `params`
 *  is null or names no spec snapshot. Refetched after any snapshot
 *  changes. */
export function useAudit(params: AuditParams | null, input: VersionInput = {}): UseQueryResult<AuditSnapshotsResponse, PisaError> {
  const enabled = params !== null && params.against?.length !== 0;
  return useQuery({ ...auditOptions(usePisaClient(), params ?? {}, input), enabled });
}

/** Invalidates every snapshot query: a change to one snapshot can show in
 *  others, as a module's `verified` and `status` are shared by its
 *  snapshots. */
function useInvalidate(): () => Promise<void> {
  const client = usePisaClient();
  const queryClient = useQueryClient();
  return () => queryClient.invalidateQueries({ queryKey: pisaKeys.snapshots(client) });
}

export interface CreateSnapshotInput extends Omit<CreateSnapshotOptions, "signal" | "version"> {
  code: string;
}

export function useCreateSnapshot(
  input: VersionInput = {},
): UseMutationResult<CreateSnapshotResponse, PisaError, CreateSnapshotInput> {
  const client = usePisaClient();
  const invalidate = useInvalidate();
  return useMutation({
    mutationFn: ({ code, ...options }) => client.createSnapshot(code, { ...options, ...input }),
    onSuccess: invalidate,
  });
}

export interface UpdateSnapshotInput extends UpdateSnapshotRequest {
  id: SnapshotId;
}

export function useUpdateSnapshot(): UseMutationResult<Snapshot, PisaError, UpdateSnapshotInput> {
  const client = usePisaClient();
  const queryClient = useQueryClient();
  const invalidate = useInvalidate();
  return useMutation({
    mutationFn: ({ id, ...update }) => client.updateSnapshot(id, update),
    onSuccess: async (snapshot) => {
      queryClient.setQueriesData<SnapshotDetail>({ queryKey: pisaKeys.snapshot(client, snapshot.id) }, (old) =>
        old === undefined ? old : { ...old, labels: snapshot.labels },
      );
      await invalidate();
    },
  });
}

/** A snapshot to delete, alone or `withReplacers`. */
export type DeleteSnapshotInput = SnapshotId | ({ id: SnapshotId } & Omit<DeleteSnapshotOptions, "signal">);

/** Deletes a snapshot, and forgets every snapshot the delete took. */
export function useDeleteSnapshot(): UseMutationResult<DeleteSnapshotResponse, PisaError, DeleteSnapshotInput> {
  const client = usePisaClient();
  const queryClient = useQueryClient();
  const invalidate = useInvalidate();
  return useMutation({
    mutationFn: (input) => {
      const { id, ...options } = typeof input === "string" ? { id: input } : input;
      return client.deleteSnapshot(id, options);
    },
    onSuccess: async (deleted, input) => {
      const ids = new Set([typeof input === "string" ? input : input.id, ...deleted.deleted]);
      for (const id of ids) queryClient.removeQueries({ queryKey: pisaKeys.snapshot(client, id) });
      await invalidate();
    },
  });
}

/** A snapshot to verify, with Lean's limit for each module. */
export type VerifySnapshotInput = SnapshotId | { id: SnapshotId; timeout?: number };

export function useVerifySnapshot(): UseMutationResult<VerifySnapshotResponse, PisaError, VerifySnapshotInput> {
  const client = usePisaClient();
  const invalidate = useInvalidate();
  return useMutation({
    mutationFn: (input) => {
      const { id, ...options } = typeof input === "string" ? { id: input } : input;
      return client.verifySnapshot(id, options);
    },
    onSuccess: invalidate,
  });
}
