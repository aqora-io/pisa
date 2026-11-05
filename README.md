# pisa

A Rust control plane for serving Lean 4 to machines. It runs unmodified
upstream [Lean REPL](https://github.com/leanprover-community/repl) processes.
Servers, as many as you run, queue requests in the database, and each job goes
to a worker that has the right project version and, ideally, the right imports
already loaded.

This repository implements stages 1–3 of the design doc *Building a Scalable
Lean 4 Server in Rust*:

1. **Servers and workers.** A server puts each request's job on a queue in
   the database, with two priority classes.
   Workers claim jobs, preferring those whose proof state they hold or whose
   header they have warm, and a job whose worker dies runs on another. Servers
   hold no state, so they run in replicas.
2. **Hardened workers.**
   - Each REPL runs in a bubblewrap sandbox and its own cgroup v2 limits.
   - Proofs are judged by the axioms they depend on, not by string matching.
   - Metaprogramming and unsafe options are rejected before the code runs.
   - Worker pools are labelled by version.
3. **Tactic sessions.**
   - Proof states are pickled into object storage (S3, or Garage in Compose),
     and their records and the sessions are kept in PostgreSQL, so any worker
     can continue a session.
   - A final check re-runs the whole proof from scratch and confirms it proves
     the original statements, with the original imports.

On top of these, [snapshots](#snapshots) let a client build a library
incrementally: accepted code is kept as a compiled module, and later code
imports it instead of re-checking it.

## Layout

| Crate | Role |
| --- | --- |
| `pisa-protocol` | The HTTP API types, REPL protocol types, header splitting, the jobs and results that go through the queue |
| `pisa-repl` | Async driver for one REPL process: blank-line framing, timeouts, crash reporting |
| `pisa-worker` | Slot pool cached by import header, process recycling, sandbox, cgroups, verification, sessions, state store, claiming jobs |
| `pisa-db` | The database (PostgreSQL, or SQLite for tests and single-process runs), its migrations, the job queue and the workers' registry, and the snapshot catalog, shared by the servers and the workers |
| `pisa-server` | The server: `pisa-router`'s API and `pisa-search`'s in one process, with a combined `/health` and graceful shutdown |
| `pisa-router` | The check, session and snapshot API: queueing jobs and waiting for their results |
| `pisa-client` | Async Rust client for the HTTP API |
| `pisa-python` | Python bindings for `pisa-client` (PyO3 + maturin), imported as `pisa_client` |
| `pisa-tests` | `fake-repl` (speaks the REPL protocol without Lean) and in-process integration tests |
| `pisa-search` | Declaration search over Mathlib and Physlib: corpus, descriptions, index, HTTP API, and the CLI that builds and loads bundles (see [Search](#search)) |

`web/` is a pnpm workspace with the TypeScript client, the React hooks and a
playground (see [Clients](#clients)).

## Running

```sh
docker compose up --build              # Lean v4.35.0 + Mathlib + Physlib (first build compiles Physlib)
PROJECT=lean-core docker compose up --build   # no Mathlib, for quick checks
python3 scripts/e2e.py [--mathlib] [--search]   # end-to-end checks against the running stack
cargo test --workspace                 # fast tests with fake-repl, no Lean needed
```

Compose is laid out like a deployment: Traefik on port 8000 (`PISA_PORT` to
change it) spreads requests over two servers, which it finds through Docker
and health-checks on `/health`, and two workers take jobs from PostgreSQL,
next to Garage for S3.
`--scale server=N` or `--scale worker=N` runs more of either. The servers and
the workers keep their data in two places, chosen by environment variables
that Compose and the chart set:
- **The job queue and the metadata**, the sessions, the records that make
  proof states loadable, and the snapshot entries and module records, are in
  a database, which every server and worker share. `PISA_DATABASE_URL` is
  a PostgreSQL URL (`postgres://user:password@host/db`) or the path of a
  SQLite file, which only one host can use. It is required.
  - Each process creates or updates the tables when it starts, one process
    at a time.
  - Jobs and workers are kept apart by `PISA_QUEUE` (default `pisa`), which
    the servers and their workers must agree on. Deployments that share a
    database need queues of their own.
  - PostgreSQL holds the only copy of the snapshot entries and module
    records, which never expire, so run it durably and back it up.
  - `sslmode=require` in the URL connects with TLS and checks the server's
    certificate against the system's roots; for a private CA, set
    `SSL_CERT_FILE` to its certificate. Other modes connect without TLS.
  - Each process opens at most 8 connections, plus one that `LISTEN`s for
    the queue's notifications. Connect directly, or through PgBouncer in
    session mode: the tables are created under a session-level advisory
    lock, and notifications need a session of their own.
  - A worker's claim on a job lasts `PISA_LEASE_SECS` (default 10) without a
    heartbeat; a job whose worker died runs elsewhere once it lapses, up to
    `PISA_MAX_RETRIES` more times.
  - The tests use in-memory SQLite, and also the PostgreSQL that
    `PISA_TEST_DATABASE_URL` names when it is set. Each test has versions of
    its own there, so tests can share one database.
- **Blobs**, the pickled proof states and snapshot modules, are in S3.
  `PISA_STORE` is `s3`, which reads the `AWS_*` variables, or `memory`.

The pinned versions live in `docker/lean-project`: `lean-toolchain`,
`lakefile.toml` and `lake-manifest.json`. Keep `REPL_REV` in `docker/Dockerfile`
on the same Lean version.

## Kubernetes

`deploy/helm/pisa` is a Helm chart containing:
- the server Deployment (`server.replicas`, 2), with its Service and a
  PodDisruptionBudget;
- the worker Deployment;
- a KEDA `ScaledObject`.

**Cluster prerequisites.** Like most application charts, it uses the cluster's
own KEDA, which reads the job queue from the database (see Scaling below).

**Storage.** The job queue and the metadata (sessions, proof-state records,
snapshot entries and module records) live in PostgreSQL, and blobs (pickled
proof states, snapshot modules) in an S3-compatible bucket.
- The chart doesn't deploy PostgreSQL. Put the URL of one you run, for
  example with [CloudNativePG](https://cloudnative-pg.io) or a managed
  service, in a Secret, and name it in `metadata.database.urlSecret.name`
  (`metadata.database.urlSecret.key` names the key, `url` by default). It is
  required, for the servers, the workers and KEDA. PostgreSQL holds the only
  copy of the snapshot entries and module records, which never expire, so it
  must be durable and backed up. The release's servers and workers share the
  queue `queue` (the release's name unless set).
- Give the bucket's credentials as an existing Secret, or leave
  `store.s3.credentialsSecret.name` empty to use the pod's ambient credentials
  (e.g. EKS IRSA).

```sh
helm repo add kedacore https://kedacore.github.io/charts
helm install keda kedacore/keda -n keda --create-namespace --version 2.21.0
kubectl create secret generic pisa-s3 \
  --from-literal=AWS_ACCESS_KEY_ID=... --from-literal=AWS_SECRET_ACCESS_KEY=...
kubectl create secret generic pisa-db \
  --from-literal=url='postgres://pisa:...@<host>/pisa?sslmode=require'
helm install pisa deploy/helm/pisa \
  --set server.image.repository=<registry>/pisa-server \
  --set worker.image.repository=<registry>/pisa-worker \
  --set metadata.database.urlSecret.name=pisa-db \
  --set store.s3.bucket=<bucket> --set store.s3.region=<region> \
  --set store.s3.credentialsSecret.name=pisa-s3
```

`store.s3.endpoint` and `store.s3.allowHttp` point the workers at a non-AWS
store such as MinIO or Garage. `credentialsSecret.accessKeyIdKey` and
`credentialsSecret.secretAccessKeyKey` name the keys inside the Secret.

**Scaling.** Workers scale on the release's outstanding jobs, queued plus
running, which KEDA's `postgresql` trigger counts in the job queue every
`keda.pollingInterval` seconds, through `metadata.database.urlSecret`.
- The target is `keda.jobsPerWorker` jobs per pod, which defaults to `worker.slots`.
- `keda.scaleDownStabilizationSecs` sets how long demand must stay low before pods are removed.
- The servers scale on their own: `server.replicas`, or `server.autoscaling`
  on CPU. Any replica serves any request, as the jobs are in the database.

**Draining.** A worker that receives SIGTERM claims no more jobs, finishes the ones it has, and exits. Two settings bound this:
- `worker.drainTimeoutSecs` is the longest it waits for running jobs.
- The pod's grace period is that plus 30s.

A server that receives SIGTERM answers `/health` with a 503 `draining` but
keeps taking requests for 5s, while its readiness probe (or Traefik's health
check, in Compose) takes it out of rotation, then waits up to
`server.drainTimeoutSecs` (360) for those in flight; its grace period is that
plus 30s. A request it cuts off still runs, as its job is in the queue.

Scale-downs and rollouts therefore don't cut jobs off.

**On minikube**, with the images built by `PROJECT=lean-core docker compose build`:

```sh
minikube start -p pisa --driver=docker --cpus=8 --memory=20g
helm repo add kedacore https://kedacore.github.io/charts
helm install keda kedacore/keda -n keda --create-namespace --version 2.21.0
minikube -p pisa image load pisa-server:latest pisa-worker:lean-core
kubectl apply -f deploy/minikube/garage.yaml   # a throwaway S3 store and its Secret
kubectl apply -f deploy/minikube/postgres.yaml # a throwaway PostgreSQL and its Secret
helm install pisa deploy/helm/pisa -f deploy/minikube/values.yaml
kubectl port-forward svc/pisa-server 8000:8000 &
python3 scripts/e2e.py --no-failover   # checks, policy, sessions against real Lean
python3 scripts/k8s_scale.py           # scales up under load, back down, drains on delete
```

**Search.** `search.enabled` has the servers search, over Qdrant: the official
chart as a dependency (`qdrant.enabled`, 3 nodes) or `search.qdrant.url`. They
search snapshots too, unless `search.snapshots` is false. Set `search.envId`,
and give API keys as an existing Secret (`search.secret.name`). Load each
bundle once, e.g. through `kubectl port-forward svc/<release>-qdrant 6333`,
with `pisa-search load --bundle search/bundle --qdrant http://localhost:6333 --replication 2`
(the server image has `pisa-search`). Until it has, `/v1/search` answers 503
and the rest of the API works.

**Not covered by the chart:**
- Separate pools per project version. The scaling query counts every job, so one release should serve one version.
- Auth on the API.
- A tighter security context: workers run privileged so bubblewrap and cgroup limits work.

## Upgrading

To the release with `replaces` on snapshot creates, upgrade the workers before
clients send `replaces`: an older worker refuses such a create, answering 500
that it may be older than the server, rather than keeping the code unchecked.
Snapshot entries gain two columns, which each process adds as it starts.

To the release with async requests, upgrade every server before clients send
`Prefer: respond-async`: older servers answer such requests at once, don't
know `/jobs`, and sweep async jobs' answers after 10 minutes. The job queue
gains two columns, which each process adds as it starts.

To the release with replicated servers, whose workers take jobs from the
database:
- `pisa-server` replaces `pisa-router` and `pisa-search serve`: one image
  (the Dockerfile's `server` target, which also has `pisa-search`) serves the
  check, session and snapshot API and search, on one port (8000; search was
  on 8100). Point `SearchClient` at the server's URL.
- Upgrade the servers and the workers together. Servers no longer listen for
  workers (`PISA_WORKERS`, port 7070), and workers no longer dial them
  (`PISA_ROUTER`): both need `PISA_DATABASE_URL`, the same database, and the
  same `PISA_QUEUE`.
- Search is on when `PISA_SEARCH_QDRANT` is set. It always has the database,
  so snapshots are searched unless `PISA_SEARCH_NO_SNAPSHOTS` is set. Until
  Qdrant answers with the bundle, `/v1/search` answers 503 instead of the
  process exiting, and `/health` is 200 whenever the API is up, with search's
  state under `search`.
- In the chart:
  - `server.*` holds what `router.image`, `router.service`, `router.resources`
    and `search.image`, `search.replicas`, `search.service`,
    `search.resources`, `search.minAvailable` and `search.autoscaling` held;
  - the `-router`, `-router-workers` and `-search` Services are replaced by
    `-server`;
  - KEDA counts the queue in the database, so `keda.prometheus` is gone;
  - `queue` names the release's queue, its name by default.
- There are no Prometheus metrics any more: `/metrics`, the chart's
  `ServiceMonitor`, scrape annotations and `metrics.*` values are gone, and
  check results no longer carry whether the header was warm. `/health`
  reports the live workers on the queue as `workers`.
- Prewarming follows the headers of the jobs that are queued when a worker
  starts, rather than the router's counts since it started.

To the release that moved metadata from TiKV into PostgreSQL:
- Run PostgreSQL, and set `PISA_DATABASE_URL` where `PISA_METADATA` and
  `PISA_TIKV_PD` were, for the search service too (in the chart,
  `metadata.database.urlSecret` where `metadata.tikv` was). The workers and
  the search service create the tables.
- Nothing is carried over from TiKV: snapshot entries and modules, sessions
  and proof states start over, and the first search leader removes the old
  snapshots from the index.
- A listing page that has `next` is full; the page after it may be empty.
- A label value or filter with a NUL character is a `bad_request`.

To the release that moved metadata into TiKV and snapshots onto entries:
- Upgrade the router and the workers together. Their snapshot jobs changed
  shape, and a router and workers of different releases fail each other's.
- Nothing is carried over: the snapshot, session and proof-state metadata in
  S3 is ignored, so snapshots, sessions and states start over.
- The snapshot HTTP API takes entry ids where it took module names; see
  [Snapshots](#snapshots).

To the release that searches snapshots:
- Upgrade the workers first: only they record what the search service reads,
  each module's declarations and a log of changes.
- Modules built before the upgrade aren't searchable, and creating the same
  code again doesn't make them so, as it doesn't build them again.

To the release that moved search onto Qdrant:
- Run Qdrant (≥ 1.19.1), build bundles again (format 4, without `tantivy/`),
  and load them with `pisa-search load`. `serve` takes `--qdrant` and
  `--env-id` where it took `--bundle`.
- Hits no longer carry `lanes`: Qdrant fuses the lanes and doesn't say which
  found a hit.
- Snapshots are indexed again from TiKV by the first leader, without calling the
  LLM or the embedder again for what was described and embedded.

## API

All fields are snake_case. The request and response types are in
`crates/pisa-protocol/src/api.rs`. Code is a Lean file: plain, or
module-system (a `module` header with `public`, `meta` and `all` imports).

The server describes its API and search as OpenAPI 3.1 at `GET /openapi.json`,
also committed as `openapi.json`. Its schemas are derived from the Rust types,
so after changing the API, regenerate the file and the TypeScript types made
from it (tests fail until you do):

```sh
PISA_UPDATE_OPENAPI=1 cargo test -p pisa-server --test openapi
pnpm -C web generate
```

| Endpoint | Request → response |
| --- | --- |
| `POST /check` | `{code, version?, timeout?, detail?}` → check result |
| `POST /check/batch` | `{items: [{id, code}], version?, timeout?, detail?}` → `{results: [...]}`, in order |
| `POST /sessions` | `{code, version?, timeout?}` with `sorry`s → `{session, states: [{state, goal, pos, end_pos}], messages}` |
| `POST /states/{state}/tactic` | `{tactic, timeout?}` → `{state, goals, complete, messages}`; earlier states stay valid |
| `POST /sessions/{session}/check` | `{code, timeout?}` → check result, also requiring the session's original statements and imports |
| `POST /snapshots` | `{code, version?, timeout?, verify?, labels?, key?, replaces?}` → `{entry?, snapshot?, header?, version, dirty, verified, labels?, replaces?, result}`; see [Snapshots](#snapshots) and [Replacing](#replacing) |
| `GET /snapshots` | `?label=k:v` (repeated), `limit?`, `after?`, `version?` → `{entries: [{entry, snapshot, imports, header, labels, replaces?, dirty, verified, created}], next?}`, newest first; see [Labels](#labels) |
| `GET /snapshots/{entry}` | `?version=` → `{entry, snapshot, imports, header, code, labels?, replaces?, dirty, verified, result, created, decls?}` |
| `PATCH /snapshots/{entry}` | `{set?, remove?}`, `?version=` → the entry as in a listing; see [Labels](#labels) |
| `DELETE /snapshots/{entry}` | `?version=` → `{entry, removed: [...]}`, the modules that went with the entry; see [Retention](#retention) |
| `POST /snapshots/{entry}/verify` | `?version=` → `{entry, snapshot, verified, dirty, checked, failure?}`; see [Verifying](#verifying) |
| `GET /jobs/{job}` | `?wait=` (seconds, at most 60) → the request's answer once its job is done, else 202 `{job, kind, status}`; see [Async requests](#async-requests) |
| `DELETE /jobs/{job}` | → `{job, kind, status}`, `cancelled` if no worker had started it |
| `GET /health` | Liveness: `{status, workers, search}`, the live workers on the queue (`null` if it can't be read) and search's state (`null` if the server doesn't search); 503 with `status: draining` once the server is stopping |

Browsers may call the API from other origins only when the server allows
them: `PISA_CORS_ORIGINS` (`--cors-origin`, the chart's `server.corsOrigins`)
takes a comma-separated list of origins, or `*`. It is empty by default.

<a id="async-requests"></a>
**Async requests.** A check, a session's start, tactic or check, and a
snapshot's create or verify can take minutes: a wait for a worker, loading the
imports, Lean's `timeout`, a kernel re-check. Behind a proxy that cuts
requests off at 60 s, or over a network that drops, send them with
`Prefer: respond-async`. The server then answers `202 {job, kind, status:
"queued"}` at once, with `Location: /jobs/{job}`, and runs the request as it
would have:
- `GET /jobs/{job}?wait=30` waits up to `wait` seconds (at most 60) and, once
  the job is done, answers exactly as the request would have: its status and
  body, errors included, e.g. a 429 when no worker took it in time. Until then
  it answers 202 with `status` `queued` or `running`. Any server answers for
  any job.
- An answer is kept for an hour after its job is done; then the job is a 404.
- `DELETE /jobs/{job}` withdraws a job no worker has started (`cancelled`); a
  `running` or `done` one stays as it is.
- Anyone with a job's id can read its answer or cancel it, so keep it secret.
  Build its URL from `job` rather than `Location`, which is relative to the
  server's root.
- `/check/batch`, and the snapshot reads and edits, answer at once whatever
  `Prefer` says: send a batch's items to `/check` instead.

A check result is `{ok, time, error?, messages, sorries, reasons, declarations}`:
- `ok` is the verdict.
- `reasons` says why `ok` is false, e.g. `axiom: t uses sorryAx`.
- `declarations` lists each new declaration with the axioms it uses.

With `detail: ["tactics"]`, a check result also has `tactics`, the goals at
every step of the proof. Each entry is one run of a tactic, in the order Lean
ran them, as `{tactic, goals_before, goals_after, pos, end_pos}`:
- `tactic` is its source text, and `pos` and `end_pos` locate it.
- `goals_before` and `goals_after` list the goals as Lean prints them.
- Structure such as `by`, `·`, `<;>` and parentheses has no entry of its own,
  but the tactics inside it do.
- A tactic run on several goals, e.g. under `all_goals` or `<;>`, appears once
  per run, and so does every alternative `first` tried. One that failed has
  the same goals after as before.

`version` selects a worker pool, by label (such as `v4.35.0-1a2b3c4d`) or by
an alias set with `PISA_ALIASES`. `timeout` is Lean's limit in seconds
(default 60).

**Errors** always look like `{"error": {"kind", "message"}}`.
- **Checks report failures in-band.** A check that times out, crashes or is
  rejected by the policy still returns 200, with `ok: false` and the `error`.
  Batch items work the same way, so one failed item never fails a batch.
- **Other requests use HTTP statuses:**

| `kind` | Status | Meaning |
| --- | --- | --- |
| `lean`, `policy` | 422 | Lean or the policy refused the input, e.g. an unknown tactic |
| `timeout`, `out_of_memory`, `crash` | 422 | The input hit a resource limit |
| `capacity`, `unavailable` | 429, 503 | No worker could take the request, or the metadata store failed; retry later |
| `bad_request`, `not_found` | 400, 404 | Malformed request, unknown state, session or snapshot entry |
| `internal` | 500 | A bug |

## Snapshots

A formalizer that builds a library statement by statement would otherwise
resend, and Lean re-check, the whole library on every compile. Instead it
keeps each accepted chunk as a snapshot and builds on it:

```text
POST /snapshots {"code": "import Mathlib\n\ndef double (n : ℕ) := 2 * n", "labels": {"run": "7"}}
  → {"entry": "9c0e…", "snapshot": "PisaSnap.s1f…", "header": "import Mathlib\nimport PisaSnap.s1f…",
     "dirty": false, "verified": false, "labels": {"run": "7"}, "result": {"ok": true, …}}
POST /check {"code": "import Mathlib\nimport PisaSnap.s1f…\n\nexample : double 2 = 4 := rfl"}
```

**Entries and modules.** A create returns two names:
- **The module** (`snapshot`, e.g. `PisaSnap.s1f…`) is what code imports. It
  comes from a hash of the code, so the same code is the same module, and its
  check result, `dirty` and `verified` belong to it.
- **The entry** (`entry`, 32 hex digits) is what this create added: a
  reference to the module with its own `labels` and `created` time. Creating
  the same code again adds another entry on the same module without building
  it again. `GET`, `PATCH`, `DELETE` and `verify` take the entry; a module
  name there is a `bad_request`.

A create that keeps nothing has no `entry`. Neither does `verify: true` on code
that is already kept but fails the re-check: the response then has its
`snapshot` and `header` with the error, as new code that fails the re-check
isn't kept.

`GET /snapshots/{entry}` lists the module's declarations in `decls`, each
`{name, kind, signature, docstring?, informal?}`. `informal` is the description
that search writes when it has a describe model (`PISA_SEARCH_DESCRIBE_MODEL`;
see [Snapshot search](#snapshot-search)), a little after the create.

<a id="labels"></a>
**Labels.** An entry's labels are string key/value pairs, at most 64 of them. A
key is 1–128 bytes of letters, digits, `.`, `_`, `/` and `-`; a value is up to
512 bytes of UTF-8 without NUL characters. Labels that break these rules are a `bad_request`: in
`result.error` on a create, which then keeps nothing, and a 400 on a `PATCH`,
which then changes nothing.
- **Editing.** `PATCH /snapshots/{entry}` with `{"set": {"stage": "done"},
  "remove": ["draft"]}` removes the `remove` keys, then sets the `set` pairs,
  and returns the entry as a listing shows it.
- **Filtering.** `GET /snapshots?label=project:foo&label=kind:lemma` lists the
  entries that have every label given, each an exact match of `key:value`
  split on the first `:`, so a value can contain `:`. Without `label` it lists
  every entry of the version. There is no OR or prefix matching. Two values for
  the same key match nothing, as an entry has one value per key.
- **Order and pages.** Entries come newest first, as summaries: what
  `GET /snapshots/{entry}` returns without `code`, `result` and `decls`.
  `limit` is 1–1000 (default 100), and a listing takes up to 64 `label`s. Keep
  requesting while a page has `next`, an opaque cursor, passing it as `after`:
  a full page has `next`, and the page after it may be empty. A cursor is a position, so it
  keeps working when its entry is deleted.
- **Speed.** On PostgreSQL, labels are matched through an index (GIN), so the
  order of the `label`s doesn't matter.

**Retrying a create.** A create can carry an idempotency `key`, 1–256 bytes of
letters, digits, `.`, `_`, `:`, `/` and `-` (anything else is a `bad_request`
in `result.error`), so that a retry is safe:
- While an entry made with the key exists, creating the same code with it
  returns that entry as it is, without building anything. The retry's other
  fields are ignored, `labels` and `verify` included, so a retry with
  `verify: true` can return `verified: false` with no error.
- Different code under a used key is a `bad_request` in `result.error`, and
  nothing is kept.
- A create that keeps nothing, e.g. when Lean reports errors or a `verify: true`
  re-check fails, doesn't use the key up, so a later create can.
- Deleting an entry frees its key.

<a id="replacing"></a>
**Replacing an entry.** A create can name the entry it `replaces`, e.g. the
skeleton with `sorry`s that a proof fills in. The code is kept only if it
still declares everything the replaced entry's module declares, each with the
same type and, for definitions, the same value; it may add declarations, such
as helper lemmas, and may still use `sorry`. So filling a `sorry` can't
quietly change what is proved:

```text
POST /snapshots {"code": "…theorem t : 1 = 1 := rfl", "replaces": "9c0e…"}
  → {"entry": "4b7a…", …, "replaces": {"entry": "9c0e…", "filled": ["t"]}}
```

- **Refused.** Code that changes a statement is not kept, and has no `entry`:
  `result.ok` is false and `result.reasons` says what changed, e.g.
  `statement: t has a different type`. To change a statement on purpose,
  create the code without `replaces`.
- **Imports.** The code may import more, but must still build on every
  snapshot the replaced entry's module builds on, directly or through others.
  A type names the constants it uses, such as a definition from an imported
  snapshot, without saying what they are, so swapping that snapshot for
  another could change what a statement means while every hash stays the
  same. To move an entry onto a new version of a snapshot it imports, create
  it again on that version first, then replace that.
- **Definitions.** `"replaces": {"entry": "9c0e…", "allow": "definitions"}`
  lets definitions change their value but not their type, e.g. to add a
  missing hypothesis. A theorem's type only names a definition, so its
  meaning can change while its type matches: `replaces` in the response lists
  the `changed_definitions` and the theorems of the new module whose type uses
  one, directly or through another definition (`affected`).
- **The link.** `filled` lists the declarations that used `sorry` and no
  longer do. The new entry records `replaces: {entry, allow?}`, which listings
  and `GET /snapshots/{entry}` show. Only a create that passed the comparison
  sets it, so it can be trusted, and it stays after the replaced entry is
  deleted. A key replay returns the entry as it is, with `filled` empty.

**How it works.**
- **Building.** A module is its code compiled with `lean -o` to the module
  `PisaSnap.s<id>`, where `id` is a hash of the code.
- **Using.** Later code imports it like any module, in `/check`, `/sessions`
  or another snapshot. A chunk can import several snapshots, which merges
  parallel branches.
- **Hits.** The worker that built a snapshot keeps a Lean process with the
  returned `header` imported, and code that starts with exactly that header
  goes there. It then costs only its own elaboration. Other workers fetch the
  module from the store and import it (seconds at Mathlib scale).

**What carries over.** Snapshots follow Lean's module semantics. Children see
a parent's declarations, instances, `@[simp]` lemmas and notation, but not its
`private` declarations, `open`s, `variable`s, `set_option`s or unclosed
sections. Re-open what a chunk needs.

**Module-system code.** A chunk can be a module-system file (`module`,
`public import`, `@[expose]`):
- **Visibility.** Children then see what the module system exports: `public`
  declarations, with bodies only for `@[expose]` definitions.
- **Imports between kinds.** A `module` file can import only module-system
  snapshots; plain files can import either.
- **The returned `header`.** It keeps `module` and imports the new snapshot
  with the modifiers the chunk used for its snapshot imports. If the chunk
  had none, it uses `public import`, so chains stay transitive.

**Results.**
- Code is kept only if Lean reports no errors and, when it runs, the kernel
  re-check passes (see [Verifying](#verifying)). `result` is a check result, as
  for `/check`, without `sorries`.
- A chunk with `sorry` or another axiom outside the allowed list is kept but
  `dirty`, and checks that use it report the axiom.
- Imports of an unknown or deleted snapshot are `not_found`.

<a id="verifying"></a>
**Verifying.** The kernel re-check imports the snapshot's dependencies again and
re-runs the kernel over its declarations, which adds about 9 s to a small chunk
on `import Mathlib`. So most code skips it while exploring and runs it at the
end. It depends on `verify` in the request:

| `verify` | Kernel re-check at creation |
| --- | --- |
| omitted, no macros | None |
| omitted, the code uses macros or imports a snapshot that does | This chunk; if the kernel rejects it, it isn't kept |
| `true` | This chunk and any unverified imports; the result is `verified`, or has an `error` |
| `false` | None, even with macros |

`verified` means the snapshot and everything it imports have passed the
re-check. So a macro chunk on top of unverified snapshots is re-checked but not
yet `verified`.

`POST /snapshots/{entry}/verify` re-checks the entry's module and everything it
imports, imports first, and records each module that passes. A later run only
covers new chunks, and a retry resumes where the last one stopped. `verified`
and `dirty` belong to modules, so every entry on a module shows the result.
- `checked` lists the modules this run re-checked and passed.
- If the kernel rejects a module or the re-check times out (60 s per module),
  the run stops, still with a 200, and `failure` has the module and the reason.
  Nothing is deleted, because other code may import it: the module stays usable
  and not `verified`. Delete its entries if you don't want it.
- `dirty` and `result` are updated from the re-check, whose axioms don't depend
  on the build being honest.

Check results don't say whether the snapshots they import are verified: look
at the snapshot, or verify it, when that matters.

<a id="retention"></a>
**Retention.** An entry is kept until it is deleted. A module is kept while an
entry points at it or a kept module imports it: deleting an entry removes its
module once nothing else needs it, then each of that module's imports on the
same terms, and `removed` lists them. So deleting the entry of a snapshot that
others build on removes nothing yet, and the code on top keeps working. Each
worker checks a module it has cached against the store at most every
`PISA_SNAPSHOT_CONFIRM_SECS` (60), so a removal takes effect everywhere within
that time.

**Trust.** A snapshot that isn't `verified` has the same guarantee as a
`/check`: the policy scan and an axiom report from the process that imported
it. Macros go beyond that, because what they expand into isn't in the text the
scan sees, so with `verify: false`, `ok` and `dirty` can be wrong until the
snapshot is verified. A `verified` snapshot's own declarations and those of
everything it imports have been re-checked through the kernel, so a forged
proof is caught.

Either way, the `.olean` is imported without re-checking, as with Mathlib's
cache: whoever can write the store can still install an unchecked module, and
the recorded SHA-256 catches only corruption, not a malicious write. The base
Mathlib/Physlib oleans and the kernel are the trusted base.

**Searching them.** The servers can search snapshot declarations, by
label, next to Mathlib's; see [Snapshots in search](#snapshot-search).

## Clients

Rust (`pisa-client`):

```rust
let client = pisa_client::Client::new("http://localhost:8000")?.with_timeout(60);
let result = client.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num").await?;
assert!(result.ok);
let session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry").await?;
let step = client.tactic(&session.states[0].state, "simp").await?; // Error::Server { kind: Lean, .. } if it fails
let verdict = client.check_session(&session.session, "theorem t (n : Nat) : n + 0 = n := by simp").await?;
let base = client.create_snapshot("import Mathlib\ndef double (n : ℕ) := 2 * n").await?;
let next = client.check(&format!("{}\nexample : double 2 = 4 := rfl", base.header.unwrap())).await?;
let entry = base.entry.unwrap();
let page = client.list_snapshots(&[("run", "7")], Some(50), None).await?; // newest first
let more = client.list_snapshots(&[("run", "7")], Some(50), page.next.as_deref()).await?;
let done = std::collections::BTreeMap::from([("stage".to_string(), "done".to_string())]);
let summary = client.set_snapshot_labels(&entry, &done, &[]).await?;
let verified = client.verify_snapshot(&entry).await?;
// Kept only if it keeps the replaced entry's statements; see Replacing.
let replaces = Some(pisa_client::Replaces::Entry(entry.clone()));
let opts = pisa_client::SnapshotOptions { replaces, ..Default::default() };
let lemma = "import Mathlib\ndef double (n : ℕ) := 2 * n\ntheorem double_two : double 2 = 4 := rfl";
let extended = client.create_snapshot_with(lemma, &opts).await?;
let removed = client.delete_snapshot(&entry).await?; // the modules that went with it
// A long request as a job (see Async requests), answered however long it takes.
let job = client.submit_check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num").await?;
let result = client.wait_job(&job).await?; // what check would have returned
```

Python (`pisa_client`), built from `crates/pisa-python` with `maturin build` or `maturin develop`:

```python
from pisa_client import Client, LeanError

client = Client("http://localhost:8000", timeout=60)
client.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num")["ok"]
session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry")
step = client.tactic(session["states"][0]["state"], "simp")   # raises LeanError if it fails
client.check_session(session["session"], "theorem t (n : Nat) : n + 0 = n := by simp")["ok"]
base = client.create_snapshot("import Mathlib\ndef double (n : ℕ) := 2 * n", labels={"run": "7"})
client.check(base["header"] + "\nexample : double 2 = 4 := rfl")["ok"]
page = client.list_snapshots({"run": "7"}, limit=50)   # newest first; pass page["next"] as after=
client.set_snapshot_labels(base["entry"], set={"stage": "done"}, remove=["run"])
client.verify_snapshot(base["entry"])["verified"]
more = "import Mathlib\ndef double (n : ℕ) := 2 * n\ntheorem double_two : double 2 = 4 := rfl"
client.create_snapshot(more, replaces=base["entry"])   # or {"entry": …, "allow": "definitions"}
client.delete_snapshot(base["entry"])   # the modules that went with it
job = client.submit_check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num")
client.wait_job(job)["ok"]   # what check would have returned, or raised
```

Calls are synchronous. They release the GIL, so one `Client` can be shared
across threads. Results are plain dicts; the type stubs describe their shapes.
Exceptions carry `.kind` and `.status`, and their class says what to do:
- `LeanError`: Lean or the policy refused the input.
- `LimitError`: the input hit a timeout, the memory limit, or crashed Lean.
- `CapacityError`: no worker was free, or the server is unavailable; retry later.
- `NotFoundError`: unknown or expired state or session, or unknown snapshot entry.
- `PisaError`: the base class; also raised for transport failures, with no
  `kind` or `status`.

To run the Python tests:

```sh
cargo build -p pisa-server -p pisa-worker -p pisa-tests -p pisa-search
cd crates/pisa-python && maturin develop && pytest tests
```

TypeScript (`@aqora/pisa-client`, in `web/packages/client`) has no
dependencies, and runs in browsers and Node:

```ts
import { PisaClient } from "@aqora/pisa-client";

const client = new PisaClient({ url: "http://localhost:8000", timeout: 60 });
const result = await client.check("theorem t : 2 + 2 = 4 := by decide");
const session = await client.startSession("theorem t (n : Nat) : n + 0 = n := by sorry");
// Throws a PisaError (kind "lean") if the tactic doesn't parse; an error Lean
// recovers from, like an unknown identifier, is in step.messages instead.
const step = await client.tactic(session.states[0]!.state, "simp");
```

React (`@aqora/pisa-react`, in `web/packages/react`) has hooks over TanStack
Query: `useCheck` and `useLiveCheck` for an editor, `useProofSession` for a
tree of tactics, the snapshot queries and mutations, `useSearch`, `useResolve`
and `useDecl`. The packages' READMEs have examples.

The playground (`web/apps/playground`) uses the hooks for checks, proofs,
snapshots and search. `vite dev` proxies `/api` to `PISA_URL`, by default the
compose stack's `http://localhost:8000`:

```sh
cd web && pnpm install
pnpm dev                                  # http://localhost:5173
PISA_URL=http://localhost:8001 pnpm dev   # another server
```

To serve the built playground from another origin, build it with
`VITE_PISA_URL` set to the server, and allow its origin with
`PISA_CORS_ORIGINS` (see [API](#api)).

To run the web tests (`test:integration` starts a server and a fake worker,
and searches too when `PISA_TEST_QDRANT_URL` is set):

```sh
cd web && pnpm typecheck && pnpm test
cargo build -p pisa-server -p pisa-worker -p pisa-tests && pnpm test:integration
```

The TypeScript types are generated from `openapi.json` (see [API](#api)) into
`web/packages/client/src/openapi.ts` by `pnpm -C web generate`; a test fails
until they are regenerated after the spec changes.

## Search

Search finds Mathlib and Physlib declarations from a description,
LaTeX, a Lean pattern or goal, or a partial or misspelled name. It is meant for
agents that know what they need but not what it is called. Its index is built
from the same pinned project as the workers, and every response carries that
project's version label as `env_id`, so the declarations it returns exist in
the matching worker pool.

**How it runs.** The index lives in [Qdrant](https://qdrant.tech) (≥ 1.19.1):
a collection `pisa-<env_id>` with a point per declaration, loaded from a bundle
with `pisa-search load`. The servers search when given `--qdrant URL --env-id
ENV` (`PISA_SEARCH_QDRANT`, `PISA_SEARCH_ENV_ID`), and hold nothing, so any
number of replicas serve one environment. Each collection has one shard, as
Qdrant's IDF is per shard; for availability, run Qdrant as a cluster and load
with `--replication 2`. A server opens its index in the background, so until
Qdrant answers with the bundle, `/v1/search` answers 503, `/health` says why,
and the rest of the API works.

**How it ranks.** Each of four lanes is a Qdrant query, and Qdrant fuses them
with weighted reciprocal rank fusion:
- **lexical:** BM25 over names, descriptions, signatures and docstrings, as a
  sparse vector with a token per field and term;
- **name:** the name's trigrams, plus an exact match, which always comes first;
- **structural:** constants the query names or implies, matched against each
  type's constants. LaTeX symbols map to constants through
  `search/symbols.toml`;
- **dense:** Qwen3-Embedding-8B vectors for every declaration, from Scaleway's
  serverless API, which also embeds each query. Qwen3-Embedding-0.6B is a
  free, local alternative (TEI on CPU, or llama.cpp on a GPU) that retrieves
  somewhat worse; see `search/bench/2026-10-22/report.md`.

Qdrant's weights scale a lane's ranks rather than its scores: a weight below 1
makes a lane count only for its first few hits. The tuned defaults (`Weights`
in `crates/pisa-search/src/engine.rs`) lean on the dense lane, with lexical and
structural matches lifting their top hits. PageRank over the dependency graph is
a prior, added after fusion, and a name query also favours names close to it by
edit distance. Declarations that depend on `sorry` are hidden unless
`include_sorried` is set, and so are Lean-generated helpers (`….mk.injEq`) and
metaprogramming internals (`Mathlib.Tactic.…`); any of them is still returned,
flagged, when a query names it exactly. Deprecated aliases and `to_additive`
pairs fold into one hit.

**Descriptions.** Mathlib declarations reuse LeanSearch v2's descriptions
(Apache-2.0, see `search/NOTICE`), matched by name and by the constants in
their type. `pisa-search informalize` writes the rest with Claude Haiku 5.5
(about $38 for the 128k declarations left, in about 3 hours; other Claude and
OpenAI models are a flag away):
- It works in dependency order, so each prompt shows descriptions of what the
  declaration uses.
- It caches by content hash, so after a library update only changed
  declarations cost anything.
- It checks LaTeX against KaTeX and checks that cited Lean names exist.

**Prebuilt indexes.** Each index is a GitHub release tagged `index-<env_id>`,
on the commit that built it. Its assets are archives of the gitignored `search/`
directories, each with `NOTICE`:
- `bundle.tar.zst`: `search/bundle`, which `load` puts into Qdrant;
- `store.tar.zst`: `search/store`, the descriptions and vectors. A rebuild that
  starts from it only describes and embeds new or changed declarations;
- `corpus.tar.zst`: `search/corpus`, the exported declarations.

```sh
# Serve a published index (the repository is private, so gh must be logged in).
# Swap bundle for store or corpus to fetch the others.
gh release list -R aqora-io/pisa
gh release download index-v4.35.0-da683984 -R aqora-io/pisa -p bundle.tar.zst -O - \
  | tar -I zstd -x -C search
docker compose --profile search-load run --rm search-load   # into Qdrant, once
PISA_SEARCH_QDRANT=http://qdrant:6333 SCW_SECRET_KEY=... \
  docker compose --profile search up --build                 # on :8000 (PISA_PORT), through Traefik
# Or export COMPOSE_PROFILES=search and PISA_SEARCH_QDRANT=http://qdrant:6333
# (in .envrc, say) for search from a plain `docker compose up`.
```

`load` refuses a bundle of another format (`bundle format 3 is not 4; rebuild
it`). Build one again from its release's `store` and `corpus` with `pisa-search
build` (below): it reads only those, so it describes and embeds nothing, and
takes a few minutes. Then publish it.

To publish an index after building it (below), from the pushed commit that built
it:

```sh
ENV_ID=$(jq -r .env_id search/bundle/manifest.json)
mkdir -p search/release
for d in bundle store corpus; do
  tar -I 'zstd -T0' -cf search/release/$d.tar.zst -C search $d NOTICE
done
# A new env_id gets a new release.
gh release create index-$ENV_ID -R aqora-io/pisa --target "$(git rev-parse main)" \
  --title "Search index $ENV_ID" --notes "$(jq -c . search/bundle/manifest.json)" \
  search/release/*.tar.zst
# A rebuild for an env_id that already has a release replaces the assets it
# rebuilt (pack only those); then edit the notes' manifest and which commit
# built what with `gh release edit index-$ENV_ID --notes-file …`.
gh release upload index-$ENV_ID -R aqora-io/pisa --clobber search/release/*.tar.zst
```

**Building an index:**

```sh
# Export the pinned project's declarations (about 25 minutes).
docker build -f docker/Dockerfile --target corpus --output search/corpus .
# Reuse LeanSearch v2's Mathlib descriptions (downloads 332 MB, verified by sha256).
pisa-search import-lsv2 --corpus search/corpus --file search/lsv2.jsonl --informal search/store
# Describe everything else with Claude (needs ANTHROPIC_API_KEY), and embed every
# declaration as its description lands (needs SCW_SECRET_KEY). Try --sample 500 first.
LIBS=Mathlib,Physlib,Batteries,Init,Std,QuantumInfo
EMBEDDER="openai+https://api.scaleway.ai/v1?model=qwen3-embedding-8b&key_env=SCW_SECRET_KEY"
pisa-search embed --corpus search/corpus --informal search/store --libraries $LIBS \
  --embedder "$EMBEDDER" --label qwen3-embedding-8b@scaleway --batch 32 --concurrency 32 \
  --follow search/work/described &
pisa-search informalize --corpus search/corpus --informal search/store --libraries $LIBS \
  --concurrency 40 && touch search/work/described; wait
# Build the bundle, load it into Qdrant and serve it on :8000.
pisa-search build --corpus search/corpus --informal search/store \
  --embedding-model qwen3-embedding-8b@scaleway --out search/bundle
docker compose --profile search-load run --rm search-load
PISA_SEARCH_QDRANT=http://qdrant:6333 docker compose --profile search up --build
```

`load` refuses a bundle whose vectors cover less than 99% of its declarations
(`--min-coverage`). Loading again the same `env_id` replaces its declarations in
place, keeping the snapshots'; queries meanwhile may see some of each. Restart
the servers after it: they read the manifest (libraries, kinds, field
statistics) when they open the index.

`--informal` takes a directory or an `s3://bucket/prefix` URL (credentials come
from `AWS_*`). It holds descriptions and vectors, keyed by content hash.

**API** (types in `crates/pisa-protocol/src/search.rs`):

| Endpoint | Request → response |
| --- | --- |
| `POST /v1/search` | `{queries: [{q?, nl?, latex?, pattern?, goal?, name?, libraries?, kinds?, k?, include_sorried?, snapshots?}], env_id?}` → `{env_id, results: [{hits}]}`, at most 64 queries |
| `POST /v1/resolve` | `{names, env_id?}` → `{env_id, results: [{name, exists, deprecated_to?, suggestions}]}` |
| `GET /v1/decl/{name}` | `?module=` → `{env_id, decl}`: signature, description, flags, dependencies and users; with a snapshot hit's `module`, that snapshot's declaration, else the bundle's |

Each hit carries:
- name, kind, library, module and signature;
- a description, when one exists;
- flags: `uses_sorry`, `informal`, `deprecated_to`, `additive_sibling`;
- a score, comparable only within one query, and a source link;
- `text`, about 150 tokens ready for a model's context;
- for a snapshot declaration, `snapshot` (see [below](#snapshot-search)).

Unknown names and empty results are answers, not errors. A `libraries` or
`kinds` filter matches case-insensitively; an unknown one is a 400 that lists
the known ones. Pinning an `env_id` the server doesn't have is a 404. If the
embedding API fails, or hasn't answered within 10 s, the query still returns
results from the other lanes and says so in the result's `warnings`. A slow
embedding goes on in the background, for up to 60 s, and the result has
`dense_pending`: asking again shortly includes the dense lane. Each server keeps
the last 1,024 query vectors, and a search for text that is already being
embedded waits for that request rather than sending another. If Qdrant
doesn't answer, search requests are a 503 and `/health`'s `search.qdrant` is
false; the server stays ready, as the rest of its API works and every replica
shares the one Qdrant.

```python
from pisa_client import SearchClient

search = SearchClient("http://localhost:8000")
for hit in search.search(nl="trace of a density matrix is one", libraries=["Physlib"])["hits"]:
    print(hit["text"])
search.resolve(["Real.sqrt_mul_sefl"])[0]["suggestions"][0]["name"]  # "Real.sqrt_mul_self"
```

The Rust `pisa_client::SearchClient` has the same methods. The search tests,
Rust and Python, need a Qdrant at `PISA_TEST_QDRANT_URL` and are skipped
without one; for the Python ones, first run
`cargo build -p pisa-server -p pisa-search`.

<a id="snapshot-search"></a>
**Snapshots in search.** The servers also search the declarations of
[snapshots](#snapshots), from the database they share with the workers, ranked with
the bundle's in every lane, unless `PISA_SEARCH_NO_SNAPSHOTS` is set:
- **Scope.** A query's `snapshots` says which: `"none"` (the default), `"all"`,
  or `{"labels": {"run": "7"}}`, the modules of the entries that have every
  label, as `GET /snapshots?label=` matches them. The snapshots those import
  aren't included. Snapshot declarations are in the library `PisaSnap`, so
  adding `"libraries": ["PisaSnap"]` searches snapshots only.
- **Hits.** A hit's `module` is the snapshot, e.g. `PisaSnap.s1f…`, and
  `flags.imports` the header that loads it. `snapshot` lists the scope's
  entries on that module, newest first, at most 10, with the module's
  `dirty` and `verified`. Names can repeat across snapshots, and each is a hit
  of its own. `/v1/resolve` and `/v1/decl` cover the bundle only.
- **The leader.** One server at a time, holding a lease in the database
  (30 s, renewed every 10 s), indexes snapshots: it writes each module's
  declarations to Qdrant, and each entry to `pisa-<env_id>-entries`, which a
  label scope is resolved against. So a module can have any number of
  entries, but a label scope may cover at most 20,000 modules. When the
  leader goes, another server takes the lease, makes Qdrant match the
  catalog, and carries on; `/health`'s `search.leader` says which leads.
- **Freshness.** Workers export a module's declarations when they build it,
  and log every create, relabel, delete and re-check in the database. The
  leader reads that log every 2 s. A snapshot is searchable by name, signature and
  docstring seconds after it is created, with a vector of its docstring;
  its description, and a vector of that, follow when the LLM and the
  embedder answer. Module-system snapshots have no docstrings in the export.
- **Descriptions.** With `PISA_SEARCH_DESCRIBE_MODEL` set to a Claude model
  (with `ANTHROPIC_API_KEY`) or an OpenAI one (with `OPENAI_API_KEY`), the
  leader describes every snapshot declaration as `informalize` describes the
  libraries.
  So all snapshot code goes to that provider, at about $0.0003 a declaration
  with Claude Haiku 5.5. Without one, declarations are embedded from their
  docstrings. A failed description is tried 3 times in all; an
  authentication error pauses describing for 5 minutes, and a failed
  embedding pauses embedding for one. `PISA_SEARCH_DESCRIBE_CONCURRENCY` (8)
  bounds the requests in flight.
- **Vectors.** The embedder that embeds queries (`PISA_SEARCH_TEI`, Scaleway or
  TEI) also embeds each snapshot declaration's signature, docstring and
  description, so it must serve the model that embedded the bundle.
- **Storage.** Descriptions and vectors are kept in the database with the
  module, so a new leader writes the same points without calling either API,
  and Qdrant can be rebuilt from the database and the bundle. They are
  deleted with the module.
- **Version.** It searches the snapshots of the workers whose version is the
  bundle's `env_id`; `PISA_SEARCH_SNAPSHOT_VERSION` names another.

```python
hits = search.search(nl="doubling adds a number to itself", snapshots={"labels": {"run": "7"}})["hits"]
hits[0]["module"], hits[0]["snapshot"]["entries"]  # "PisaSnap.s1f…", ["9c0e…"]
```

**Quality.** Two reports track it:
- `search/eval/<env_id>-qdrant.md`: recall, MRR and nDCG on LeanSearch v2's
  MathlibQR benchmark, per lane and per query style, with
  `<env_id>-before-qdrant.md` for the in-memory engine it replaced. Run it with
  `pisa-search eval --qdrant URL --env-id ENV --data MathlibQR.json`; several
  `--weights` are compared in one run;
- `search/pilot/2026-10-22/report.md`: the informalization pilot, with cost,
  a full-run estimate, check pass rates and samples to review.

**Imports.** Most of the index loads with `import Mathlib` and `import Physlib`.
QuantumInfo (density matrices, entropies, channels) can't be imported
alongside Mathlib, because both declare `Commute.log_left`; pisa reports that
clash as a header error. Its declarations are exported from a second
environment, and each hit carries the header that loads it in
`flags.imports` (`import Physlib; import QuantumInfo`) and in its `text`.

## Verification policy

A check is `ok` only when all of these hold:

- The code elaborated without errors.
- Every new declaration depends only on `propext`, `Classical.choice` and
  `Quot.sound`. This rejects `sorry`, `admit`, `native_decide` and new axioms.
- The code contains no metaprogramming: `#eval`, `run_cmd`, `elab`,
  elaborator attributes, `unsafe`, `implemented_by` and similar.
- Any `set_option` changes only an allowlisted option: resource limits
  (`maxHeartbeats` and similar), `linter.*`, `pp.*`, `trace.*`, and
  `backward.*`, which restores older elaborator behaviour that Mathlib-style
  code often asks for. Options that skip the kernel, such as
  `debug.skipKernelTC`, are rejected.

The axiom report is a `run_cmd` metaprogram. It needs `Lean.Elab.Command`, so
workers add `import Lean.Elab.Command` to every header they load, or
`meta import Lean.Elab.Command` to a module-system header.

This scan is a conservative pre-filter, not the last word: it over-approximates
"the kernel checked every declaration with sound options" by forbidding
anything that could subvert the kernel or the report. A `verified`
[snapshot](#verifying) doesn't rely on it. The re-check runs after the build,
in a separate clean process (`docker/verifier/PisaVerify.lean`, run with
`lean --run`): it re-adds the module's own declarations to an environment of
just its dependencies, with the kernel checking each one, and collects their
axioms by walking the re-checked constants. So a declaration added during the
build with the kernel check skipped is caught even though no forbidden keyword
appears in the source. Because that verdict doesn't depend on the build being
honest, snapshot creation allows the **macro family** (`macro`, `macro_rules`,
`syntax`, `declare_syntax_cat`) — pure syntax transformers whose only risk is
what they expand into, which the re-check covers. It runs by default for code
that uses them, and `verify: false` opts out. `/check` and `/sessions`, which
don't produce a module to re-check, keep the scan as-is.

## Limits

- The REPL never frees environments, so workers replace each process after
  `PISA_MAX_USES` jobs or at a memory high-water mark.
- REPL tactic mode can't write a finished tactic script back into the `sorry`.
  Clients submit the assembled proof to `/sessions/{session}/check`.
- Scoped extensions, such as scoped notation, don't survive pickling.
- Not built yet: auth and per-tenant fairness, `/jobs` async batches, and
  human workspaces (stage 4).
