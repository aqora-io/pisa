import type { CheckOptions, PisaError } from "@aqora/pisa-client";
import { type UseQueryResult, keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";

import { useDebouncedValue } from "../debounce.ts";
import { type CheckInput, type Checked, checkOptions } from "../options.ts";
import { usePisaClient } from "../provider.tsx";

export type CheckQuery = UseQueryResult<Checked, PisaError> & {
  /** Stops waiting for the check. A worker that has started it runs it to
   *  the end, or to its timeout; only a check no worker took yet is given
   *  up. */
  cancel: () => void;
};

/** Checks `input.code`, or nothing while `input` is null. A result for the
 *  same request is reused, unless it failed in a way that may not recur
 *  (e.g. a timeout). While a new request runs, the last result stays. */
export function useCheck(input: CheckInput | null): CheckQuery {
  const client = usePisaClient();
  const queryClient = useQueryClient();
  const options = checkOptions(client, input ?? { code: "" });
  const query = useQuery({ ...options, enabled: input !== null, placeholderData: keepPreviousData });
  const cancel = () => void queryClient.cancelQueries({ queryKey: options.queryKey, exact: true });
  return Object.assign(query, { cancel });
}

export interface LiveCheckOptions extends Omit<CheckOptions, "signal"> {
  /** How long the code must stay unchanged before it is checked; 500 ms by
   *  default. */
  delayMs?: number;
}

export type LiveCheckQuery = CheckQuery & {
  /** The code isn't checked yet as it stands. */
  stale: boolean;
};

/** Checks `code` as it is edited: once it has settled, and only once the
 *  check before has finished, so that one editor never fills more than one
 *  worker slot. Aborting a check a worker has started frees nothing, so
 *  newer code waits its turn instead. */
export function useLiveCheck(code: string, options: LiveCheckOptions = {}): LiveCheckQuery {
  const { delayMs = 500, ...rest } = options;
  const settled = useDebouncedValue(code, delayMs);
  const [submitted, setSubmitted] = useState<string | null>(null);
  const query = useCheck(submitted === null ? null : { ...rest, code: submitted });
  const busy = query.isFetching;
  useEffect(() => {
    if (!busy && settled.trim() !== "" && settled !== submitted) setSubmitted(settled);
  }, [busy, settled, submitted]);
  return Object.assign(query, { stale: code !== query.data?.code });
}
