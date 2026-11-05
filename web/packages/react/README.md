# @aqora/pisa-react

React hooks for a pisa server, built on TanStack Query v5 and
`@aqora/pisa-client`: Lean checks, proof sessions, snapshots and declaration
search.

```tsx
import { PisaClient } from "@aqora/pisa-client";
import { PisaProvider, useLiveCheck, useProofSession, useSearch } from "@aqora/pisa-react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const client = new PisaClient({ url: "/api" });

<QueryClientProvider client={new QueryClient()}>
  <PisaProvider client={client}>
    <App />
  </PisaProvider>
</QueryClientProvider>;

function Editor({ code }: { code: string }) {
  const check = useLiveCheck(code); // check.data: { code, result }
  return check.data?.result.ok ? <p>✓</p> : <pre>{check.data?.result.reasons.join("\n")}</pre>;
}
```

## Hooks

- **Health.** `useHealth()` polls the server every 5 s: it says whether the
  server is up or draining, how many workers are live (with none, checks wait
  and then get a 429), and whether search can serve.
- **Checks.**
  - `useCheck({ code, version?, timeout?, detail? } | null)` checks the code.
    Its `data` is `{ code, result }`, so an editor draws diagnostics only while
    its text is still the code that was checked. `cancel()` stops waiting.
  - `useLiveCheck(code, { delayMs? })` checks as the code is edited. It keeps at
    most one check in flight and sends the newest code once that check is done.
    Aborting a check a worker has started frees nothing, so newer code waits its
    turn.
- **Proof sessions.** `useProofSession({ version?, timeout? })` holds a session
  as a tree, and offers `start(code)`, `apply(nodeId, tactic)`, `check(code)`,
  `reset()`, `path(id)` and `proofText(leaves)`.
  - Each `sorry` is a root, and every tactic adds a node. Nodes have ids of
    their own, as two tactics can reach the same state.
  - Branches run in parallel, and a tactic Lean rejects marks its node
    `failed`: with `error` when the server refused it, or with an error in
    `messages` when Lean recovered (an unknown identifier, say). A failed node
    can't be built on.
  - Requests are aborted on reset and unmount. Call `start` from an event,
    not an effect.
- **Snapshots.**
  - `useSnapshots({ labels?, limit? })` is an infinite query; page with
    `fetchNextPage`.
  - `useSnapshot(entry)` reads one entry.
  - `useCreateSnapshot()`, `useSetSnapshotLabels()`, `useDeleteSnapshot()` and
    `useVerifySnapshot()` change entries. Each invalidates every snapshot query
    of its version, as a module's `verified` and `dirty` are shared by all its
    entries.
- **Search.**
  - `useSearch(query)` runs once the query has a term, and keeps the last hits
    while new ones load. A result with `dense_pending` is stale at once, so
    `refetch()` or asking again gets the dense lane once it's embedded.
  - `useResolve(names)`, and `useDecl(name, module?)`, where `module` is a
    snapshot hit's, to open that snapshot's declaration.
  - `useDebouncedValue` debounces typed input.

**Query options.** `checkOptions`, `searchOptions` and the like give the same
options for `useQueries` or prefetching. Keys start with `["pisa", url]`, so
servers don't share a cache.

**Caching and retries.**
- A check is cached for good, unless it failed in a way that may not recur.
- Searches over snapshots go stale after 30 s.
- Queries retry once after a 503 and twice after a network error. A 429
  already means the server waited for a worker, so it isn't retried.
- Mutations aren't retried.
