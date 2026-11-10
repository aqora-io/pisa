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
docker compose up --build              # Lean v4.36.0-rc2 + Mathlib + Physlib (first build compiles Physlib)
PROJECT=lean-core docker compose up --build   # no Mathlib, for quick checks
python3 scripts/e2e.py [--mathlib] [--search]   # end-to-end checks against the running stack
cargo test --workspace                 # fast tests with fake-repl, no Lean needed
(cd docker/pisa-lean && lake build) && cargo test -p pisa-worker --test real_lean -- --ignored   # the report and the verifier on real Lean
```

CI (`.github/workflows/ci.yml`) runs these on every pull request and push to
main: the workspace's tests with PostgreSQL as well as SQLite, the real-Lean
tests, the web and Python clients' tests, and `scripts/e2e.py` on a
`lean-core` stack, without `--mathlib` or `--search`.

Compose is laid out like a deployment: Traefik on port 8000 (`PISA_PORT` to
change it) spreads requests over two servers, which it finds through Docker
and health-checks on `/health`, and two workers take jobs from PostgreSQL,
next to Garage for S3.
`--scale server=N` or `--scale worker=N` runs more of either. The servers and
the workers keep their data in two places, chosen by environment variables
that Compose and the chart set:
- **The job queue and the metadata**, the sessions, the records that make
  proof states loadable, and the snapshots and module records, are in a
  database, which every server and worker share. `PISA_DATABASE_URL` is
  a PostgreSQL URL (`postgres://user:password@host/db`) or the path of a
  SQLite file, which only one host can use. It is required.
  - Each process creates or updates the tables when it starts, one process
    at a time.
  - Jobs and workers are kept apart by `PISA_QUEUE` (default `pisa`), which
    the servers and their workers must agree on. Deployments that share a
    database need queues of their own.
  - PostgreSQL holds the only copy of the snapshots and module records,
    which never expire, so run it durably and back it up.
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
snapshots and module records) live in PostgreSQL, and blobs (pickled
proof states, snapshot modules) in an S3-compatible bucket.
- The chart doesn't deploy PostgreSQL. Put the URL of one you run, for
  example with [CloudNativePG](https://cloudnative-pg.io) or a managed
  service, in a Secret, and name it in `metadata.database.urlSecret.name`
  (`metadata.database.urlSecret.key` names the key, `url` by default). It is
  required, for the servers, the workers and KEDA. PostgreSQL holds the only
  copy of the snapshots and module records, which never expire, so it
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
(the server image has `pisa-search`). Until it has, `/search` answers 503
and the rest of the API works.

**Not covered by the chart:**
- Separate pools per project version. The scaling query counts every job, so one release should serve one version.
- Auth on the API.
- A tighter security context: workers run privileged so bubblewrap and cgroup limits work.

## Upgrading

To the release that names each thing in the API once:
- Reset the database, and the object store's `snapshots/` and `states/`.
  Nothing reads what earlier releases stored, so snapshots, sessions and
  proof states start over.
- Rebuild the images, and upgrade the servers and the workers together: their
  jobs changed shape, and a server and workers of different releases fail
  each other's.

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
| `POST /check` | `{code, version?, timeout?, detail?, at?}` → check result |
| `POST /check/batch` | `{items: [{code}], version?, timeout?, detail?, at?}`, up to 128 items, 32 queued at a time → `{results: [...]}`, in order |
| `POST /sessions` | `{code, version?, timeout?}` with `sorry`s → `{id, states: [{id, goals, pos, end_pos}], messages}` |
| `POST /states/{id}/tactic` | `{tactic, timeout?}` → `{state: {id, goals}, complete, messages}`; earlier states stay valid |
| `POST /sessions/{id}/check` | `{code, timeout?, detail?}` → check result, also requiring the session's original statements and imports |
| `POST /snapshots` | `{code, version?, timeout?, verify?, labels?, key?, replaces?, fork?, holes?, detail?}` → `{snapshot?, check, replacement?, heads?}`; see [Snapshots](#snapshots) and [Replacing](#replacing) |
| `GET /snapshots` | `?label=k:v` (repeated), `limit?`, `after?`, `heads?`, `version?` → `{snapshots: [{id, version, module, header, imports, labels, replaces?, holes?, status, verified, created}], next?}`, newest first; see [Labels](#labels) |
| `GET /snapshots/{id}` | `?detail=declarations` → the snapshot as in a listing, with `code`, `check` and `exports?` |
| `GET /snapshots/{id}/dependents` | `?label=k:v` (repeated) → `{snapshots: [...]}` as in a listing, parents first; see [Rebuilding](#rebuilding) |
| `POST /snapshots/rebuild` | `{from, to, labels, dry_run?, allow_changes?, retire?, verify?, timeout?}` → `{ok, changes, changed_definitions?, plan: [{level, snapshot_id?, module, new_module, new_header, code, labels, key, replaces?, affected?}], replaced?, created?, failed?, skipped?, not_deleted?, retired?}`; async with `Prefer: respond-async`; see [Rebuilding](#rebuilding) |
| `GET /snapshots/audit` | `?label=k:v` (repeated), `version?` → `{summary: {proved, conditional, sorry, rejected, unverified_modules}, assumptions: [{name, signature?, snapshot_id?, verified, used_by}], not_proved: [{name, snapshot_id, status, axioms}]}`; or `?against=<id>` (repeated), `allow=<name>` (repeated) → the same, the assumptions with `in_spec`, and `{clean, heads?, forks?: [{snapshot_id, replaced_by}], targets?: [{name, spec, head, status, verified, accepted}], links?: [{snapshot_id, replaces: {snapshot_id, allow}, replacement: {filled, changed_definitions?, affected?}}], comparisons?: [{spec, head, reasons?, changes?, replacement?}]}`; see [Statuses](#statuses) and [Auditing a run](#spec-audit) |
| `PATCH /snapshots/{id}` | `{labels: {key: value or null}}` → the snapshot as in a listing; see [Labels](#labels) |
| `DELETE /snapshots/{id}` | `?with_replacers=true` (default `false`) → `{removed: [...], deleted: [...]}`, the modules that went and the snapshots deleted, the asked one first; see [Retention](#retention) |
| `POST /snapshots/{id}/verify` | → `{snapshot, checked, failure?}`; see [Verifying](#verifying) |
| `GET /jobs/{id}` | `?wait=` (seconds, at most 60) → the request's answer once its job is done, else 202 `{id, kind, status}`; see [Async requests](#async-requests) |
| `DELETE /jobs/{id}` | → `{id, kind, status}`, `cancelled` if no worker had started it |
| `GET /health` | Liveness: `{status, workers, search}`, the live workers on the queue (`null` if it can't be read) and search's state (`null` if the server doesn't search); 503 with `status: draining` once the server is stopping |

Browsers may call the API from other origins only when the server allows
them: `PISA_CORS_ORIGINS` (`--cors-origin`, the chart's `server.corsOrigins`)
takes a comma-separated list of origins, or `*`. It is empty by default.

<a id="async-requests"></a>
**Async requests.** A check, a session's start, tactic or check, a
snapshot's create or verify, and a rebuild can take minutes: a wait for a worker, loading the
imports, Lean's `timeout`, a kernel re-check. Behind a proxy that cuts
requests off at 60 s, or over a network that drops, send them with
`Prefer: respond-async`. The server then answers `202 {id, kind, status:
"queued"}` at once, with `Location: /jobs/{id}`, and runs the request as it
would have:
- `GET /jobs/{id}?wait=30` waits up to `wait` seconds (at most 60) and, once
  the job is done, answers exactly as the request would have: its status and
  body, errors included, e.g. a 429 when no worker took it in time. Until then
  it answers 202 with `status` `queued` or `running`. Any server answers for
  any job.
- An answer is kept for an hour after its job is done; then the job is a 404.
- `DELETE /jobs/{id}` withdraws a job no worker has started (`cancelled`); a
  `running` or `done` one stays as it is.
- Anyone with a job's id can read its answer or cancel it, so keep it secret.
  Build its URL from `id` rather than `Location`, which is relative to the
  server's root.
- `/check/batch`, and the snapshot reads and edits, answer at once whatever
  `Prefer` says: send a batch's items to `/check` instead.

A check result is `{ok, time, error?, messages, sorries, reasons, changes?, declarations}`:
- `ok` is the verdict.
- `messages` are Lean's, each `{severity, pos, end_pos?, text}`.
- `reasons` says why `ok` is false, e.g. `axiom: t uses sorryAx`.
- `changes` lists, for a [replacement](#replacing) or a session's final
  check, the declarations that don't keep the statements they must, each also
  one of `reasons`: `{name, module?, change, was?, now?, includes?}`, see
  *Refused* under [Replacing](#replacing).
- `declarations` lists each new declaration as `{name, kind, status,
  axioms}`, its axioms being those it uses besides the allowed ones (see
  [Statuses](#statuses)). With `detail: ["declarations"]` each has every
  field: all its `axioms`, the assumptions it uses (`assumes`), and the hashes
  and references the comparisons run on (`type_hash`, `value_hash?`,
  `type_refs`, `value_refs`, `generated?`, `parent?`). `/sessions/{id}/check`
  and `POST /snapshots` take the same `detail`, and `GET /snapshots/{id}`
  takes `?detail=declarations`.

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

<a id="positions"></a>
**Goals and types at positions.** `at` takes up to 64 positions in the code,
as `{line, column}` like those of `messages`: lines count from 1, and columns
from 0 in Unicode code points. A check result then has `at`, one entry per
position in request order, as `{pos, tactic?, term?}`:
- `tactic` is the tactic at the position, as an entry of `tactics`: its
  `goals_before` and `goals_after` are the goals there. There is none outside
  a tactic proof, nor on structure such as `by` or the space between two
  tactics; the earlier one's `goals_after` are the goals in that space.
- With `detail: ["terms"]` too, `term` is the term at the position that Lean
  elaborated against an expected type, as `{term, expected_type, expr, pos,
  end_pos}`, where `term` is the source text and `expr` the term as Lean
  elaborated it. Lean gives the head of an application the application's
  expected type: on `f` in `f x`, `term` is `f`, expecting what `f x` was
  expected to be.
- Both are the innermost of their kind written in the code: the range that
  starts last, then ends first. Of two nodes on one range, such as `rfl` and
  its head `@rfl`, the outer one is reported.
- On a batch, `at` applies to every item. `terms` without `at`, or more than
  64 positions, is a `bad_request`.

Goals cost what `detail: ["tactics"]` does: 16–79% more than a plain check on
the bench library. Terms make Lean print every term of the code, 1.9 to 6.2
times the check: seconds for a statement, but minutes for a 1,000-line chunk,
and within `timeout` (see `bench/infotree/2026-11-06/report.md`). Nothing is
kept between checks, so ask about every position in one: each check
elaborates the code again.

`version` selects a worker pool, by label (such as `v4.35.0-1a2b3c4d`) or by
an alias set with `PISA_ALIASES`. Session, state and snapshot ids carry their
version, so the requests that take one take no `version`. `timeout` is Lean's
limit in seconds (default 60).

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
| `bad_request`, `not_found` | 400, 404 | Malformed request; unknown or expired job, state or session; unknown snapshot or declaration |
| `conflict` | 409 | A create replaces a snapshot that another already replaces; in-band, in `check.error` (see [Replacing](#replacing)) |
| `internal` | 500 | A bug |

## Snapshots

A formalizer that builds a library statement by statement would otherwise
resend, and Lean re-check, the whole library on every compile. Instead it
keeps each accepted chunk as a snapshot and builds on it:

```text
POST /snapshots {"code": "import Mathlib\n\ndef double (n : ℕ) := 2 * n", "labels": {"run": "7"}}
  → {"snapshot": {"id": "v4.35.0-1a2b3c4d~9c0e…", "version": "v4.35.0-1a2b3c4d", "module": "PisaSnap.s1f…",
                  "header": "import Mathlib\nimport PisaSnap.s1f…", "imports": [], "labels": {"run": "7"},
                  "status": "proved", "verified": false, "created": 1762387200},
     "check": {"ok": true, …}}
POST /check {"code": "import Mathlib\nimport PisaSnap.s1f…\n\nexample : double 2 = 4 := rfl"}
```

**Snapshots and modules.** A snapshot has two names:
- **The module** (`module`, e.g. `PisaSnap.s1f…`) is what code imports. It
  comes from a hash of the code, so the same code is the same module, and its
  check, `status` and `verified` belong to it.
- **The id** (`id`, `<version>~<32 hex>`) is the snapshot's own: a reference
  to the module with its own `labels` and `created` time. Creating the same
  code again adds another snapshot of the same module without building it
  again. `GET`, `PATCH`, `DELETE` and `verify` take the id, which carries the
  version, so they take no `version`; a module name there is a `bad_request`.

A create that keeps nothing has no `snapshot`, and its `check` says why. That
includes `verify: true` on code that is already kept but fails the re-check:
`check.error` has the reason, as new code that fails the re-check isn't kept.
It also includes a header whose imports Lean reads differently from its lines,
such as an import after a block comment on the same line: the record would
miss what the module builds on, so it is a `bad_request` that asks for each
import on its own line.

`GET /snapshots/{id}` adds the module's `code`, its `check`, and its
`exports`: the declarations that code importing it sees, each `{name, kind,
signature, docstring?, description?}`. `description` is `{title, text,
latex?, aliases, source}`, which search writes when it has a describe model
(`PISA_SEARCH_DESCRIBE_MODEL`; see [Snapshot search](#snapshot-search)), a
little after the create.

<a id="labels"></a>
**Labels.** A snapshot's labels are string key/value pairs, at most 64 of
them. A key is 1–128 bytes of letters, digits, `.`, `_`, `/` and `-`; a value
is up to 512 bytes of UTF-8 without NUL characters. Labels that break these
rules are a `bad_request`: in `check.error` on a create, which then keeps
nothing, and a 400 on a `PATCH`, which then changes nothing.
- **Editing.** `PATCH /snapshots/{id}` with `{"labels": {"stage": "done",
  "draft": null}}` is a JSON merge patch: it sets `stage`, removes `draft`,
  keeps the other labels, and returns the snapshot as a listing shows it.
- **Filtering.** `GET /snapshots?label=project:foo&label=kind:lemma` lists the
  snapshots that have every label given, each an exact match of `key:value`
  split on the first `:`, so a value can contain `:`. Without `label` it lists
  every snapshot of the version. There is no OR or prefix matching. Two values for
  the same key match nothing, as a snapshot has one value per key. With
  `heads=true` it lists only heads, the snapshots that nothing replaces (see
  [Replacing](#replacing)); `heads=false`, the default, lists every snapshot.
- **Order and pages.** Snapshots come newest first, as summaries: what
  `GET /snapshots/{id}` returns without `code`, `check` and `exports`.
  `limit` is 1–1000 (default 100), and a listing takes up to 64 `label`s. Keep
  requesting while a page has `next`, an opaque cursor, passing it as `after`:
  a full page has `next`, and the page after it may be empty. A cursor is a position, so it
  keeps working when its snapshot is deleted.
- **Speed.** On PostgreSQL, labels are matched through an index (GIN), so the
  order of the `label`s doesn't matter.

**Retrying a create.** A create can carry an idempotency `key`, 1–256 bytes of
letters, digits, `.`, `_`, `:`, `/` and `-` (anything else is a `bad_request`
in `check.error`), so that a retry is safe:
- While a snapshot made with the key exists, creating the same code with it
  returns that snapshot as it is, without building anything. The retry's other
  fields are ignored, `labels` and `verify` included, so a retry with
  `verify: true` can return `verified: false` with no error.
- Different code under a used key is a `bad_request` in `check.error`, and
  nothing is kept.
- A create that keeps nothing, e.g. when Lean reports errors or a `verify: true`
  re-check fails, doesn't use the key up, so a later create can.
- Deleting the snapshot frees its key.

<a id="replacing"></a>
**Replacing a snapshot.** A create can name the snapshot of its version that
it `replaces`, e.g. the skeleton with `sorry`s that a proof fills in. The code
is kept only if it still declares everything the replaced snapshot's module
declares, each the same kind of declaration with the same type and, for
definitions, the same value; it may add declarations, such as helper lemmas,
and may still use `sorry`. So filling a `sorry` can't quietly change what is
proved:

```text
POST /snapshots {"code": "…theorem t : 1 = 1 := rfl", "replaces": {"snapshot_id": "v4.35.0-1a2b3c4d~9c0e…"}}
  → {"snapshot": {"id": "v4.35.0-1a2b3c4d~4b7a…", …, "replaces": {"snapshot_id": "v4.35.0-1a2b3c4d~9c0e…"}},
     "check": {"ok": true, …}, "replacement": {"filled": ["t"]}}
```

- **No new assumptions.** A replacement can't fill a `sorry` by assuming it:
  a theorem stays a theorem, and every `axiom` the code declares, or that a
  snapshot it builds on and the replaced module doesn't declares, must already
  be declared, with the same statement, by the replaced module or a snapshot
  it no longer builds on (`statement: the code adds the assumption helper`).
  The replaced module's own assumptions may stay. To add one on purpose,
  create the code without `replaces`.
- **Refused.** Code that changes a statement is not kept, and has no
  `snapshot`: `check.ok` is false and `check.reasons` says what changed, e.g.
  `statement: t has a different type`. `check.changes` says the same of each
  declaration, `{name, module?, change, was?, now?, includes?}`:
  - `change` is `missing`, `kind`, `type`, `value`, `derived`,
    `added_assumption`, `assumption` (an assumption with another statement)
    or `ambiguous`;
  - `module` names the snapshot a constant is from, when the code no longer
    builds on it (see *Imports*);
  - `was` and `now` are its signature before and after, for a changed type or
    assumption, from the modules' search exports. Those are what the builds
    printed; the comparison reads the declarations' hashes;
  - what Lean makes for an inductive type (its constructors, recursor,
    `casesOn`, `injEq`, projections and the like) is one change, to that
    type, whose `includes` lists them: `derived` unless the type itself
    changed.

  To change a statement on purpose, create the code without `replaces`.
- **Only a head.** A create may replace only a head, a snapshot that nothing
  replaces. Otherwise nothing is built or kept: `check.error` is a `conflict`
  that names what replaces it, and `heads` lists the snapshots to replace
  instead, those nothing replaces that are reached from the replaced one
  through what replaces it, oldest first. A create that loses a race to
  another during its build is refused the same way. `"fork": true` skips the
  check, for parallel attempts at one target, and a fork is reported by the
  [spec audit](#spec-audit). A key replay comes first and returns its
  snapshot, whatever replaced it since.
- **Imports.** The code may import more, and may build on other snapshots
  than the replaced snapshot's module does. A type names the constants it uses,
  such as a definition from an imported snapshot, without saying what they
  are, so swapping that snapshot could change what a statement means while
  every hash stays the same. So for each snapshot the code no longer builds
  on, the comparison follows what the replaced module's declarations use
  there, through types and definitions' values, and each constant it reaches
  must be the same, by name and hashes, in what the code builds on now, e.g.
  `statement: foo, from PisaSnap.s3c…, has a different value` otherwise. It
  stops at Lean's and Mathlib's constants, which the version fixes.
- **What declarations use.** Each declaration in a check result records the
  constants of the code and of its snapshots that its type and value use
  (`type_refs`, `value_refs`, with `detail: ["declarations"]`). Private names are compared without the module
  they were declared in, and what Lean makes on demand, such as equation
  lemmas (`generated`), needn't stay.
- **Definitions.** `"replaces": {"snapshot_id": "…~9c0e…", "allow":
  {"definitions": ["helper", "bound"]}}` lets the named definitions change
  their value but not their type, e.g. to add a missing hypothesis; any other
  definition that changes is refused (`statement: g has a different value`).
  It may also name an inductive type, structure or class: its constructors and
  what Lean makes from them may then change, e.g. to add a field, but not its
  own type. What Lean makes from it is told by name and shape (a constructor,
  `casesOn`, `injEq`, a projection named after its field), so a declaration
  the code names `S.below` isn't one, and `changed_definitions` lists, after
  the type, each of them that changed.
  A theorem's type only names a definition, so its meaning can change while
  its type matches: `replacement` in the response lists the
  `changed_definitions` and the theorems of the new module whose type uses
  one, directly or through another definition (`affected`).
  - **What may be named.** A definition (a declaration with a value) or an
    inductive type of the replaced snapshot's module, or of a snapshot the
    code no longer builds on (see *Imports*). A name is compared without a private one's module, so
    `private def h` is `_private.h`, or the full name a refusal gives,
    `_private.PisaSnap.s….0.h`, but not `h`.
  - **Errors.** An empty list is a 400 `bad_request`: to change no
    definition, leave `allow` out. A name that isn't such a declaration is a
    `bad_request` in `check.error` (`allow names nope, which isn't a
    definition or inductive type of the replaced snapshot or of a snapshot
    the code no longer builds on`), found before the code is built, and
    nothing is kept. For
    `h` where a `private def h` would match, it adds `(for a private
    definition, write _private.h)`.
- **Holes.** A spec snapshot may leave definitions for the snapshots that
  replace it to change: `"holes": ["program", "cost"]` at its create, named
  as `allow` names them, each a definition or inductive type of its module
  or of a snapshot it builds on (else a `bad_request` in `check.error`, `holes
  names nope, which isn't a definition or inductive type of the snapshot or
  of a snapshot it builds on`, and nothing is kept). A snapshot that
  replaces it inherits them, as `holes` in `GET /snapshots/{id}` shows, and
  may change them without `allow`; `changed_definitions` lists them as it
  lists what `allow` lets change. The [spec audit](#spec-audit) allows them
  to change, and `allow`s nothing else the leader didn't. `holes` is a 400
  with `replaces`, since a replacement inherits the replaced snapshot's, and
  when empty.
- **The link.** `replacement.filled` lists the declarations that used `sorry`
  and no longer do. The new snapshot records `replaces: {snapshot_id,
  allow?}`, which listings and `GET /snapshots/{id}` show. It is set by a
  create that passed the comparison, or composed when the snapshot between is
  deleted (see [Retention](#retention)). That comparison ran on what the
  create's build reported, so acceptance doesn't rest on it: the
  [spec audit](#spec-audit) compares again from verified records. A key
  replay returns the snapshot as it is, without `replacement`.

<a id="witnesses"></a>
**Witnesses.** A target whose hypotheses can't hold together can be proved
vacuously, and an allowed definition change can make them so. By convention,
a skeleton states next to each such target a witness that its hypotheses can
hold:

```lean
theorem t (x : ℕ) (h : Pre x) : Post x := sorry
theorem t_nonvacuous : ∃ x, Pre x := sorry
```

`replaces` freezes the witness like any statement, so every replacement keeps
it, and it is a target of the [spec audit](#spec-audit) like `t`. If a change
that `allow` lets through makes `Pre` impossible, the witness can no longer be
proved: the audit shows it unproved, and the run isn't `clean`.

<a id="statuses"></a>
**Statuses and assumptions.** An `axiom` declared in snapshot code, or in the
code being checked, is an assumption: a statement taken as given, such as a
lemma from the literature. Each declaration in a check result has a `status`,
and `assumes` lists the assumptions it uses (with `detail: ["declarations"]`;
the summary's `axioms` show them too):

| `status` | Axioms it uses besides the allowed ones (`propext`, `Classical.choice`, `Quot.sound`) |
| --- | --- |
| `proved` | None |
| `conditional` | Assumptions only |
| `sorry` | `sorryAx`, with or without assumptions |
| `rejected` | Any other: the one `native_decide` declares, `Lean.trustCompiler`, a library's |

A snapshot's `status`, on creates, listings and `GET /snapshots/{id}`, is the
worst of its module's declarations', in that order, and its check is `ok` only
if that is `proved`.

`GET /snapshots/audit?label=project:foo` says what the snapshots with the
labels rest on, from their records, without running Lean:
- `summary` counts their declarations by status, each module's once, without
  the assumptions themselves and what Lean generates, and `unverified_modules`
  counts the modules not yet re-checked by the kernel, whose statuses come
  from the build's report.
- `assumptions` lists each assumption they use, wherever it is declared, with
  its `signature`, a snapshot of its module (`snapshot_id`), whether it is
  `verified`, and `used_by`.
- `not_proved` lists the declarations that are `sorry` or `rejected`, each
  with a snapshot of its module (`snapshot_id`) and its axioms.

A trusted audit has no `rejected` or `sorry` and no `unverified_modules`.
Whether each assumption is reasonable is for review: `axiom oops : False`
makes everything above it `conditional`.

<a id="spec-audit"></a>
**Auditing a run against its spec.** A run starts from spec snapshots,
skeletons whose statements are its targets, and goes on through the snapshots
that [replace](#replacing) them. `GET /snapshots/audit?against=<id>` (repeated
for a spec spread over several snapshots) audits it from its records:

```text
GET /snapshots/audit?against=v4.35.0-1a2b3c4d~9c0e…
  → {"summary": {"proved": 1, "conditional": 0, "sorry": 0, "rejected": 0, "unverified_modules": 0},
     "assumptions": [], "not_proved": [], "clean": true, "heads": ["v4.35.0-1a2b3c4d~4b7a…"],
     "targets": [{"name": "t", "spec": "v4.35.0-1a2b3c4d~9c0e…", "head": "v4.35.0-1a2b3c4d~4b7a…",
                  "status": "proved", "verified": true, "accepted": true}],
     "comparisons": [{"spec": "v4.35.0-1a2b3c4d~9c0e…", "head": "v4.35.0-1a2b3c4d~4b7a…",
                      "replacement": {"filled": ["t"]}}]}
```

- **The walk.** From each spec snapshot it follows the snapshots that replace
  it, whatever their labels, to the run's `heads`, which nothing replaces. A
  snapshot replaced more than once is a fork, listed in `forks` with what
  replaced it: there is no merge, so delete all but one branch. A deleted
  snapshot on the way doesn't cut the run, as what replaced it now replaces
  what it replaced; deleting a head makes what it replaced a head again,
  unless something else replaces it. To drop a losing branch,
  `DELETE /snapshots/{id}?with_replacers=true` on the snapshot `forks` names
  for it: it takes the branch with it, and `deleted` lists what went. A plain
  delete of that snapshot moves the rest of the branch down, and the fork
  stays.
- **Targets.** Each declaration of a spec snapshot's module, other than its
  assumptions and what Lean generated, is a target. `targets` has a row for
  each target and each head reached from its spec snapshot, with its `status`
  there, whether the head is `verified`, and whether the target is
  `accepted` there.
- **Comparisons.** The audit doesn't take a link's word that the head keeps
  the spec's statements: that word is what a create's build reported, and the
  code being judged runs in that build. For each spec snapshot and each head
  it reaches, `comparisons` has the comparison a create replacing the spec
  snapshot with the head's code would run (see [Replacing](#replacing)), on
  their records once the kernel has re-checked both, allowing only the
  definitions the leader names to change: `reasons` and `changes` if the
  head doesn't keep the spec's statements, as a refused create says them, or
  else `replacement`, what it filled and what allowed changes it made. For
  review, these are what the head changed, whatever its links say.
- **What may change.** A spec snapshot's [holes](#replacing), and the
  definitions `allow=<name>`, repeated, names: those the leader lets the
  heads change, as `allow` names them in `replaces` (`_private.h` for a
  private one; an inductive type for its constructors and what Lean derives
  from it), such as one its own base edit changed. A link's own `allow` is
  what its create asked for, written by whoever submitted it, so it doesn't
  count: a head that changed anything else is shown with `reasons` and isn't
  `clean`. A statement only names the definitions it uses, so redefining one
  would change what it says.
- **`clean`.** There is no fork, no spec snapshot was copied from one that
  is gone (see *Parameters*), every comparison found the spec's statements
  kept with no definition changed but its holes and those `allow` names (a spec of
  assumptions only has no targets, but statements all the same), and every
  target is `accepted` on its head: the
  spec snapshot and the head are verified, their comparison found the
  spec's statements kept, and the target is `proved`, or `conditional` only
  on assumptions the spec permits, those declared in a spec snapshot's module
  or in a module it builds on. The records of a module the kernel hasn't
  re-checked come from its build's report, which the code being judged could
  influence, so they don't count. Each of `assumptions` says whether the spec
  permits it (`in_spec`).
- **Links.** For review, `links` lists every link that let definitions change
  (with an `allow`, or filling holes): those on the way to the heads, and those of each spec
  snapshot's own chain back through what it replaced, as far as those
  snapshots exist, such as a rebuilt spec's link to the original, which
  allows what `to` changed under it. Each is `{snapshot_id, replaces: {snapshot_id, allow},
  replacement: {filled, changed_definitions?, affected?}}`: the link as the
  snapshot's own `replaces` shows it, and what its comparison found: its
  create's, or the union of two if the snapshot between was deleted. Those
  comparisons are the builds' reports; `comparisons` holds what the audit
  found from verified records.
- **Counts.** `summary`, `assumptions` and `not_proved` are those of the
  heads, each standing for its module.
- **Parameters.** The ids are of one version, which the audit uses; ids of
  several versions are a 400 that names them, and so is a `version` that
  isn't theirs. `label` doesn't combine with `against` (400), nor does
  `allow`, which can't be empty either, and a spec snapshot that doesn't
  exist is a 404. A [rebuild](#rebuilding) copies
  only a run's heads, so audit against the same ids after it. A spec
  snapshot that was still a head, with nothing replacing it yet, is copied
  like any head, and with `retire` its old id is deleted if its copy replaces
  it. Its copy in `created` then replaces a snapshot that is gone, so nothing
  shows that it states what the original did, and an audit against it isn't
  `clean`: rebuild a run's spec snapshots without `retire`.

<a id="agent-runs"></a>
**Running agents on snapshots.** A leader hands targets to agents, and
[replacing](#replacing) keeps their submissions in one run:
- **The leader.** It creates the spec snapshots, skeletons with `sorry` and
  one or a few targets each, with their [witnesses](#witnesses), with
  `verify: true` (see [Verifying](#verifying)) and, where agents are to fill
  in definitions as well as proofs, with those definitions as `holes` (see
  [Replacing](#replacing)), labels the
  run (see [Labels](#labels)) and holds the spec ids. They stay valid through
  [rebuilds](#rebuilding) without `retire`.
- **Agents.** Each gets its target's spec id and labels every snapshot with
  the run's labels. Scratch work goes to `/check` (see
  [Snapshots](#snapshots)), which keeps no snapshot. To submit, an agent
  creates its code with `replaces` the snapshot it last submitted for the
  target, or the spec snapshot at first. [Search](#snapshot-search) shows
  heads only, so what an agent imports from a hit is a head.
- **On a `conflict`** (see [Replacing](#replacing)). The run moved under the
  agent: a rebuild copied its last version onto a new base, or another agent
  extended it. Nothing was built. The agent takes a head from the answer's
  `heads`, moves its code onto it, its imports becoming those of the head's
  `code` (`GET /snapshots/{id}`, see [Snapshots](#snapshots)), and submits
  again, replacing that head.
- **Parallel attempts.** Agents on one target pass `"fork": true` on their
  first submission and extend their own branches. The leader keeps one and
  drops the others with `DELETE /snapshots/{id}?with_replacers=true`, on the
  ids [`forks`](#spec-audit) names (see [Retention](#retention)). Every fork
  in the audit is one of these.
- **Re-checks.** When something below the run changes, such as a helper
  proved, the leader [rebuilds](#rebuilding) without `retire` while agents
  work. Agents meet the copies as `conflict`s and move onto them. A rebuild
  item that loses a race to an agent's submission fails; send the rebuild
  again and it copies the agent's version. The run doesn't fork, and no
  submission is lost. `retire` would delete versions agents hold, so their
  submissions would answer `not_found` without `heads`.
- **Accepting.** The run is accepted when `audit?against=` its spec ids is
  `clean` (see [Auditing a run](#spec-audit)), with `allow=` each definition
  the leader decided later may change, such as one its own base edit
  changed; the spec's holes may change already. What the agents' links
  allowed doesn't count; `comparisons` and `links` show what each head
  changed, for review, holes' values included.
- **Afterwards.** The leader may retire and delete superseded versions.
  Deletes splice, so the run stays whole, and its links merge once reviewed
  (see [Retention](#retention)).
- **On a `not_found`** for its `replaces`, the create lost a race with a
  delete (see [Retention](#retention)). The agent finds the run's heads with
  `audit?against=<its spec id>` (see [Auditing a run](#spec-audit)) and goes
  on as for a `conflict`.

<a id="rebuilding"></a>
**Rebuilding dependents.** A module's name is a hash of its code, imports
included, so replacing B with B′ leaves every chunk above B importing B.
`GET /snapshots/{id}/dependents?label=project:foo` lists the snapshots of
every module that imports the snapshot's module, directly or through others,
parents first: by level (1 imports it, 2 imports a level 1, and so on, by the
longest chain), then oldest first. With `label` filters, as in a listing, only
the snapshots with every label.

`POST /snapshots/rebuild` copies their heads onto the replacement, or with
`"dry_run": true` only plans how:

```text
POST /snapshots/rebuild {"from": "…~9c0e…", "to": "…~4b7a…", "labels": {"project": "foo"}, "dry_run": true}
  → {"ok": true, "changes": [], "plan": [
       {"level": 1, "snapshot_id": "…", "module": "PisaSnap.sC…", "new_module": "PisaSnap.sC′…",
        "new_header": "…", "code": "…", "labels": {…}, "key": "rebuild:…"}, …]}
```

- **What it rebuilds.** The dependents with a head that has every label
  (`labels` is required, so the caller chooses which projects follow the
  edit), and the modules they build on above `from` (see *Support modules*).
  A head is a snapshot that nothing replaces, with the labels or without: a
  replaced one is history, and its run goes on through what replaced it. A
  snapshot that only its copies from this same rebuild replace stays an item,
  so sending the rebuild again replays them. Each item's `code` is the old
  code with its imports of `from`'s module, and of the other modules the plan
  rebuilds, swapped for the new ones; lines and columns stay where they were.
  `from` and `to` are of one version, which the rebuild runs on.
- **Support modules.** A module that is rebuilt only because a copy builds on
  it, such as one between the heads and `from`, or an obsolete snapshot's
  that a head still imports, is a support item: no `snapshot_id`, no
  `labels` and no `replaces`. The rebuild creates it so that the items above
  can build, then deletes its snapshot. Its module stays only while a created
  snapshot builds on it, and is in no listing, search or audit, so old code
  doesn't come back as a head. One whose dependents all failed goes with its
  snapshot. If the server stops mid-run, the support snapshots stay until the
  rebuild is sent again, which replays and deletes them. A delete that fails,
  of a support snapshot or, with `retire`, an old one, doesn't lose the
  answer: it is in `not_deleted` as `{snapshot_id, error}`, `ok` is false,
  and sending the rebuild again deletes it.
- **Runs that left the labels.** A snapshot with the labels that one
  without them replaces isn't copied, as its run went on elsewhere: on dry
  runs and runs alike, `replaced` lists it as `{snapshot_id, replaced_by}`,
  with those replacers. Rebuild that run with its heads' labels. It doesn't
  change `ok`.
- **Levels.** An item's `level` is the wave its create runs in: one more than
  the highest among the items whose modules it imports, so 1 for one that
  imports only `from`'s module.
- **Running it.** Without `dry_run`, the server runs the plan: a level at a
  time, up to 8 of a level's creates side by side once the level below is
  done, each with the item's `code`, `labels`, `key` and `replaces`, and the
  request's `verify` and `timeout`. A create no worker takes in time is sent
  again, up to 5 times. The response adds what it did: `created` (each old
  `snapshot_id` and `module` with its `new_snapshot_id` and `new_module`; a
  support item has the modules only), `failed` (with the create's `check`),
  and `skipped`, the items that build on one that failed or was skipped; other
  branches go on, and `ok` says whether every item was created.
- **Retiring.** With `"retire": true`, once every item is created, the server
  deletes the old snapshots whose copies replace them, from the top down, and
  with them the old modules nothing else needs; `retired` lists the
  snapshots' ids. Each delete splices the run (see [Retention](#retention)),
  so a skeleton S0 and its proof P1, with P1 copied as P1′, read S0 ← P1′.
  An old snapshot whose copy has no link stays, and its run with it. `from`
  stays: when `to` replaces it, it is part of the run's history, such as the
  skeleton a proof replaced. Delete it yourself if nothing needs it.
- **Sending it again.** The keys come from the plan, so sending the same
  rebuild again, after a failure, a timeout or a lost server, replays the
  creates that are done and runs the rest. An item is a create without
  `fork`: if an agent replaced its snapshot after the plan was made, the item
  fails with a `conflict`, and sending the rebuild again plans the agent's
  head instead.
- **Later edits.** A rebuild follows the run's heads. After one from `from` to
  `to` without `retire`, the originals are replaced by their copies on `to`,
  so a later edit is rebuilt from `to`, the base the heads now build on: a
  rebuild from `from` would find nothing left to copy.
- **Async.** A run can take minutes, so it takes `Prefer: respond-async` (see
  [Async requests](#async-requests)): the 202's job, of kind
  `rebuild_snapshots`, is run by the server that answered, and
  `GET /jobs/{id}` on any server answers with the response once it is done.
  If that server stops first, the job reads as lost (503): send the rebuild
  again. `DELETE /jobs/{id}` leaves a running rebuild running. A dry run
  answers at once.
- **Running it yourself.** Or take the plan and send each item as
  `POST /snapshots {code, labels, key, replaces?}`, a level at a time, the
  same way; each create's `snapshot.module` is the item's `new_module`. A
  support item has no labels: once the items above it are created, delete its
  snapshot with `DELETE /snapshots/{id}`.
- **Statements.** If `to` replaces `from` (see [Replacing](#replacing)), its
  create compared them, so `changes` is empty. Otherwise the rebuild compares
  them the same way and, if `to` changes what `from` states, answers `ok:
  false` with the `changes` and no plan, unless `allow_changes` is set.
- **Changed definitions.** When `to` changes a definition's value (one its
  `allow` names, or with `allow_changes`), `changed_definitions` lists
  them, and each item lists its theorems whose type uses one, directly or
  through definitions here or below, as `affected`. That is read from what
  the modules' declarations use.
- **Links.** Each copy replaces the snapshot it copies, so a run's history
  carries over, and a run's spec snapshots keep their ids. Before anything
  is built, the plan runs the comparison the copy's create will run (see
  [Replacing](#replacing)), the copy and the new modules it builds on taken
  to declare what their originals do, and every definition allowed to
  change its value. If it passes, the item's `replaces` is `{"snapshot_id":
  …, "allow"?}`, where `allow` names the definitions it found changed, such
  as one of `to`'s that the copy's statements use; with none, there is no
  `allow`. If it fails, as when `to` changes the type of something the
  copy's statements use, the item has no `replaces`: a changed statement is
  a new spec, for a person to set. The copy then belongs to no run, and the
  old run stays as it was, even with `retire`. To start a new run, create
  the skeleton again on the new base, then the copy's code replacing it.
- **Limits.** At most 1000 items, as the plan carries code.

**How it works.**
- **Building.** A module is its code compiled with `lean -o` to the module
  `PisaSnap.s<hash>`, where `hash` is a hash of the code.
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
  re-check passes (see [Verifying](#verifying)). `check` is a check result, as
  for `/check`, without `sorries`.
- A chunk with `sorry` or another axiom outside the allowed list is kept, with
  that `status` (see [Statuses](#statuses)), and checks that use it report the
  axiom.
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
| `true` | This chunk and any unverified imports; the snapshot is `verified`, or `check` has an `error` |
| `false` | None, even with macros |
| any, module-system code | This chunk, as for macros: the build's report sees only what the module exports |

`verified` means the snapshot and everything it imports have passed the
re-check. So a macro chunk on top of unverified snapshots is re-checked but not
yet `verified`.

`POST /snapshots/{id}/verify` re-checks the snapshot's module and everything it
imports, imports first, and records each module that passes. A later run only
covers new chunks, and a retry resumes where the last one stopped. `verified`
and `status` belong to modules, so every snapshot of a module shows the result.
- `snapshot` is the snapshot as the re-check left it, and `checked` lists the
  modules this run re-checked and passed.
- If the kernel rejects a module or the re-check times out (60 s per module),
  the run stops, still with a 200, and `failure` has the module and the reason.
  Nothing is deleted, because other code may import it: the module stays usable
  and not `verified`. Delete its snapshots if you don't want it.
- `status` and the module's `check` are updated from the re-check, whose axioms
  don't depend on the build being honest.

Check results don't say whether the snapshots they import are verified: look
at the snapshot, or verify it, when that matters.

<a id="retention"></a>
**Retention.** A snapshot is kept until it is deleted. A module is kept while
a snapshot of it exists or a kept module imports it: deleting a snapshot
removes its module once nothing else needs it, then each of that module's
imports on the same terms, and `removed` lists them. So deleting a snapshot
whose module others build on removes nothing yet, and the code on top keeps
working. Deleting a snapshot that [replaced](#replacing) another re-points
what replaced it at that one, with the two links composed: `allow` names
what either link allowed, and `replacement` holds the changes both steps
found, so a run isn't cut. A snapshot with no link, such as a spec snapshot,
leaves its replacers' links as they are, pointing at a snapshot that is gone:
the [audit](#spec-audit) of a spec copied from it isn't `clean`. With `?with_replacers=true` a delete takes the snapshot and every
snapshot that replaces it, directly or through others, in one step and
splices nothing; what the snapshot replaced is a head again if nothing else
replaces it, and `deleted` lists every snapshot that went, the asked one
first (a plain delete lists one). A create that replaces a snapshot deleted
while it was being built answers with a `not_found` in `check.error`. Each
worker checks a module it has cached against the store at most every
`PISA_SNAPSHOT_CONFIRM_SECS` (60), so a removal takes effect everywhere within
that time. Past a snapshot's own entry, a delete is best effort: a worker
that stops partway leaves modules nothing needs, and files in the store.
Every hour one worker of each version sweeps them: the modules no snapshot
is on and no module imports go as a delete would remove them, then the
files no module's record names, once a day old, as a build uploads its files
before its record.

**Trust.** A snapshot that isn't `verified` has the same guarantee as a
`/check`: the policy scan and an axiom report from the process that imported
it. Macros go beyond that, because what they expand into isn't in the text the
scan sees, so with `verify: false`, `ok` and `status` can be wrong until the
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
// The tactic at line 2, column 30 (`norm_num`) and its goals; see Goals and types at positions.
let at = vec![pisa_client::Pos { line: 2, column: 30 }]; // `Detail::Terms` in `detail` adds terms
let opts = pisa_client::CheckOptions { at, ..Default::default() };
let goals = client.check_with("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num", &opts).await?.at;
let session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry").await?;
let step = client.apply_tactic(&session.states[0].id, "simp").await?; // Error::Server { kind: Lean, .. } if it fails
let verdict = client.check_session(&session.id, "theorem t (n : Nat) : n + 0 = n := by simp").await?;
let base = client.create_snapshot("import Mathlib\ndef double (n : ℕ) := 2 * n").await?;
let base = base.snapshot.expect("kept"); // none if nothing was kept: `check` says why
let next = client.check(&format!("{}\nexample : double 2 = 4 := rfl", base.header)).await?;
let run = std::collections::BTreeMap::from([("run".to_string(), "7".to_string())]);
let page = client.list_snapshots(&run, Some(50), None, false).await?; // newest first
let more = client.list_snapshots(&run, Some(50), page.next.as_deref(), false).await?;
let heads = client.list_snapshots(&run, None, None, true).await?; // only what nothing replaces
// A merge patch: `Some` sets a label, `None` removes it, and the rest stay.
let done = std::collections::BTreeMap::from([("stage".to_string(), Some("done".to_string()))]);
let snapshot = client.update_snapshot(&base.id, &done).await?;
let verified = client.verify_snapshot(&base.id).await?;
// Kept only if it keeps the replaced snapshot's statements; see Replacing.
let replaces = Some(pisa_client::Replaces { snapshot_id: base.id.clone(), allow: None });
let opts = pisa_client::CreateSnapshotOptions { replaces, ..Default::default() };
let lemma = "import Mathlib\ndef double (n : ℕ) := 2 * n\ntheorem double_two : double 2 = 4 := rfl";
let extended = client.create_snapshot_with(lemma, &opts).await?.snapshot.expect("kept");
let above = client.list_dependents(&base.id, &run).await?; // parents first
let audit = client.audit_snapshots(&run).await?; // statuses, assumptions, what isn't proved
let audit = client.audit_snapshots_against([&base.id], &[]).await?; // the run from base: audit.clean, heads, targets
let opts = pisa_client::RebuildOptions { dry_run: true, ..Default::default() };
// Create each item, a level at a time, with its `replaces`.
let plan = client.rebuild_snapshots(&base.id, &extended.id, &run, &opts).await?.plan;
let opts = pisa_client::RebuildOptions::default(); // `retire: true` deletes the originals the copies replace, not `base`
let job = client.submit_rebuild_snapshots(&base.id, &extended.id, &run, &opts).await?;
let done = client.wait_job(&job).await?; // the server ran the plan: done.created, done.failed
let removed = client.delete_snapshot(&base.id).await?.removed; // the modules that went with it
let opts = pisa_client::DeleteSnapshotOptions { with_replacers: true };
let dropped = client.delete_snapshot_with(&extended.id, &opts).await?.deleted; // it and what replaces it
// A long request as a job (see Async requests), answered however long it takes.
let job = client.submit_check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num").await?;
let result = client.wait_job(&job).await?; // what check would have returned
```

Python (`pisa_client`), built from `crates/pisa-python` with `maturin build` or `maturin develop`:

```python
from pisa_client import Client, LeanError

client = Client("http://localhost:8000", timeout=60)
client.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num")["ok"]
client.check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num", at=[(2, 30)])["at"]   # norm_num's goals; detail=["terms"] adds terms
session = client.start_session("theorem t (n : Nat) : n + 0 = n := by sorry")
step = client.apply_tactic(session["states"][0]["id"], "simp")   # raises LeanError if it fails
client.check_session(session["id"], "theorem t (n : Nat) : n + 0 = n := by simp")["ok"]
base = client.create_snapshot("import Mathlib\ndef double (n : ℕ) := 2 * n", labels={"run": "7"})["snapshot"]
client.check(base["header"] + "\nexample : double 2 = 4 := rfl")["ok"]
page = client.list_snapshots({"run": "7"}, limit=50)   # newest first; pass page["next"] as after=
heads = client.list_snapshots({"run": "7"}, heads=True)   # only what nothing replaces
client.update_snapshot(base["id"], {"stage": "done", "run": None})   # a merge patch: None removes a label
client.verify_snapshot(base["id"])["snapshot"]["verified"]
more = "import Mathlib\ndef double (n : ℕ) := 2 * n\ntheorem double_two : double 2 = 4 := rfl"
extended = client.create_snapshot(more, replaces={"snapshot_id": base["id"]})["snapshot"]   # "allow": {"definitions": ["double"]} lets double change
client.list_dependents(base["id"], {"run": "7"})["snapshots"]   # parents first
client.audit_snapshots({"run": "7"})["summary"]   # by status: proved, conditional, sorry, rejected
client.audit_snapshots(against=[base["id"]])["clean"]   # the run from base, to its heads; see Auditing a run
plan = client.rebuild_snapshots(base["id"], extended["id"], {"run": "7"}, dry_run=True)["plan"]   # create each item, a level at a time, with its replaces
done = client.wait_job(client.submit_rebuild_snapshots(base["id"], extended["id"], {"run": "7"}))   # retire=True deletes the originals the copies replace, not base
client.delete_snapshot(base["id"])["removed"]   # the modules that went with it
client.delete_snapshot(extended["id"], with_replacers=True)["deleted"]   # it and what replaces it
job = client.submit_check("import Mathlib\ntheorem t : 2 + 2 = 4 := by norm_num")
client.wait_job(job)["ok"]   # what check would have returned, or raised
```

Calls are synchronous. They release the GIL, so one `Client` can be shared
across threads. Results are plain dicts; the type stubs describe their shapes.
Exceptions carry `.kind` and `.status`, and their class says what to do:
- `LeanError`: Lean or the policy refused the input.
- `LimitError`: the input hit a timeout, the memory limit, or crashed Lean.
- `CapacityError`: no worker was free, or the server is unavailable; retry later.
- `NotFoundError`: unknown or expired job, state or session, or unknown
  snapshot or declaration.
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
const step = await client.applyTactic(session.states[0]!.id, "simp");
```

React (`@aqora/pisa-react`, in `web/packages/react`) has hooks over TanStack
Query: `useCheck` and `useLiveCheck` for an editor, `useProofSession` for a
tree of tactics, the snapshot queries and mutations, `useSearch`, `useResolve`
and `useDeclaration`. The packages' READMEs have examples.

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
project's version label as `version`, so the declarations it returns exist in
the matching worker pool.

**How it runs.** The index lives in [Qdrant](https://qdrant.tech) (≥ 1.19.1):
a collection `pisa-<env_id>` with a point per declaration, loaded from a bundle
with `pisa-search load`. The servers search when given `--qdrant URL --env-id
ENV` (`PISA_SEARCH_QDRANT`, `PISA_SEARCH_ENV_ID`), and hold nothing, so any
number of replicas serve one environment. Each collection has one shard, as
Qdrant's IDF is per shard; for availability, run Qdrant as a cluster and load
with `--replication 2`. A server opens its index in the background, so until
Qdrant answers with the bundle, `/search` answers 503, `/health` says why,
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
  somewhat worse; see `search/bench/2026-10-22/report.md`. A declaration is
  embedded as its kind and name, its description (or docstring) and its
  signature, in at most 2,000 characters: the description is cut so that the
  signature fits.

Qdrant's weights scale a lane's ranks rather than its scores: a weight below 1
makes a lane count only for its first few hits. The tuned defaults (`Weights`
in `crates/pisa-search/src/engine.rs`) lean on the dense lane, with lexical and
structural matches lifting their top hits. PageRank over the dependency graph is
a prior, added after fusion, and a name query also favours names close to it by
edit distance. Declarations that depend on `sorry` are hidden unless
`include_sorried` is set, which it is by default when `snapshots` is, so that
agents find the skeleton's lemmas before they are proved; so are Lean-generated helpers (`….mk.injEq`) and
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
- Its prompt has a glossary of how to read names many declarations use
  (`search/glossary/`): Mathlib's for every declaration, and Physlib's,
  QuantumInfo's terms included, for Physlib, PhyslibAlpha, QuantumInfo and
  snapshot declarations. A changed glossary doesn't change the content hash:
  `informalize --force --libraries …` describes those again.

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
gh release download index-v4.36.0-rc2-1e541805 -R aqora-io/pisa -p bundle.tar.zst -O - \
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
# After a version bump, keep the corpus the store's descriptions were made for.
mv search/corpus search/work/corpus-previous
# Export the pinned project's declarations (about 30 minutes).
docker build -f docker/Dockerfile --target corpus --output search/corpus .
# Keep descriptions of declarations unchanged but for docstring whitespace, which
# Lean releases reformat (for v4.36.0-rc2 it kept 329k descriptions out of 423k).
pisa-search carry --from search/work/corpus-previous --corpus search/corpus --informal search/store
# Reuse LeanSearch v2's Mathlib descriptions (downloads 332 MB, verified by sha256).
pisa-search import-lsv2 --corpus search/corpus --file search/lsv2.jsonl --informal search/store
# Describe everything else with Claude (needs ANTHROPIC_API_KEY), and embed every
# declaration as its description lands (needs SCW_SECRET_KEY). Try --sample 500 first.
LIBS=Mathlib,Physlib,PhyslibAlpha,Batteries,Init,Std,QuantumInfo
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
| `POST /search` | `{queries: [{text?, description?, latex?, pattern?, goal?, name?, libraries?, kinds?, limit?, include_sorried?, snapshots?}], version?}` → `{version, results: [{hits, warnings?, dense_pending?}]}`, at most 64 queries |
| `POST /resolve` | `{names, version?}` → `{version, results: [{name, exists, deprecated_to?, suggestions}]}` |
| `GET /declarations/{name}` | `?module=&version=` → `{version, declaration}`: signature, description, flags, dependencies and users; with a snapshot hit's `module`, that snapshot's declaration, else the bundle's |

Each hit carries:
- name, kind, library, module and signature;
- a description's `title` and `summary`, when one exists;
- flags: `uses_sorry`, `informal`, `deprecated_to`, `additive_sibling`;
- a score, comparable only within one query, and a source link;
- `text`, about 150 tokens ready for a model's context;
- for a snapshot declaration, `snapshot_ids` (the scope's heads on its
  module), `status` and `verified` (see [below](#snapshot-search)).

Unknown names and empty results are answers, not errors. A `libraries` or
`kinds` filter matches case-insensitively; an unknown one is a 400 that lists
the known ones. A `version` names the index's version or its snapshots', or
an alias of one; any other is a 404. If the
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
from pisa_client import Client

client = Client("http://localhost:8000")
for hit in client.search(description="trace of a density matrix is one", libraries=["Physlib"])["hits"]:
    print(hit["text"])
client.resolve(["Real.sqrt_mul_sefl"])[0]["suggestions"][0]["name"]  # "Real.sqrt_mul_self"
```

The Rust and TypeScript clients have the same methods. The search tests,
Rust and Python, need a Qdrant at `PISA_TEST_QDRANT_URL` and are skipped
without one; for the Python ones, first run
`cargo build -p pisa-server -p pisa-search`.

<a id="snapshot-search"></a>
**Snapshots in search.** The servers also search the declarations of
[snapshots](#snapshots), from the database they share with the workers, ranked with
the bundle's in every lane, unless `PISA_SEARCH_NO_SNAPSHOTS` is set:
- **Scope.** A query's `snapshots` says which: `"none"` (the default), `"all"`,
  or `{"labels": {"run": "7"}}`, the modules of the snapshots that have every
  label, as `GET /snapshots?label=&heads=true` matches them. The snapshots
  those import aren't included. Both cover heads only, the snapshots that
  nothing replaces (see [Replacing](#replacing)): a skeleton drops out once
  its proof replaces it, and comes back if the proof is deleted. A head keeps
  every statement of what it replaces, so none is lost. Snapshot declarations
  are in the library `PisaSnap`, so adding `"libraries": ["PisaSnap"]`
  searches snapshots only.
- **Hits.** A hit's `module` is the snapshot's module, e.g. `PisaSnap.s1f…`,
  and `flags.imports` the header that loads it. `snapshot_ids` lists the
  scope's heads on that module, the snapshots to build on or replace, newest
  first, at most 10. `status` is the declaration's own, so a proved lemma
  next to a `sorry` is `proved`, and `verified` is the module's. Names can
  repeat across snapshots, and each is a hit of its own. `/resolve` covers the
  bundle only, and so does `/declarations/{name}` without `module`.
- **The leader.** One server at a time, holding a lease in the database
  (30 s, renewed every 10 s), indexes snapshots: it writes each module's
  declarations to Qdrant, and each snapshot to `pisa-<env_id>-entries`, which
  a label scope is resolved against. So a module can have any number of
  snapshots, but a label scope may cover at most 20,000 modules. When the
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
hits = client.search(description="doubling adds a number to itself", snapshots={"labels": {"run": "7"}})["hits"]
hits[0]["module"], hits[0]["snapshot_ids"], hits[0]["status"]  # "PisaSnap.s1f…", ["v4.35.0-1a2b3c4d~9c0e…"], "proved"
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
Mathlib, Physlib, QuantumInfo (density matrices, entropies, channels) and
PhyslibAlpha (Physlib's lightly reviewed, often AI-written contributions) import
together, so the index is exported from one environment. A QuantumInfo or
PhyslibAlpha hit carries the header that loads it, one import a line, in
`flags.imports` (`import Mathlib`, `import Physlib`, `import QuantumInfo`) and
in its `text`, and `/resolve` gives it for a name and each suggestion.
PhyslibAlpha is searched by default; `libraries` narrows a search to the
reviewed ones. An index exported before this, from three environments, has
`import Physlib; import QuantumInfo` there, until it is exported again.

## Verification policy

A check is `ok` only when all of these hold:

- The code elaborated without errors.
- Every new declaration depends only on `propext`, `Classical.choice` and
  `Quot.sound`. This rejects `sorry`, `admit`, `native_decide` and new axioms.
- The code contains no metaprogramming: `#eval`, `run_cmd`, `elab`,
  elaborator attributes, `unsafe`, `implemented_by`, simprocs, attributes
  that install code to run later (`@[init]`, `@[tactic]`, delaborators,
  `norm_num` and `positivity` extensions) and similar. Attributes are refused
  where they are applied, in `@[…]` and `attribute […]`, so `extern` or
  `init` can still name a variable, and a name in `«»` is never a command.
- Any `set_option` changes only an allowlisted option: resource limits
  (`maxHeartbeats`, `maxSynthPendingDepth` and similar), `diagnostics`,
  `quotPrecheck*`, `linter.*`, `pp.*`, `trace.*`, and `backward.*`, which
  restores older elaborator behaviour that Mathlib-style code often asks
  for. Options that skip the kernel, such as `debug.skipKernelTC`, are
  rejected.

Code the scan refuses is refused before it waits for a worker's Lean
process. The result's `reasons` say what (`forbidden: syntax`), and its
`messages` say where, each with what to do instead, such as that a snapshot
allows the macro family; a session's or a tactic's error says the same of
the first.

Lean's `autoImplicit` option is off by default, in checks, sessions and
snapshot builds alike, so a name that isn't bound in a statement is an error
rather than a new implicit argument. Code can turn it back on with
`set_option autoImplicit true`, alone or with `in`, because it is an allowlisted
option. Code written for a library that turns it on, as Physlib does, meets
"Unknown identifier `d`" here: when code that doesn't set the option has
such an error (or an unknown universe level), the result's `messages` add an
`info` hint at it, naming the option.

The axiom report is a `run_cmd` call into the `Pisa` library
(`docker/pisa-lean`, built into the worker image at `PISA_LEAN_LIB`), so
workers add `import Pisa` to every header they load, or `meta import Pisa` to
a module-system header. It describes each declaration: its axioms, and the
canonical text of its type and, for a definition, of its value, which the
worker hashes with SHA-256. The text is injective, so two declarations with
the same hashes have the same statement. The kernel re-check describes
declarations with the same library. On `/check` and sessions the report runs
even when the code has errors, so the declarations Lean kept still have a
status; one it can't describe, such as one left with a metavariable, is a
reason (`error: axiom report: describing x: a metavariable`), as is a report
that fails outright (`error: axiom report unavailable: …`, with what Lean
said). A snapshot's module and the kernel re-check describe every declaration
or nothing. The canonical form reads code as the
kernel does, so that what doesn't change a statement doesn't change its text:
- Binder names don't count, and binder kinds (`(n)`, `{n}`, `[inst]`) do.
  A declaration's universe parameters count by position, so renaming one,
  or the hygienic name Lean gives one it makes, doesn't change it.
- A module's own private names read without the module, which differs
  between a module and its replacement; another module's read whole, so one
  `import all`-ed can't stand in for the module's own.
- A `sorry` reads as one marker of its type, whatever Lean labels it with
  (the module and position it was written at).
- A proof reads as the proposition it proves, as the kernel takes any two
  proofs of a proposition to be equal: an application of a theorem, or of a
  constant whose type is a proposition (`rfl`, `Eq.refl`), and a `sorry` of a
  proposition. So a field filled with `sorry`, Lean's `_proof_1` or a lemma
  reads the same.
- Lean's auxiliaries (internal names such as `f._unary` or `f._mutual`,
  matchers such as `f.match_1`, `grind`'s) and the definitions it makes for
  notation and syntax (the parser of `notation "d" => 2`) aren't declarations
  of their own: what uses one reads its value in its place. So notation can
  be added, dropped or reworded, as statements are compared after it is
  expanded.
- Equation lemmas Lean makes on demand are `generated`, including those it
  names privately in module-system code (`_private.….f.eq_1`, for a `f` whose
  body isn't exposed).

What it doesn't see through is refused, as a changed statement would be:
- an instance found by another path after new instances or imports;
- a proof by a lemma stated in another but equal form, such as one of
  `b ≥ a` where `a ≤ b` is wanted, which reads as a proof of `b ≥ a`;
- a statement that uses an `import all`-ed private of a snapshot the code no
  longer builds on;
- a definition that uses `panic!`, whose message carries the module and the
  line it is written on.

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
that uses them, and `verify: false` opts out. It also runs for module-system
code, whatever `verify` says: the build's own report imports the module as
code on top of it does, seeing only what it exports. `/check` and
`/sessions`, which don't produce a module to re-check, keep the scan as-is.

## Limits

- The REPL never frees environments, so workers replace each process after
  `PISA_MAX_USES` jobs or at a memory high-water mark.
- REPL tactic mode can't write a finished tactic script back into the `sorry`.
  Clients submit the assembled proof to `/sessions/{id}/check`.
- Scoped extensions, such as scoped notation, don't survive pickling.
- A session's states start from its imports alone: the REPL makes each on
  the environment the code started from. So a tactic can't use what the
  session's own code declares before the `sorry`, and answers that it
  doesn't know the name; the session's `messages` warn at the first such
  declaration. Put those declarations in a [snapshot](#snapshots) and start
  the session on its header: what a snapshot declares is in the environment
  of the code that imports it.
- Not built yet: auth and per-tenant fairness, `/jobs` async batches, and
  human workspaces (stage 4).
