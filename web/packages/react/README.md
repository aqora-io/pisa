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
  and then get a 429), which run without limits, which versions' sweeps are
  refused, and whether search can serve.
- **Checks.**
  - `useCheck({ code, version?, timeout?, detail?, at? } | null)` checks the code.
    Its `data` is `{ code, result }`, so an editor draws diagnostics only while
    its text is still the code that was checked. `cancel()` stops waiting.
  - `useLiveCheck(code, { delayMs? })` checks as the code is edited. It keeps at
    most one check in flight and sends the newest code once that check is done.
    Aborting a check a worker has started frees nothing, so newer code waits its
    turn.
  - `useCheckBatch({ codes, ...options } | null)` checks up to 128 codes in
    one request; its `data` gives each code its result, in order.
- **Proof sessions.** `useProofSession({ version?, timeout? })` holds a session
  as a tree, and offers `start(code)`, `apply(nodeId, tactic)`, `check(code)`,
  `reset()`, `path(id)` and `proofText(leaves)`.
  - Each `sorry` is a root, and every tactic adds a node. Nodes have ids of
    their own, as two tactics can reach the same state.
  - Branches run in parallel, and a tactic Lean rejects marks its node
    `failed`, with the server's `error`. A failed node can't be built on.
  - Requests are aborted, and the session closed, on reset, on starting
    again and on unmount. Call `start` from an event, not an effect.
  - `check(code, { detail: ["declarations"] })` checks with the
    declarations in full.
- **Snapshots.**
  - `useSnapshots({ labels?, limit? })` is an infinite query; page with
    `fetchNextPage`.
  - `useSnapshot(id, { detail? })` reads one snapshot; `detail:
    ["declarations"]` gives its check's declarations in full.
  - `useCreateSnapshot()`, `useUpdateSnapshot()`, `useDeleteSnapshot()` and
    `useVerifySnapshot()` change snapshots; `useUpdateSnapshot` takes `{ id,
    labels }`, a merge patch. `useDeleteSnapshot` takes an id or `{ id,
    withReplacers }`, and forgets every snapshot the delete took;
    `useVerifySnapshot` an id or `{ id, timeout }`. Each invalidates every
    snapshot query, as a module's `verified` and `status` are shared by all
    its snapshots.
  - `useDependents(id, { labels? })` lists what builds on a snapshot, and
    `useAudit({ labels } | { against, allow?, review? })` audits a project
    or a run against its spec; both refetch when any snapshot changes.
- **Documents.** `useDocument({ version?, timeout? })` keeps code open on
  Lean Beam: `open(code)`, then `edit(code)` a revision at a time (only the
  newest code waits for the edit in flight; an expired document is opened
  again), and `close()`; it closes on unmount. `useDocumentGoals({ id,
  revision? }, at, { mode? })` asks for goals at positions, kept for good
  per revision; a session's code is `{ id: session, revision: 1 }`.
  `useProbeDocument()` tries candidate tactics at a sorry or a position.
- **Long jobs.** `useLongJob(submit, { wait? })` runs a request as a job
  (`submit` is e.g. `(client, code, o) => client.submitCheck(code, o)`): a
  mutation whose `job` shows `queued`, `running` then `done`, polled every
  `wait` seconds (10). `cancel()` withdraws a job no worker has started, and
  the mutation fails `aborted`; one a worker has started runs on. Unmounting
  stops waiting only.
- **Runs.** `useRebuildPlan({ from, to, labels, allowChanges? } | null)`
  plans a rebuild, a dry run answered at once; `useRebuildSnapshots()` runs
  one as a long job, and refetches every snapshot query once it ends.
  `useAcceptRun()` accepts a run, its audit and `lake comparator` on each
  pair, as a long job, `useAcceptances(spec | null)` lists a spec's stored
  acceptances, and `useExportRun()` gets its `pisa-run.tar.gz` as a Blob to
  save.
- **Search.**
  - `useSearch(query)` runs once the query has a term, and keeps the last hits
    while new ones load. A result with `dense_pending` is stale at once, so
    `refetch()` or asking again gets the dense lane once it's embedded.
  - `useResolve(names)`, and `useDeclaration(name, module?, { version? })`,
    where `module` is a snapshot hit's, to open that snapshot's declaration.
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
