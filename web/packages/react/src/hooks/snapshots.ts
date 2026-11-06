import type {
  CreateSnapshotOptions,
  CreateSnapshotResponse,
  DeleteSnapshotResponse,
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

import { type VersionInput, pisaKeys, snapshotOptions, snapshotsOptions } from "../options.ts";
import { usePisaClient } from "../provider.tsx";

/** Snapshots with every label in `params.labels`, newest first, a page at a
 *  time: `fetchNextPage` while `hasNextPage`. */
export function useSnapshots(
  params: Omit<ListSnapshotsParams, "after"> = {},
  input: VersionInput = {},
): UseInfiniteQueryResult<InfiniteData<ListSnapshotsResponse, string | undefined>, PisaError> {
  return useInfiniteQuery(snapshotsOptions(usePisaClient(), params, input));
}

/** One snapshot, or nothing while `id` is null. */
export function useSnapshot(id: SnapshotId | null, input: VersionInput = {}): UseQueryResult<SnapshotDetail, PisaError> {
  return useQuery({ ...snapshotOptions(usePisaClient(), id ?? "", input), enabled: id !== null });
}

/** Invalidates every snapshot query of the version: a change to one snapshot
 *  can show in others, as a module's `verified` and `status` are shared by
 *  its snapshots. */
function useInvalidate(input: VersionInput): () => Promise<void> {
  const client = usePisaClient();
  const queryClient = useQueryClient();
  return () => queryClient.invalidateQueries({ queryKey: pisaKeys.snapshots(client, input.version) });
}

export interface CreateSnapshotInput extends Omit<CreateSnapshotOptions, "signal" | "version"> {
  code: string;
}

export function useCreateSnapshot(
  input: VersionInput = {},
): UseMutationResult<CreateSnapshotResponse, PisaError, CreateSnapshotInput> {
  const client = usePisaClient();
  const invalidate = useInvalidate(input);
  return useMutation({
    mutationFn: ({ code, ...options }) => client.createSnapshot(code, { ...options, ...input }),
    onSuccess: invalidate,
  });
}

export interface UpdateSnapshotInput extends UpdateSnapshotRequest {
  id: SnapshotId;
}

export function useUpdateSnapshot(input: VersionInput = {}): UseMutationResult<Snapshot, PisaError, UpdateSnapshotInput> {
  const client = usePisaClient();
  const queryClient = useQueryClient();
  const invalidate = useInvalidate(input);
  return useMutation({
    mutationFn: ({ id, ...update }) => client.updateSnapshot(id, update, input),
    onSuccess: async (snapshot) => {
      queryClient.setQueryData<SnapshotDetail>(snapshotOptions(client, snapshot.id, input).queryKey, (old) =>
        old === undefined ? old : { ...old, labels: snapshot.labels },
      );
      await invalidate();
    },
  });
}

export function useDeleteSnapshot(input: VersionInput = {}): UseMutationResult<DeleteSnapshotResponse, PisaError, SnapshotId> {
  const client = usePisaClient();
  const queryClient = useQueryClient();
  const invalidate = useInvalidate(input);
  return useMutation({
    mutationFn: (id) => client.deleteSnapshot(id, input),
    onSuccess: async (_, id) => {
      queryClient.removeQueries({ queryKey: snapshotOptions(client, id, input).queryKey });
      await invalidate();
    },
  });
}

export function useVerifySnapshot(input: VersionInput = {}): UseMutationResult<VerifySnapshotResponse, PisaError, SnapshotId> {
  const client = usePisaClient();
  const invalidate = useInvalidate(input);
  return useMutation({
    mutationFn: (id) => client.verifySnapshot(id, input),
    onSuccess: invalidate,
  });
}
