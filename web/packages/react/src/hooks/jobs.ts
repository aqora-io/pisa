import { type Job, type JobSchema, type JobStatus, type PisaClient, PisaError } from "@aqora/pisa-client";
import { type UseMutationResult, useMutation } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";

import { usePisaClient } from "../provider.tsx";

export interface LongJobOptions<T, V> {
  /** Seconds each look at the job waits; how soon `job.status` follows the
   *  server's. 10 by default. */
  wait?: number;
  onSuccess?: (data: T, variables: V) => unknown;
  onSettled?: (data: T | undefined, error: PisaError | null, variables: V) => unknown;
}

export type LongJob<T, V> = UseMutationResult<T, PisaError, V> & {
  /** The job as last seen: `queued`, `running`, then `done`; or `cancelled`.
   *  Undefined until the server has queued it. */
  job: JobSchema | undefined;
  /** Withdraws the job if no worker has started it, and stops waiting: the
   *  mutation then fails `aborted`. One a worker has started runs on, and is
   *  still waited for. Resolves what became of it, or nothing if the
   *  server hadn't queued it yet, which it is withdrawn as soon as it is. */
  cancel: () => Promise<JobStatus | undefined>;
};

/** One run of the mutation. */
interface Run {
  controller: AbortController;
  job?: JobSchema;
  /** Whether `cancel` was called before the job was queued. */
  wanted: boolean;
  /** The cancel sent, once it is. */
  cancel?: Promise<JobStatus | undefined>;
}

/** A request run as a job (`submit`, e.g. `client.submitCreateSnapshot`),
 *  waited for with its progress in `job`. Unmounting stops waiting only:
 *  the job runs on, and its answer is kept an hour under `job.id`. */
export function useLongJob<T, V>(
  submit: (client: PisaClient, variables: V, options: { signal: AbortSignal }) => Promise<Job<T>>,
  options: LongJobOptions<T, V> = {},
): LongJob<T, V> {
  const client = usePisaClient();
  const { wait = 10, onSuccess, onSettled } = options;
  const [job, setJob] = useState<JobSchema | undefined>();
  const run = useRef<Run | null>(null);
  useEffect(() => () => run.current?.controller.abort(), []);

  const withdraw = async (current: Run, queued: JobSchema): Promise<JobStatus | undefined> => {
    const r = await client.cancelJob(queued);
    setJob(r);
    if (r.status === "cancelled") current.controller.abort();
    return r.status;
  };

  const mutation = useMutation<T, PisaError, V>({
    mutationFn: async (variables) => {
      const current: Run = { controller: new AbortController(), wanted: false };
      run.current?.controller.abort();
      run.current = current;
      setJob(undefined);
      const { signal } = current.controller;
      const queued = await submit(client, variables, { signal });
      current.job = queued;
      setJob(queued);
      if (current.wanted) current.cancel = withdraw(current, queued);
      try {
        const answer = await client.waitJob(queued, { signal, wait, onPending: setJob });
        setJob({ ...queued, status: "done" });
        return answer;
      } catch (e) {
        // Withdrawn: the wait was aborted, or the job was gone before the
        // cancel's answer came.
        const status = await current.cancel?.catch(() => undefined);
        if (status === "cancelled") throw new PisaError("aborted", "the job was cancelled", undefined, { cause: e });
        throw e;
      }
    },
    ...(onSuccess !== undefined && { onSuccess }),
    ...(onSettled !== undefined && { onSettled }),
  });

  const cancel = (): Promise<JobStatus | undefined> => {
    const current = run.current;
    if (current === null) return Promise.resolve(undefined);
    if (current.job === undefined) {
      current.wanted = true;
      return Promise.resolve(undefined);
    }
    current.cancel ??= withdraw(current, current.job);
    return current.cancel;
  };
  // Taken before `Object.assign` replaces it with `reset`.
  const forget = mutation.reset;
  const reset = () => {
    run.current?.controller.abort();
    run.current = null;
    setJob(undefined);
    forget();
  };
  return Object.assign(mutation, { job, cancel, reset });
}
