import type { DeclarationDetail, ModuleName, PisaError, Query, QueryResult, Resolution } from "@aqora/pisa-client";
import { type UseQueryResult, keepPreviousData, useQuery } from "@tanstack/react-query";

import { type VersionInput, declarationOptions, resolveOptions, searchOptions } from "../options.ts";
import { usePisaClient } from "../provider.tsx";

/** Searches once `query` has a term, keeping the last hits while the next
 *  ones load. Debounce typed input with `useDebouncedValue`. */
export function useSearch(query: Query, input: VersionInput = {}): UseQueryResult<QueryResult, PisaError> {
  return useQuery({ ...searchOptions(usePisaClient(), query, input), placeholderData: keepPreviousData });
}

/** Whether each name exists, with close names for those that don't. */
export function useResolve(names: string[], input: VersionInput = {}): UseQueryResult<Resolution[], PisaError> {
  return useQuery(resolveOptions(usePisaClient(), names, input));
}

/** A declaration's details, or nothing while `name` is null. Pass a snapshot
 *  hit's `module` to open that snapshot's declaration. */
export function useDeclaration(
  name: string | null,
  module?: ModuleName,
  input: VersionInput = {},
): UseQueryResult<DeclarationDetail, PisaError> {
  return useQuery({ ...declarationOptions(usePisaClient(), name ?? "", module, input), enabled: name !== null });
}
