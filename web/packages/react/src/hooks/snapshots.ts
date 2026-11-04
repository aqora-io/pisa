import type {
  CreateSnapshotOptions,
  Deleted,
  Labels,
  ListSnapshotsParams,
  PisaError,
  SnapshotInfo,
  SnapshotList,
  SnapshotResponse,
  SnapshotSummary,
  SnapshotVerified,
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

/** Entries with every label in `params.labels`, newest first, a page at a
 *  time: `fetchNextPage` while `hasNextPage`. */
export function useSnapshots(
  params: Omit<ListSnapshotsParams, "after"> = {},
  input: VersionInput = {},
): UseInfiniteQueryResult<InfiniteData<SnapshotList, string | undefined>, PisaError> {
  return useInfiniteQuery(snapshotsOptions(usePisaClient(), params, input));
}

/** One entry, or nothing while `entry` is null. */
export function useSnapshot(entry: string | null, input: VersionInput = {}): UseQueryResult<SnapshotInfo, PisaError> {
  return useQuery({ ...snapshotOptions(usePisaClient(), entry ?? "", input), enabled: entry !== null });
}

/** Invalidates every snapshot query of the version: a change to one entry
 *  can show in others, as a module's `verified` and `dirty` are shared by
 *  its entries. */
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
): UseMutationResult<SnapshotResponse, PisaError, CreateSnapshotInput> {
  const client = usePisaClient();
  const invalidate = useInvalidate(input);
  return useMutation({
    mutationFn: ({ code, ...options }) => client.createSnapshot(code, { ...options, ...input }),
    onSuccess: invalidate,
  });
}

export interface LabelsInput {
  entry: string;
  set?: Labels;
  remove?: string[];
}

export function useSetSnapshotLabels(input: VersionInput = {}): UseMutationResult<SnapshotSummary, PisaError, LabelsInput> {
  const client = usePisaClient();
  const queryClient = useQueryClient();
  const invalidate = useInvalidate(input);
  return useMutation({
    mutationFn: ({ entry, ...change }) => client.setSnapshotLabels(entry, change, input),
    onSuccess: async (summary) => {
      queryClient.setQueryData<SnapshotInfo>(snapshotOptions(client, summary.entry, input).queryKey, (old) =>
        old === undefined ? old : { ...old, labels: summary.labels },
      );
      await invalidate();
    },
  });
}

export function useDeleteSnapshot(input: VersionInput = {}): UseMutationResult<Deleted, PisaError, string> {
  const client = usePisaClient();
  const queryClient = useQueryClient();
  const invalidate = useInvalidate(input);
  return useMutation({
    mutationFn: (entry) => client.deleteSnapshot(entry, input),
    onSuccess: async (deleted) => {
      queryClient.removeQueries({ queryKey: snapshotOptions(client, deleted.entry, input).queryKey });
      await invalidate();
    },
  });
}

export function useVerifySnapshot(input: VersionInput = {}): UseMutationResult<SnapshotVerified, PisaError, string> {
  const client = usePisaClient();
  const invalidate = useInvalidate(input);
  return useMutation({
    mutationFn: (entry) => client.verifySnapshot(entry, input),
    onSuccess: invalidate,
  });
}
