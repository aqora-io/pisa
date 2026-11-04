import type { Health, PisaError } from "@aqora/pisa-client";
import { type UseQueryResult, useQuery } from "@tanstack/react-query";

import { healthOptions } from "../options.ts";
import { usePisaClient } from "../provider.tsx";

/** The server's health, every 5 s: whether it is up or draining, its live
 *  workers (with none, checks wait and then get a 429), and search. */
export function useHealth(): UseQueryResult<Health, PisaError> {
  return useQuery(healthOptions(usePisaClient()));
}
