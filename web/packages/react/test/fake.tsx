import { PisaClient } from "@aqora/pisa-client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";

import { PisaProvider } from "../src/index.ts";

export interface Call {
  method: string;
  path: string;
  body: Record<string, unknown> | undefined;
  headers: Record<string, string>;
  signal?: AbortSignal;
}

/** A client over a fake server: `route` answers each request, with a value
 *  for a 200 or a Response. */
export function fakeClient(route: (call: Call) => unknown | Promise<unknown>): { client: PisaClient; calls: Call[] } {
  const calls: Call[] = [];
  const client = new PisaClient({
    url: "http://pisa.test",
    fetch: async (input, init) => {
      const url = new URL(String(input));
      const call: Call = {
        method: init?.method ?? "GET",
        path: url.pathname + url.search,
        body: init?.body === undefined ? undefined : (JSON.parse(String(init.body)) as Record<string, unknown>),
        headers: (init?.headers ?? {}) as Record<string, string>,
        ...(init?.signal != null && { signal: init.signal }),
      };
      calls.push(call);
      const signal = init?.signal;
      const answer = await Promise.race([
        Promise.resolve(route(call)),
        new Promise<never>((_, reject) => signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")))),
      ]);
      return answer instanceof Response ? answer : new Response(JSON.stringify(answer), { status: 200 });
    },
  });
  return { client, calls };
}

/** Renders hooks with `client`, in `queryClient`'s cache: pass one to look
 *  at the cache, or to share it between renders. */
export function wrapper(
  client: PisaClient,
  queryClient: QueryClient = new QueryClient(),
): (props: { children: ReactNode }) => ReactNode {
  return ({ children }) => (
    <QueryClientProvider client={queryClient}>
      <PisaProvider client={client}>{children}</PisaProvider>
    </QueryClientProvider>
  );
}

/** A promise and the function that settles it. */
export function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}
