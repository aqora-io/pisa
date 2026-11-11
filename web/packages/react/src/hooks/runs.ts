import type {
  AcceptResponse,
  Labels,
  PisaError,
  RebuildOptions,
  RebuildSnapshotsResponse,
  SnapshotId,
} from "@aqora/pisa-client";
import { type UseMutationResult, type UseQueryResult, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { type RebuildPlanInput, pisaKeys, rebuildPlanOptions } from "../options.ts";
import { usePisaClient } from "../provider.tsx";
import { type LongJob, type LongJobOptions, useLongJob } from "./jobs.ts";

/** What a rebuild would create, or nothing while `input` is null. */
export function useRebuildPlan(input: RebuildPlanInput | null): UseQueryResult<RebuildSnapshotsResponse, PisaError> {
  const options = rebuildPlanOptions(usePisaClient(), input ?? { from: "", to: "", labels: {} });
  return useQuery({ ...options, enabled: input !== null });
}

/** A rebuild to run. */
export type RebuildRunInput = { from: SnapshotId; to: SnapshotId; labels: Labels } & Omit<
  RebuildOptions,
  "signal" | "dryRun"
>;

/** Runs rebuilds as jobs, which can take minutes. Every snapshot query is
 *  refetched once one is done, or has failed, as it may have created some
 *  snapshots before failing. */
export function useRebuildSnapshots(
  options: Omit<LongJobOptions<RebuildSnapshotsResponse, RebuildRunInput>, "onSettled"> = {},
): LongJob<RebuildSnapshotsResponse, RebuildRunInput> {
  const client = usePisaClient();
  const queryClient = useQueryClient();
  return useLongJob(
    (c, { from, to, labels, ...rest }, o) => c.submitRebuildSnapshots(from, to, labels, { ...rest, ...o }),
    { ...options, onSettled: () => queryClient.invalidateQueries({ queryKey: pisaKeys.snapshots(client) }) },
  );
}

/** A run to accept: its spec snapshots, the definitions it may change
 *  besides the spec's holes, whether comparator runs Lean's other kernels
 *  too, and the seconds for each pair. */
export interface AcceptInput {
  against: SnapshotId[];
  allow?: string[];
  paranoid?: boolean;
  timeout?: number;
}

/** Accepts runs, as long jobs: the audit, and `lake comparator` on each
 *  pair, which builds the spec and the head and takes minutes. The answer
 *  is kept under its `id`. */
export function useAcceptRun(
  options: LongJobOptions<AcceptResponse, AcceptInput> = {},
): LongJob<AcceptResponse, AcceptInput> {
  return useLongJob(
    (c, { against, allow, paranoid, timeout }, { signal }) =>
      c.submitAcceptRun(
        against,
        { ...(allow !== undefined && { allow }), ...(paranoid !== undefined && { paranoid }) },
        { ...(timeout !== undefined && { timeout }), signal },
      ),
    options,
  );
}

/** A run to export. */
export interface ExportRunInput {
  against: SnapshotId[];
  allow?: string[];
}

/** Exports runs as Lake projects: each a `pisa-run.tar.gz` Blob, for the
 *  page to save. */
export function useExportRun(): UseMutationResult<Blob, PisaError, ExportRunInput> {
  const client = usePisaClient();
  return useMutation({
    mutationFn: async ({ against, allow }) =>
      new Blob([await client.exportRun(against, { ...(allow !== undefined && { allow }) })], { type: "application/gzip" }),
  });
}
