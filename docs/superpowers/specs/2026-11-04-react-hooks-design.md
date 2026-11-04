# React hooks for pisa, and a playground

## Context

pisa serves its whole API from one replicated `pisa-server`:
- checks;
- proof sessions (sorry → states → tactics);
- snapshots;
- search.

There is no browser client yet, and no server sends CORS headers.

**Goal (user's):** a React package whose core is reusable hooks for talking to a pisa server, and a Vite playground that uses it to demonstrate checks, a proof REPL, snapshots and search.

### Decisions made with the user
- **Location:** in this repo, as a pnpm workspace under `web/`.
- **Data layer:** hooks on TanStack Query v5, a peer dependency.
- **Browser access:** the playground proxies `/api` to the server in `vite dev`, and `pisa-server` gains an opt-in `PISA_CORS_ORIGINS` for apps on another origin.
- **Playground UI:** CodeMirror 6 and Tailwind 4.

### Facts this rests on
- **Errors.**
  - The error envelope is `{error:{kind,message}}`: 422 for `timeout`, `out_of_memory`, `crash`, `policy` and `lean`; 429 `capacity`; 503 `unavailable`; 400 `bad_request`; 404 `not_found`; 500 `internal`.
  - Check failures are in-band, in `CheckResult.error`, as are snapshot create failures, in `result.error`.
  - Not every error response is the envelope: `/health`'s 503 while draining, axum's 405s, and query rejections aren't.
- **Long requests.** A request waits up to `PISA_MAX_WAIT_SECS` for a worker, then runs. Aborting only gives up a job no worker has claimed, and idle workers claim jobs within milliseconds.
- **State keys** (`version~header_key~hash`) are content hashes. The same state can come back from another tactic or branch, and earlier states stay valid. A session is a tree whose nodes can't be keyed by state key.
- **Positions** count lines from 1 and columns from 0 in Unicode code points. Mathlib's `𝕜` and `𝓝` are outside the BMP, so these aren't JavaScript string indices.
- **Shapes and scopes.**
  - `verified` and `dirty` belong to a snapshot's module, so every entry on it shows the same values.
  - `next` is present exactly when a listing page is full.
  - `/v1/*` is a 404 on a server without search, and `/v1/decl` can't be pinned to an `env_id`.

## Design

### CORS
`--cors-origin` / `PISA_CORS_ORIGINS` (comma-separated, or `*`) adds a `tower_http` CORS layer over the whole API, search and `/health` included. It allows GET/POST/PATCH/DELETE, `content-type`, and a one-hour `max_age`. The chart's `server.corsOrigins` and compose pass it through.

### `@aqora/pisa-client` (no dependencies)
- **Types:** every request and response type, written by hand with the semantics as doc comments.
- **Drift guard.** A Rust test builds every response type twice, once with every optional field set and once with none. It also lists every enum variant, and writes `wire.fixtures.ts` as `… satisfies T`. `tsc` then catches drift; the test fails when the file is stale, and `PISA_UPDATE_FIXTURES=1` regenerates it.
- **`PisaClient`.**
  - It takes `{url, version?, timeout?, envId?, fetch?, headers?}` and has a method per endpoint, each taking `{signal?, version?, timeout?, envId?}`.
  - URLs are joined as strings, so a relative base such as `/api` works, and every path segment is encoded.
  - `version` goes in the body or the query, as in the Rust client.
- **`PisaError`.** Its `kind` is an `ErrorKind`, `network`, `aborted` or `bad_response`. `isPisaError` checks `name` rather than `instanceof`, and there are groups as in the Python client: lean, limit, capacity, not found.
- **`posMapper(code)`** converts Lean positions to and from string offsets, clamping out-of-range ones.
- **`proofText(code, roots)`** puts tactic paths in place of sorries, adding `by` for a term-mode sorry.

### `@aqora/pisa-react`
- **Setup.** `PisaProvider({client})` supplies the client; the app supplies the `QueryClient`.
- **`queryOptions` factories.** Keys start with `["pisa", url]` and end with the request body.
  - Checks stay fresh forever, except transient in-band failures (`timeout`, `out_of_memory`, `crash`, `internal`).
  - Search stays fresh forever, except for 30 s when snapshots are in scope.
- **Retries:**
  - none for 4xx, 422, 429 or 500;
  - one for 503;
  - two for network errors, on queries only;
  - mutations aren't retried, except snapshot creates, which carry a fresh idempotency key.
- **Checks and health:**
  - `useHealth`.
  - `useCheck(request | null)` is a query, so "Stop waiting" can cancel it. It also returns `checkedCode`, for diagnostics.
  - `useLiveCheck(code)` debounces and keeps at most one check in flight.
- **`useProofSession`** has its own reducer.
  - Nodes have counter ids and are `pending`, `ok` or `failed`, with branches running in parallel.
  - A 422 marks a node failed rather than failing the hook, and a `not_found` means the state expired.
  - Responses that arrive after a reset are dropped, and requests are aborted on reset and unmount.
- **Snapshots:** `useSnapshots` (an infinite query), `useSnapshot`, and mutations that invalidate every snapshot query.
- **Search:** `useSearch`, `useResolve`, `useDecl`, and `useDebouncedValue`.

### Playground
- **Stack:** Vite, React and Tailwind, with a CodeMirror `LeanEditor` (a small Lean tokenizer, and lint diagnostics drawn only for the code that was checked).
- **Layout:** a health badge, then tabs kept mounted with `<Activity>`:
  - **Check:** run or live, diagnostics, verdict, the goal at the cursor, and "Stop waiting";
  - **Prove:** start, a state tree with branching, and checking the assembled proof;
  - **Snapshots:** create, list by label, labels, verify, delete, and "use header";
  - **Search:** only when the server searches. Queries, hits, declaration details, and "did you mean".

### Not in scope
- Serving the playground from `pisa-server`, or as a compose or Helm service.
- A list of versions in `/health`.
- A JavaScript formatter or linter.

## Verification
- **Rust:** the fixture test, and the CORS test.
- **Web unit tests:**
  - **Client:** URLs, errors, health on a 503, positions with `𝕜`, and `proofText`.
  - **React:** the proof reducer (duplicate keys, reset), at most one live check in flight, and snapshot invalidation.
- **Integration:** an integration suite against `pisa-server` and a fake-repl worker, which checks each request field by its effect; search only when `PISA_TEST_QDRANT_URL` is set.
- **The playground by hand,** driven with Playwright against compose with lean-core workers.
