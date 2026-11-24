# @aqora/pisa-react

React hooks for a pisa server, built on TanStack Query v5 and
`@aqora/pisa-client`: Lean checks, proof sessions, snapshots and declaration
search.

```sh
npm install @aqora/pisa-react @aqora/pisa-client @tanstack/react-query
```

It needs React 19.2 or later and `@tanstack/react-query` 5.100 or later.

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

- **The client.** `PisaProvider` gives the hooks below it a `PisaClient`;
  they also need a `QueryClientProvider` above them. `usePisaClient()`
  returns that client, for the calls no hook covers.
- **Health.** `useHealth()` polls the server every 5 s: it says whether the
  server is up or draining, how many workers are live (with none, checks wait
  and then get a 429), which run without limits, which versions' sweeps are
  refused, and whether search can serve.
- **Checks.**
  - `useCheck({ code, version?, timeout?, detail?, at? } | null)` checks the code.
    Its `data` is `{ code, result }`, so an editor draws diagnostics only while
    its text is still the code that was checked. `cancel()` stops waiting.
  - `useLiveCheck(code, { delayMs?, ...options })` checks as the code is
    edited, once it has stayed unchanged for `delayMs` (500). It keeps at
    most one check in flight and sends the newest code once that check is
    done; `stale` says the code isn't checked yet as it stands. Aborting a
    check a worker has started frees nothing, so newer code waits its turn.
  - `useCheckBatch({ codes, ...options } | null)` checks up to 128 codes in
    one request; its `data` gives each code its result, in order.
- **Proof sessions.** `useProofSession({ version?, timeout? })` holds a session
  as a tree, and offers `start(code)`, `apply(nodeId, tactic)`, `check(code)`,
  `reset()`, `node(id)`, `children(id)`, `path(id)` and `proofText(leaves)`.
  - Each `sorry` is a root, and every tactic adds a node. Nodes have ids of
    their own, as two tactics can reach the same state.
  - Branches run in parallel, and a tactic Lean rejects marks its node
    `failed`, with the server's `error`. A failed node can't be built on.
  - Requests are aborted, and the session closed, on reset, on starting
    again and on unmount. Call `start` from an event, not an effect.
  - `check(code, { detail: ["declarations"] })` checks with the
    declarations in full.
- **Snapshots.**
  - `useSnapshots({ labels?, limit?, heads? }, { version? })` is an infinite
    query; page with `fetchNextPage`.
  - `useSnapshot(id, { detail? })` reads one snapshot; `detail:
    ["declarations"]` gives its check's declarations in full.
  - `useCreateSnapshot({ version? })`, `useUpdateSnapshot()`,
    `useDeleteSnapshot()` and `useVerifySnapshot()` change snapshots.
    `useCreateSnapshot` takes `{ code, ...options }`, with `createSnapshot`'s
    options, `replaces`, `holes`, `amends`, `refutes` and `name` among them;
    `useUpdateSnapshot` takes `{ id, labels }`, a merge patch;
    `useDeleteSnapshot` an id or `{ id, withReplacers }`, and forgets every
    snapshot the delete took; `useVerifySnapshot` an id or `{ id, timeout }`.
    Each invalidates every snapshot query, as a module's `verified` and
    `status` are shared by all its snapshots.
  - `useDependents(id, { labels? })` lists what builds on a snapshot, and
    `useAudit({ labels } | { against, allow?, review?, reviewLimit? })`
    audits a project or a run against its spec; both refetch when any
    snapshot changes.
- **Documents.** `useDocument({ version?, timeout? })` keeps code open on
  Lean Beam: `open(code)`, then `edit(code)` a revision at a time (only the
  newest code waits for the edit in flight; an expired document is opened
  again), and `close()`; it closes on unmount. `useDocumentGoals({ id,
  revision? }, at, { mode?, timeout? })` asks for goals at positions, kept
  for good per revision; a session's code is `{ id: session, revision: 1 }`.
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
  - `useSearch(query, { version? })` runs once the query has a term, and
    keeps the last hits while new ones load. A result with `dense_pending` is
    stale at once, so `refetch()` or asking again gets the dense lane once
    it's embedded.
  - `useResolve(names)`, and `useDeclaration(name, module?, { version? })`,
    where `module` is a snapshot hit's, to open that snapshot's declaration.
  - `useDebouncedValue(value, ms)` debounces typed input.

**Without a hook.** Environments, run names, settling a run, resubmitting a
run onto an amended spec, flagging a description and reading one acceptance
by id have no hook. Call the client from `usePisaClient()`, in a query of
your own or a long job:

```tsx
import { useQuery } from "@tanstack/react-query";
import { retryQuery, useLongJob, usePisaClient } from "@aqora/pisa-react";

function Environments() {
  const client = usePisaClient();
  const environments = useQuery({
    queryKey: ["environments", client.url],
    queryFn: ({ signal }) => client.listEnvironments({ signal }),
    retry: client.retry === undefined ? retryQuery : false,
  });
  const settle = useLongJob((c, against: string[], o) => c.submitSettleRun(against, o));
  // settle.mutate([specId]); settle.job?.status
  return <ul>{environments.data?.environments.map((e) => <li key={e.version}>{e.version}</li>)}</ul>;
}
```

**Query options.** `healthOptions`, `checkOptions`, `checkBatchOptions`,
`searchOptions`, `resolveOptions`, `declarationOptions`, `snapshotOptions`,
`dependentsOptions`, `auditOptions`, `acceptancesOptions`,
`rebuildPlanOptions` and `documentGoalsOptions` give the options the hooks
use, and `snapshotsOptions` the infinite query's, each from the client and
the hook's arguments, for `useQueries` or prefetching. Keys start with
`["pisa", url]`, so servers don't share a cache.

**Caching and retries.**
- A check is cached for good, unless it failed in a way that may not recur.
- Searches over snapshots go stale after 30 s.
- The client sends requests again itself (see its `retry`), so queries
  aren't run again on top of it. A client with `retry: false` falls back on
  `retryQuery`: once after `unavailable` (a 503) and twice after a network
  error, and never after a 429, which already means the server waited for a
  worker. `retryQuery` is exported for queries of your own.
- Mutations aren't run again; the client's own retries cover them.

The [client documentation](https://github.com/aqora-io/pisa/blob/main/docs/clients.md)
covers the TypeScript client and the others.
