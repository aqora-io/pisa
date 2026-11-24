# HTTP API

This page is the reference for pisa's HTTP API, for anyone writing a client
or calling it directly: its conventions, every endpoint, scopes, async
requests and jobs, errors, and what to do about a lost job. The
[clients](clients.md) wrap all of it. What the endpoints do is explained in
[Checking code](checking.md), [Snapshots](snapshots.md), [Runs](runs.md),
[Acceptance](acceptance.md) and [Search](search.md).

## Conventions

- **JSON.** Requests and responses are JSON, and every field is
  snake_case. The types are in `crates/pisa-protocol/src/api.rs` and
  `search.rs`.
- **Absent, not null.** The server leaves an optional field out rather
  than sending `null`, and always sends a field that can be `null`.
- **Code** is a Lean file: plain, or module-system (a `module` header with
  `public`, `meta` and `all` imports).
- **Ids carry their version.** Snapshot, document, session and acceptance
  ids are `<version>~<32 hex>`, and a state's is
  `<version>~<session>~<state>`, so the requests that take one take no
  `version`. Job ids are 32 hex digits.
- **`version`** selects a worker pool; see [Versions](#versions).
  **`timeout`** is Lean's limit in seconds, 60 unless given.
- **Errors** are `{"error": {"kind", "message"}}`, with a status that
  follows the kind; see [Errors](#errors).
- **`Pisa-Scope`** keeps a request to one tenant; see [Scopes](#scopes).
- **`Prefer: respond-async`** runs a long request as a job; see
  [Async requests and jobs](#async-requests-and-jobs).
- **CORS.** Browsers may call the API from other origins only when the
  server allows them: `PISA_CORS_ORIGINS` (`--cors-origin`, the chart's
  `server.corsOrigins`) takes a comma-separated list of origins, or `*`.
  It is empty by default. See [Server settings](configuration.md#server).

## OpenAPI

The server describes its API and search as OpenAPI 3.1 at
`GET /openapi.json`, and the same document is committed as `openapi.json`.
They are the complete schema: every endpoint, parameter, request and
response field, and error, with a description of each. The schemas are
derived from the Rust types; [Development](development.md) says how to
regenerate it and the TypeScript types made from it.

## Endpoints

| Method | Path | What it does | See |
| --- | --- | --- | --- |
| `POST` | `/check` | Check code | [Checks and batches](checking.md#checks-and-batches) |
| `POST` | `/check/batch` | Check up to 128 pieces of code | [Checks and batches](checking.md#checks-and-batches) |
| `POST` | `/sessions` | Start a tactic session on code with `sorry`s | [Tactic sessions](checking.md#tactic-sessions) |
| `POST` | `/states/{id}/tactic` | Run a tactic on a state | [Tactic sessions](checking.md#tactic-sessions) |
| `POST` | `/sessions/{id}/check` | Check a session's finished proof | [Tactic sessions](checking.md#tactic-sessions) |
| `DELETE` | `/sessions/{id}` | Close a session | [Tactic sessions](checking.md#tactic-sessions) |
| `POST` | `/documents` | Open code on Lean Beam | [Documents](checking.md#documents) |
| `PATCH` | `/documents/{id}` | Replace a document's code | [Documents](checking.md#documents) |
| `POST` | `/documents/{id}/goals` | The goals at positions | [Documents](checking.md#documents) |
| `POST` | `/documents/{id}/probes` | Try candidate tactics at a position | [Documents](checking.md#documents) |
| `DELETE` | `/documents/{id}` | Close a document | [Documents](checking.md#documents) |
| `POST` | `/snapshots` | Keep code as a snapshot | [Snapshots](snapshots.md) |
| `GET` | `/snapshots` | List snapshots by label | [Labels](snapshots.md#labels) |
| `GET` | `/snapshots/{id}` | A snapshot, with its code, check and exports | [Snapshots](snapshots.md) |
| `PATCH` | `/snapshots/{id}` | Set or remove a snapshot's labels | [Labels](snapshots.md#labels) |
| `DELETE` | `/snapshots/{id}` | Delete a snapshot | [Retention](snapshots.md#retention) |
| `POST` | `/snapshots/{id}/verify` | Re-check a snapshot's module with the kernel | [Verifying](snapshots.md#verifying) |
| `GET` | `/snapshots/{id}/dependents` | The snapshots that build on a snapshot | [Rebuilding](runs.md#rebuilding-dependents) |
| `POST` | `/snapshots/rebuild` | Rebuild dependents on a replacement | [Rebuilding](runs.md#rebuilding-dependents) |
| `POST` | `/snapshots/settle` | Rebuild a run's heads onto what their targets hold through | [Settling](runs.md#settling-a-run) |
| `POST` | `/snapshots/resubmit` | Carry a run over to the spec that amends its spec | [Amending](runs.md#amending-a-spec) |
| `GET` | `/snapshots/audit` | What snapshots rest on, or a run against its spec | [Auditing](runs.md#auditing-a-run-against-its-spec) |
| `GET` | `/snapshots/export` | A run as a Lake project, or a pair as a Palomar package | [Acceptance](acceptance.md) |
| `POST` | `/snapshots/accept` | Judge a run with comparator and store the verdict | [Acceptance](acceptance.md) |
| `GET` | `/snapshots/accept/{id}` | A stored acceptance | [Acceptance](acceptance.md) |
| `GET` | `/snapshots/accept` | A spec's stored acceptances | [Acceptance](acceptance.md) |
| `GET` | `/runs/{name}` | A run by name | [Naming a run](runs.md#naming-a-run) |
| `PUT` | `/runs/{name}` | Name a run | [Naming a run](runs.md#naming-a-run) |
| `POST` | `/search` | Search declarations | [Search](search.md#api) |
| `POST` | `/resolve` | Check that names exist | [Search](search.md#api) |
| `GET` | `/declarations/{name}` | A declaration's details | [Search](search.md#api) |
| `POST` | `/declarations/{name}/flags` | Flag a declaration's description | [Search](search.md#api) |
| `GET` | `/jobs/{id}` | An async request's answer | [Async requests](#async-requests-and-jobs) |
| `DELETE` | `/jobs/{id}` | Cancel an async request | [Async requests](#async-requests-and-jobs) |
| `GET` | `/environments` | Every version a worker has registered | [`GET /environments`](#get-environments) |
| `GET` | `/health` | Whether the server takes requests, and its workers | [`GET /health`](#get-health) |
| `GET` | `/openapi.json` | This API as OpenAPI 3.1 | [OpenAPI](#openapi) |

The blocks below give each endpoint's request and response in short, a `?`
marking what may be absent. `openapi.json` has every field.

### Checks

- `POST /check`: `{code, version?, timeout?, detail?, at?, against?}` →
  a [check result](checking.md#the-check-result). Async with `Prefer`.
- `POST /check/batch`: `{items: [{code}], version?, timeout?, detail?,
  at?}`, up to 128 items, 32 queued at a time → `{results: [...]}`, a check
  result per item, in order. It answers at once whatever `Prefer` says.

### Sessions

- `POST /sessions`: `{code, version?, timeout?}`, code with `sorry`s →
  `{id, states: [{id, goals, pos, end_pos}], messages, runs?, timing}`.
  Async with `Prefer`.
- `POST /states/{id}/tactic`: `{tactic, timeout?}` → `{state: {id, goals},
  complete, messages, reopened, replayed, timing}`. A tactic that fails is
  a 422 `lean`. Async with `Prefer`.
- `POST /sessions/{id}/check`: `{code, timeout?, detail?}` → a check
  result, which also requires the session's original statements and
  imports. Async with `Prefer`.
- `DELETE /sessions/{id}` → 204. It closes the session's document, and its
  states then answer 404. `DELETE /documents/{id}` on the session's id is
  the same.

### Documents

- `POST /documents`: `{code, version?, timeout?}` → `{id, revision,
  messages, sorries: [{goal, pos?, end_pos?}], runs?, timing}`, each run
  with `moved_to?`.
- `PATCH /documents/{id}`: `{code, revision, timeout?}` → the document at
  its next revision; a 409 `conflict` if it is at another than `revision`.
- `POST /documents/{id}/goals`: `{at: [{line, column?}], revision?, mode?,
  timeout?}`, 1 to 64 positions, `mode` `before` (the default) or `after` →
  `{revision, at: [{pos, line, in_proof, goals}], reopened, timing}`.
- `POST /documents/{id}/probes`: `{at: {sorry} or {line, column?},
  candidates, revision?, timeout?, budget?}`, 1 to 64 candidates →
  `{revision, pos, results: [{ok, closed, goals, messages, error?}],
  reopened, timing}`.
- `DELETE /documents/{id}` → 204.

Documents answer synchronously only: they don't take `Prefer`.

### Snapshots

- `POST /snapshots`: `{code, version?, timeout?, verify?, labels?, key?,
  replaces?, fork?, holes?, amends?, refutes?, name?, detail?}` →
  `{snapshot?, refused?, check, cached, timing, replacement?, heads?,
  unverified?, amendment?, refutation?, refutations?, runs?}`. Async with
  `Prefer`. See [Snapshots](snapshots.md),
  [Replacing a snapshot](snapshots.md#replacing-a-snapshot),
  [Amending](runs.md#amending-a-spec), [Refuting](runs.md#refuting-a-target)
  and [Naming a run](runs.md#naming-a-run).
- `GET /snapshots`: `?label=k:v` (repeated), `limit?`, `after?`, `heads?`,
  `version?` → `{snapshots: [{id, version, module, header, imports, labels,
  replaces?, holes?, status, verified, created, run?}], next?}`, newest
  first.
- `GET /snapshots/{id}`: `?detail=declarations` → the snapshot as in a
  listing, with `code`, `source?`, `check` and `exports`.
- `PATCH /snapshots/{id}`: `{labels: {key: value or null}}` → the snapshot
  as in a listing.
- `DELETE /snapshots/{id}`: `?with_replacers=true` (`false` by default) →
  `{removed, deleted}`: the modules that went, and the snapshots deleted,
  the asked one first.
- `POST /snapshots/{id}/verify`: a JSON body `{timeout?, again?}`, which
  may be left out or `null` → `{snapshot, checked, failure?: {module, kind,
  reason}}`. `timeout` is seconds per module once its imports are loaded,
  60 unless given; `again` (`false` unless given) re-checks modules that
  are verified already. Query parameters are not read. Async with `Prefer`.
- `GET /snapshots/{id}/dependents`: `?label=k:v` (repeated) →
  `{snapshots: [...]}` as in a listing, parents first.
- `POST /snapshots/rebuild`: `{from, to, labels, dry_run?, allow_changes?,
  retire?, verify?, timeout?}` → `{ok, changes, changed_definitions?,
  plan: [{level, snapshot_id?, module, new_module, new_header, code,
  labels, key, replaces?, affected?, source?}], replaced?, superseded?,
  created?, failed?, skipped?, not_deleted?, retired?}`. Async with
  `Prefer`, unless a dry run.
- `POST /snapshots/settle`: `{against, allow?, dry_run?, verify?,
  timeout?}` → as `POST /snapshots/rebuild`. Async with `Prefer`, unless a
  dry run.
- `POST /snapshots/resubmit`: `{from, to, dry_run?, labels?, verify?,
  timeout?}` → `{ok, items: [{snapshot_id, module, key, passes, reasons?,
  changes?, replacement?, new_snapshot_id?, check?}]}`. Async with
  `Prefer`, unless a dry run.

### Audits, exports and acceptances

- `GET /snapshots/audit`: `?label=k:v` (repeated), `version?` →
  `{summary: {proved, conditional, sorry, rejected, unverified_modules},
  assumptions: [{name, signature?, snapshot_id?, verified, used_by}],
  not_proved: [{name, snapshot_id, status, axioms}]}`. Or, for a run,
  `?against=<id>` (repeated), `allow=<name>` (repeated), `review?`,
  `review_limit?` → the same, each assumption with `in_spec`, and `clean`,
  `heads`, `forks?`, `targets?`, `links?`, `comparisons?`, `review?`,
  `review_left_out?`, `unverified?`, `amendments?`, `superseded?`,
  `refutations?` and `unsettled?`. Each target is `{name, spec, head,
  status, verified, accepted, refuted_by?, contradicted?, linked_status?,
  through?, unlinked?}`. See
  [Auditing a run](runs.md#auditing-a-run-against-its-spec).
- `GET /snapshots/export`: `?against=<id>` (repeated), `allow=<name>`
  (repeated) → the run as a Lake project, a tar.gz; with
  `profile=palomar`, `pair=<n>`, `challenge_module` and `solution_module`,
  one pair as a Palomar package.
- `POST /snapshots/accept`: `{against, allow?, paranoid?, timeout?}` →
  `{id, created, against, allow?, paranoid, accepted, clean, heads, audit,
  truncated?, pairs: [{spec, head, spec_module, head_module, config,
  accepted, error?: {kind, message}, exit_code?, kernels?, output,
  theorems, left_out?, phases?}]}`. Async with `Prefer`: a Mathlib-sized
  pair takes about half a minute, or a minute paranoid.
- `GET /snapshots/accept/{id}` → the stored result of
  `POST /snapshots/accept`, with `stale?`.
- `GET /snapshots/accept`: `?against=<id>`, a spec snapshot →
  `{acceptances}`: the stored results that judged it, newest first, at
  most 100, each with `stale?`.

### Runs

- `GET /runs/{name}`: `?version=` → `{name, root, heads, module?,
  snapshot_id?}`: the run, and what `import PisaRun.<name>` resolves to
  when its heads are on one module.
- `PUT /runs/{name}`: `{snapshot_id}` → as `GET /runs/{name}`. It names the
  run the snapshot is in.

### Search

- `POST /search`: `{queries: [...], version?}` → `{version, results:
  [{hits, ...}]}`, a result per query, in order.
- `POST /resolve`: `{names, snapshots?, version?}` → `{version, results:
  [{name, exists, ...}]}`.
- `GET /declarations/{name}`: `?module=`, `?version=` → its signature,
  description, flags, dependencies and users.
- `POST /declarations/{name}/flags`: `{reason}` → `{version,
  description_flags}`.

A server searches only when it is given Qdrant and a version
(`PISA_SEARCH_QDRANT` and `PISA_SEARCH_VERSION`). On a server without
search, these four endpoints answer 404 `not_found` ("this server doesn't
search"). On a server with search, they answer 503 `unavailable` until the
index is open in Qdrant, while the rest of the API works, and `/health`'s
`search.error` says why. A request that names another version than search
covers is a 404 `not_found`. See [Search](search.md#api).

### Jobs

- `GET /jobs/{id}`: `?wait=` (seconds, at most 60, 0 by default) → once
  its job is done, 200 `{id, kind, status: "done", http_status, body}`,
  the request's status and answer; until then 202 `{id, kind, status,
  progress?}`, where `progress` is `{phase: {name, pair?, side?}, kernel?,
  seconds, peak_memory_mb?}` for a run's acceptance (see
  [Progress](acceptance.md#progress)).
- `DELETE /jobs/{id}` → `{id, kind, status}`, `cancelled` if no worker had
  started it.

See [Async requests and jobs](#async-requests-and-jobs).

### GET /environments

`GET /environments` → `{environments: [{version, toolchain, packages:
[{name, url?, rev, input_rev?}], files, created, pool?}]}`: every version a
worker has registered, by version.
- `toolchain` and `packages` are what its project pins, as `/health`'s
  pools have them.
- `files` has the project's files by name: `lean-toolchain`, the lakefile,
  the manifest and the top-level `.lean` files. Written to a directory they
  are the project again, which builds a worker image of the same version.
- `created` is when a worker first registered it, in seconds since the Unix
  epoch.
- `pool` has its live workers, as `/health` lists them, and is absent while
  it has none.

An environment stays listed after its workers are gone, as its snapshots
stay, so what a snapshot's version was can be read after its pool is scaled
to zero or removed. Workers started with a version of their own choosing
(`PISA_VERSION`) list what the latest of them serves. `/environments`
ignores `Pisa-Scope`. See
[Listing environments](environments.md#listing-environments).

### GET /health

`GET /health` is a readiness check: it says whether this server takes
requests. It answers 200 while the server is up, whatever the state of the
database, the store or search, since a dependency that is down fails the
requests that need it, not the server. Once the server is stopping it
answers 503 with `status: "draining"`, so that load balancers stop sending
it requests while it finishes those it has. Use it as a readiness probe,
not a liveness probe: a liveness probe would fail during that drain and
kill the server before it finished. It answers within about 2 seconds even
while the database or Qdrant stalls.

The body is `{status, workers, queued, unlimited_workers, sweeps_refused,
revision, worker_revisions, pools, dependencies, hosts, search}`:
- `workers` and `queued`: the live workers on the queue, and the jobs
  waiting for one.
- `unlimited_workers`: the live workers that run Lean without memory or
  process limits, by name.
- `sweeps_refused`: the versions whose sweeps of the store are refused,
  `[{version, reason, since}]` (see [Retention](snapshots.md#retention)).
- `revision` and `worker_revisions`: the server's commit (`unknown` if its
  build didn't say) and the distinct commits of the live workers.
- `pools`: the live workers by the version they serve, `[{version, workers,
  slots, beam_slots, draining, toolchain, packages}]`.
- `dependencies`: `{database, store}`, each `ok`, `unreachable` or
  `unknown`. The database is `ok` when it answers within 2 seconds; the
  store is as the live workers last probed it, about every minute.
- `hosts`: the live workers' memory limits per host, `[{boot_id, workers,
  uncapped_workers, memory_caps_mb, memory_mb, overcommitted}]`.
- `search`: `{ready, version, qdrant, leader, describe_model, error?}`, or
  `null` on a server without search. `describe_model` is the model that
  snapshot code is sent to, `null` if none (see
  [What snapshot search sends](search.md#what-snapshot-search-sends-to-a-model)).

Each field that reads the queue is `null` if it can't be read. See
[Health](operations.md#health).

## Versions

`version` selects a worker pool, by its label, such as
`v4.36.0-54500a67`, or by an alias that `PISA_ALIASES` sets, such as
`physlib=v4.36.0-54500a67`. A request that names none goes to the server's
default, `PISA_DEFAULT_VERSION`, and with no default to whichever pool takes
it. A check's result says which version ran it, in `version`. Reads that
name no version, such as listings, label audits and runs by name, read the
default, or else the one version the live workers serve, and ask for one
when there are several.

A version names an environment, a Lean toolchain and the Lake project its
workers build; see [Versions and environments](concepts.md#versions-and-environments)
and [Reaching a pool](environments.md#reaching-a-pool).

## Scopes

A request may send `Pisa-Scope: user=42,org=7`: labels that keep it to one
tenant, so that tenants can share a server.
- **The header** is comma-separated `key=value` pairs, each key once, at
  most 64, keys and values as for [labels](snapshots.md#labels) with no `,`
  in a value. A malformed one is a 400 on every endpoint, `/health`
  included. No header, or an empty one, is no scope, which bounds nothing.
- **What it creates** has the scope's labels: snapshots, those a rebuild, a
  settle or a resubmit copies included, documents, sessions, jobs and
  acceptances. A create's `labels` that give a scope key another value keep
  nothing (`bad_request` in `check.error`), and `PATCH /snapshots/{id}` that
  changes or removes one is a 400.
- **What it reads and names** must have them, or it is `not_found`, as for
  what doesn't exist: snapshots by id, replaced, amended or refuted ones,
  dependents, listings and label audits (the scope is added to their
  filters), runs, their audits, exports, settles, rebuilds, resubmits and
  acceptances, documents, sessions and their states, and jobs by id.
- **Imports.** Code in a scope imports a snapshot module only if a snapshot
  in the scope is on it, else `not_found` as for an unknown snapshot, and
  names resolve to the scope's runs. A module's own imports come with it.
  No code imports an open document's module, `PisaDocs.…` (400).
- **Keys and names** are the scope's own: an idempotency key and a run name
  are unique in a scope, so two tenants can use the same ones.
- **Search** keeps snapshot hits to the scope (see
  [Snapshots in search](search.md#snapshots-in-search)), and
  `GET /declarations/{name}` with `module=` is `not_found` for a module no
  snapshot of the scope is on. The library's declarations are everyone's,
  and so are description flags: a flag made in a scope carries none, and
  counts in every scope's hits.
- **What it doesn't bound.** `/health`, `/openapi.json` and `/environments`
  ignore the scope.
- **Not built:** fairness between scopes, caps and metrics per scope, and
  listing or deleting a scope's documents, sessions and jobs.

## Async requests and jobs

A check, a session's start, tactic or check, a snapshot's create or verify,
a rebuild, a settle, a resubmit, and a run's acceptance can take minutes: a
wait for a worker, loading the imports, Lean's `timeout`, a kernel
re-check. Behind a proxy that cuts requests off at 60 s, or over a network
that drops, send them with `Prefer: respond-async`. The server then answers
202 `{id, kind, status}` at once, with `Location: /jobs/{id}`, and runs the
request as it would have. `status` is `queued` for a job a worker runs, and
`running` for a rebuild, a settle or a resubmit, which the server runs
itself, sending each of its creates to a worker.

- **Waiting for it.** `GET /jobs/{id}?wait=30` waits up to `wait` seconds
  (at most 60) and, once the job is done, answers 200 `{id, kind, status:
  "done", http_status, body}`: the status and the answer the request would
  have had, errors included, e.g. an `http_status` of 429 when no worker of
  its version is on record, or 503 `worker_lost` when the job was lost (see
  [When a worker is lost](#when-a-worker-is-lost)). Until then it answers
  202 with `status` `queued` or `running`, and for a run's acceptance
  `progress`, what its worker last said it was doing (see
  [Progress](acceptance.md#progress)).
- **Waiting for a worker.** While every worker that could run it is busy,
  or none is live, e.g. across a restart, the job waits for a slot, up to a
  day. A sync request on a busy pool gets a 429 after
  `PISA_MAX_WAIT_SECS`, so send long builds and acceptances async.
- **The look's own errors.** `GET /jobs/{id}` answers within `wait` and five
  seconds even if the database does not: the last status it read, else a
  503. A 404 or a 503 is the look's own, never the job's. Any server
  answers for any job. The clients look every 25 s, well short of a
  proxy's 60.
- **How long it is kept.** An answer is kept for an hour after its job is
  done, a snapshot's create and a run's acceptance for a day; then the job
  is a 404. For 30 days after that, asked in the scope it was made in, the
  404 says what the job was and how it ended (`expired: a create_snapshot
  that answered 200 at …`, `was given up at …, as no worker took it`, `was
  lost at …`, or `was cancelled at …`), never its answer; see
  [Recovering a lost job](#recovering-a-lost-job). In any other scope, and
  for an id that never was, it says `unknown job`.
- **Cancelling.** `DELETE /jobs/{id}` withdraws a job no worker has
  started (`cancelled`); a `running` or `done` one stays as it is.
- **Keep the id secret.** Anyone with a job's id can read its answer or
  cancel it. Build its URL from `id` rather than `Location`, which is
  relative to the server's root.
- **What answers at once.** `/check/batch`, documents, the snapshot reads
  and edits, dry runs, and a settle whose plan is refused answer at once
  whatever `Prefer` says: send a batch's items to `/check` instead.

### Job kinds

A job's `kind` names the endpoint it runs, and says what its `body` is once
it is done with an `http_status` of 200. With any other status, `body` is
the request's error.

| `kind` | Endpoint | Run by | `body` when 200 | Answer kept |
| --- | --- | --- | --- | --- |
| `check` | `POST /check` | a worker | a check result | an hour |
| `start_session` | `POST /sessions` | a worker | the session | an hour |
| `apply_tactic` | `POST /states/{id}/tactic` | a worker | the tactic's answer | an hour |
| `check_session` | `POST /sessions/{id}/check` | a worker | a check result | an hour |
| `create_snapshot` | `POST /snapshots` | a worker | the create's answer | a day |
| `verify_snapshot` | `POST /snapshots/{id}/verify` | a worker | the verify's answer | an hour |
| `rebuild_snapshots` | `POST /snapshots/rebuild` and `POST /snapshots/settle`, not dry runs | the server | the rebuild's answer | an hour |
| `resubmit_snapshots` | `POST /snapshots/resubmit`, not a dry run | the server | the resubmit's answer | an hour |
| `accept_run` | `POST /snapshots/accept` | a worker | the acceptance | a day |

A settle's job is a `rebuild_snapshots`: its answer has a rebuild's shape.

## Errors

Every error is `{"error": {"kind", "message"}}`, and some carry more:
`limit` for `out_of_memory`, `recursion_limit` and `heartbeat_limit`,
`timing` for a document that didn't open, and `resend_safe` for
`worker_lost`.

- **Checks report failures in-band.** A check that times out, crashes or is
  refused by the policy still answers 200, with `ok: false` and the
  `error`. Batch items work the same way, so one failed item never fails a
  batch, and so does a session's check, but for an unknown or malformed
  session. So does a create: one that keeps nothing says in `refused` what
  to do next (see [Snapshots](snapshots.md)), a worker that failed it
  included.
- **Other requests use HTTP statuses,** which follow the kind.

### Error kinds

The clients raise each kind as the class or predicate below. In Rust every
kind is an `Error::Server` with its `kind`, `status`, `message` and
`resend_safe`.

| `kind` | Status | Meaning | Python | TypeScript |
| --- | --- | --- | --- | --- |
| `lean` | 422 | Lean refused the input, e.g. an unknown tactic or a header that doesn't elaborate | `LeanError` | `isLeanError` |
| `policy` | 422 | The [verification policy](checking.md#the-verification-policy) refused the input before it ran | `LeanError` | `isLeanError` |
| `timeout` | 422 | Lean ran past the request's `timeout` | `LimitError` | `isLimitError` |
| `out_of_memory` | 422 | Lean passed its memory limit; `limit` has the setting and its value in MiB | `LimitError` | `isLimitError` |
| `crash` | 422 | The Lean process died | `LimitError` | `isLimitError` |
| `recursion_limit` | 422 | Lean reached `maxRecDepth`; `limit` has the option, its value and the phase. The input fails as it is, so don't retry it | `LimitError` | `isLimitError` |
| `heartbeat_limit` | 422 | Lean used up `maxHeartbeats`; as for `recursion_limit` | `LimitError` | `isLimitError` |
| `capacity` | 429 | No worker took the job within `PISA_MAX_WAIT_SECS`; retry later | `CapacityError` | `isCapacityError` |
| `unavailable` | 503 | The database failed or stayed contended, no worker serves documents, or search isn't ready; retry later | `CapacityError` | `isCapacityError` |
| `worker_lost` | 503 | The job was lost; `resend_safe` says whether to send it again (see [When a worker is lost](#when-a-worker-is-lost)) | `CapacityError` | `isCapacityError` |
| `bad_request` | 400 | The request breaks the API's rules; sent again as it is, it fails again | `BadRequestError` | `isPisaError(e, "bad_request")` |
| `not_found` | 404 | An unknown or expired job, document, session or state; an unknown snapshot, run, declaration or endpoint; or one outside the request's scope | `NotFoundError` | `isNotFoundError` |
| `conflict` | 409 | What the request names has moved on; see [Conflicts](#conflicts) | `ConflictError` | `isConflictError` |
| `internal` | 500 | A bug | `InternalError` | `isPisaError(e, "internal")` |

Every Python class is a `PisaError`, which is also what Python raises, with
`kind` and `status` `None`, when the server can't be reached. TypeScript's
`PisaError.kind` may also be `network` (no answer), `aborted` (the
request's signal fired) or `bad_response` (an answer that isn't pisa's).
All three clients read a gateway's 502, 503 or 504, whose body isn't
pisa's, as `unavailable`.

A 429 or a 503 carries `Retry-After`, in seconds. The clients send a
request again by themselves (see [Retries](clients.md#retries)).

### Conflicts

A `conflict` means the request was right when it was written, but what it
names has moved on since. Each says in its message what to do instead.
- **A create that replaces a snapshot another replaces already**, without
  `fork`: in-band, in `check.error`, with `refused: "conflict"` and `heads`,
  the heads to replace instead. A create that loses that race during its
  build is refused the same way, and its build is kept. See
  [Replacing a snapshot](snapshots.md#replacing-a-snapshot).
- **A create that replaces into a run whose spec was amended**, a fork
  included: in-band, with the current spec in the message and its run's
  heads in `heads`. See [Amending](runs.md#amending-a-spec).
- **A second amendment of a spec**: in-band, naming the current spec, which
  is the one to amend.
- **A run name another run has**, while that run has snapshots: in-band for
  a create's `name`, naming that run's first snapshot, and a 409 for
  `PUT /runs/{name}`. See [Naming a run](runs.md#naming-a-run).
- **An import by run name whose run is forked**, its heads on several
  modules: in `check.error` for a check, a batch and a create, and a 409
  for the other requests, naming the heads.
- **An acceptance of a run that isn't settled**, whose audit lists
  `unsettled` heads: a 409 before anything is judged, naming the settle to
  send first. Nothing is stored. See [Settling a run](runs.md#settling-a-run).
- **A rebuild's or a settle's item whose snapshot was replaced** after the
  plan was made: the item fails with a `conflict`, and sending the request
  again plans from the new head. See
  [Rebuilding](runs.md#rebuilding-dependents).
- **A document at another revision** than an edit, goals or probes name: a
  409. See [Documents](checking.md#documents).

## Lost jobs

### When a worker is lost

A job whose workers kept disconnecting, that no worker able to run it came
back for in time, or that a server ran itself and lost as it stopped,
answers 503 `worker_lost` with `Retry-After`, and `resend_safe`. Its
message says how many times it was claimed, and whether its attempts ran
out or no worker came back. An async job answers it as its `http_status`
and `body`, as it would any error. The clients' errors carry it:
`Error::Server`'s `resend_safe` in Rust, `PisaError.resend_safe` in Python
and `PisaError.resendSafe` in TypeScript.

| Request | `resend_safe` |
| --- | --- |
| Checks, sessions and their tactics and checks, verify, export, document open, goals, probes | `true` |
| Creates (replace, amend, refute) | `true`: a resend with the same `key` replays; the clients always send one |
| Rebuild, settle, resubmit | `true`: their creates carry the plan's keys |
| Delete, document close | `true`: a second run answers `not_found`, which means done |
| Accept | `true`: an acceptance is stored only with its job's answer, so a lost one stored nothing; a resend judges the run again, from the start |
| Document edit | `false`: check the document's revision first |

The clients send a lost request again by themselves when it is safe to
send twice, as they do after any 503 (see [Retries](clients.md#retries)):
checks, tactics, goals, probes, verifies and creates, and a batch's lost
items. An async job's lost answer is returned, not sent again, and so is
any other lost request: those are yours to resend, by this table.

### Recovering a lost job

An [async](#async-requests-and-jobs) job's answer is kept for an hour after
it is done (a day for a snapshot's create and a run's acceptance), a lost
job's too: until then `GET /jobs/{id}` answers 200 with an `http_status` of
503 `worker_lost`. If its client was away longer, `GET /jobs/{id}` is a 404
that says what the job was and how it ended, for 30 days, in the scope it
was made in only (the `Pisa-Scope` it was sent with). The answer itself is
never kept. What the job made is still there:
- **A create** sent again with the same `key` replays: it returns the
  snapshot the first one made. The clients always send a key. A create
  sent without one gets a key of the server's (`job:…`), which keeps the
  job's own retries from keeping it twice, but which the client doesn't
  know: sent again, it is a new create.
- **A create that named a run:** `GET /runs/{name}` returns the run and
  what `import PisaRun.<name>` resolves to, with the same `Pisa-Scope` the
  create was sent with.
- **An accept** is found with `GET /snapshots/accept?against=<id>`, which
  lists the spec's stored acceptances, newest first. Match yours on
  `created`, `against`, `allow` and `paranoid`. Don't send an accept again
  unless none matches: a finished first run may be stored already.
- **Anything else:** send it again, which is safe for every other async
  request (see [When a worker is lost](#when-a-worker-is-lost)). The 404
  doesn't carry `resend_safe`; only the lost job's own answer did.
