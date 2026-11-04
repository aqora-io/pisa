import type { DeclDetail, PisaError, Query, QueryResult, Resolution } from "@aqora/pisa-client";
import { type UseQueryResult, keepPreviousData, useQuery } from "@tanstack/react-query";

import { type SearchInput, declOptions, resolveOptions, searchOptions } from "../options.ts";
import { usePisaClient } from "../provider.tsx";

/** Searches once `query` has a term, keeping the last hits while the next
 *  ones load. Debounce typed input with `useDebouncedValue`. */
export function useSearch(query: Query, input: SearchInput = {}): UseQueryResult<QueryResult, PisaError> {
  return useQuery({ ...searchOptions(usePisaClient(), query, input), placeholderData: keepPreviousData });
}

/** Whether each name exists, with close names for those that don't. */
export function useResolve(names: string[], input: SearchInput = {}): UseQueryResult<Resolution[], PisaError> {
  return useQuery(resolveOptions(usePisaClient(), names, input));
}

/** A declaration's details, or nothing while `name` is null. */
export function useDecl(name: string | null): UseQueryResult<DeclDetail, PisaError> {
  return useQuery({ ...declOptions(usePisaClient(), name ?? ""), enabled: name !== null });
}
