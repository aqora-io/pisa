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
   Workers claim jobs, preferring those whose document they hold open or
   whose header they have warm, and a job whose worker dies runs on another.
   Servers hold no state, so they run in replicas.
2. **Hardened workers.**
   - Each REPL runs in a bubblewrap sandbox and its own cgroup v2 limits.
   - Proofs are judged by the axioms they depend on, not by string matching.
   - Metaprogramming and unsafe options are rejected before the code runs.
   - Worker pools are labelled by version.
3. **Tactic sessions.**
   - A session is a document open on Lean Beam, and a state is a tactic run
     from a `sorry` or from another state, so tactics see what the session's
     own code declares. The states' records are kept in PostgreSQL, so any
     worker can continue a session by replaying them.
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
| `pisa-worker` | Slot pool cached by import header, process recycling, sandbox, cgroups, verification, documents and tactic sessions on Lean Beam, claiming jobs |
| `pisa-db` | The database (PostgreSQL, or SQLite for tests and single-process runs), its migrations, the job queue and the workers' registry, and the snapshot catalog, shared by the servers and the workers |
| `pisa-server` | The server: `pisa-router`'s API and `pisa-search`'s in one process, with a combined `/health` and graceful shutdown |
| `pisa-router` | The check, session and snapshot API: queueing jobs and waiting for their results, and answering catalog reads |
| `pisa-snapshots` | What reads the snapshot catalog, for the servers and the workers: snapshots, the comparison of a replacement with what it replaces, audits and their review lists, rebuild plans |
| `pisa-client` | Async Rust client for the HTTP API |
| `pisa-python` | Python bindings for `pisa-client` (PyO3 + maturin), imported as `pisa_client` |
| `pisa-tests` | `fake-repl`, `fake-pisa` and `fake-lean` (stand-ins for the REPL, the `pisa` tool and `lean`, without Lean) and in-process integration tests |
| `pisa-search` | Declaration search over Mathlib, Physlib and CSLib: corpus, descriptions, index, HTTP API, and the CLI that builds and loads bundles (see [Search](#search)) |

`web/` is a pnpm workspace with the TypeScript client, the React hooks and a
playground (see [Clients](#clients)).

## Running

```sh
docker compose up --build              # Lean v4.36.0 + Mathlib + Physlib + CSLib (first build compiles Physlib and CSLib)
PROJECT=lean-core docker compose up --build   # no Mathlib, for quick checks
python3 scripts/e2e.py [--mathlib] [--search] [--beam]   # end-to-end checks against the running stack
cargo test --workspace                 # fast tests with fake-repl, no Lean needed
(cd docker/pisa-lean && lake build) && cargo test -p pisa-worker --test real_lean -- --ignored   # the report and the verifier on real Lean
cargo test -p pisa-worker --test real_comparator -- --ignored   # acceptance on real Lean, which needs bwrap
```

CI (`.github/workflows/ci.yml`) runs these on every pull request and push to
main: the workspace's tests with PostgreSQL as well as SQLite, the real-Lean
tests, acceptance's with bwrap, the web and Python clients' tests, and `scripts/e2e.py` on a
`lean-core` stack, with `--beam` (and `PISA_BEAM_SLOTS=1`) but without
`--mathlib` or `--search`.

Compose is laid out like a deployment: Traefik on port 8000 (`PISA_PORT` to
change it) spreads requests over two servers, which it finds through Docker
and health-checks on `/health`, and two workers take jobs from PostgreSQL,
next to Garage for S3.
`--scale server=N` or `--scale worker=N` runs more of either. Traefik, Garage,
PostgreSQL, Qdrant, the servers and the workers restart with the Docker
daemon, and whenever they exit, even a server or worker that drained
cleanly, unless stopped; Garage
and PostgreSQL keep their data in the named volumes `garage-meta`,
`garage-data` and `postgres-data`, which `docker compose down -v` deletes.
A worker is given 630 s to drain on a stop. The servers and
the workers keep their data in two places, chosen by environment variables
that Compose and the chart set:
- **The job queue and the metadata**, the documents, the sessions and their
  states, and the snapshots and module records, are in a database, which
  every server and worker share. `PISA_DATABASE_URL` is a PostgreSQL URL
  (`postgres://user:password@host/db`) or the path of a SQLite file, which
  only one host can use. It is required.
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
  - A worker's claim on a job lasts `PISA_LEASE_SECS` (default 10; Compose
    and the Helm chart's `worker.leaseSecs` set 30) without a heartbeat,
    which renews it every third of that; a job whose worker died runs
    elsewhere once it lapses, up to `PISA_MAX_RETRIES` more times. That is
    the lease's trade-off: a dead worker's job waits up to the lease, 30 s
    with Compose's, for another worker, while a shorter lease lets a stall
    of a live worker, such as a host short of memory, hand its job to
    another worker as it still runs it. A lapsed claim stays its worker's
    until another attempt claims the job. A claim is its worker's name and
    the time it claimed, so a worker that claims its own job again makes a
    new attempt, and the old one can't answer. A run's acceptance stops as
    soon as it sees that its claim moved, within a third of the lease, and
    stores nothing; it stores its acceptance only with its answer, in one
    transaction. A request waiting on its answer gives
    another worker `PISA_MAX_WAIT_SECS` (default 60) after the lapse, then
    answers 503. An [async](#async-requests) job instead waits until a
    worker that can run it, of its version and with Beam slots for a
    document, has been live that long since the lapse, so it outlasts a
    restart of every such worker, or of the whole host; it is given up 24
    hours after it was queued at the latest. An async job no worker has
    claimed yet waits, up to those 24 hours, while a worker that can run it
    is on record, live, busy or restarting (a worker is from when it
    registers until an hour after it was last seen, even once it stopped
    cleanly, so a reboot doesn't clear it): its turn comes when a slot is
    free, however long the jobs ahead of it take. With none on record it
    gets a 429 after `PISA_MAX_WAIT_SECS`.
  - The tests use in-memory SQLite, and also the PostgreSQL that
    `PISA_TEST_DATABASE_URL` names when it is set. Each test has versions of
    its own there, so tests can share one database.
- **Blobs**, the snapshot modules, are in S3.
  `PISA_STORE` is `s3`, which reads the `AWS_*` variables, or `memory`.

To name the commit a stack runs, build it with `PISA_REVISION`, which
Compose passes to the images as the `REVISION` build arg (default `unknown`).
The binaries report it, in `/health`'s `revision` and `worker_revisions`, and
the images carry it as the label `org.opencontainers.image.revision`:

```sh
# jj: the working copy's commit if it has changes, else its parent
PISA_REVISION=$(jj log -r 'coalesce(@ ~ empty(), @-)' --no-graph -T 'commit_id') docker compose up --build
# git
PISA_REVISION=$(git rev-parse HEAD)$(git diff --quiet || echo -dirty) docker compose up --build
# a plain image build
docker build --build-arg REVISION=<commit> -f docker/Dockerfile --target server .
```

The pinned versions live in `docker/lean-project`: `lean-toolchain`,
`lakefile.toml` and `lake-manifest.json`. Keep `REPL_REV` in `docker/Dockerfile`
on the same Lean version. The image applies `docker/repl/infotree-at.patch` to
the REPL, which adds `infotreeAt` (see [goals](#positions)): it must apply to
the new `REPL_REV` too.

<a id="another-project"></a>
**A pool for another project.** A worker serves one Lake project, and its
version is that project's (`<toolchain>-<hash of its lakefile, manifest and
top-level files>`), so another toolchain, another library, or another
Mathlib, is another pool of workers on the same servers, database and store:
- **From a file.** `scripts/env.py build <file>` builds a pool's image from
  a TOML file that says what the environment is, e.g.
  `environments/lean-4-35.toml`:
  ```toml
  toolchain = "leanprover/lean4:v4.35.0"
  imports = ["Mathlib"]   # the modules its workers build, which requests may import

  [[require]]             # as in a lakefile.toml
  name = "mathlib"
  git = "https://github.com/leanprover-community/mathlib4.git"
  rev = "v4.35.0"
  ```
  It writes the project beside the file, in a directory of its name
  (`environments/lean-4-35/`), with the `lake-manifest.json` Lake resolves
  for it, in Docker; builds the worker image on it, tagged
  `pisa-worker:<version>` (`--repository` names it otherwise, and `--push`
  pushes it); and prints `version=` and `image=`. `--project-only` stops
  before the image. Commit the file and the directory: the manifest pins
  what the version hashes, and is kept until the file changes, or
  `--update` resolves it again. The toolchain is the one its requirements
  are on, or the script says which that is. `repl` names the tag of
  leanprover-community/repl to build, the toolchain's by default.
  `scripts/env.py label <dir>` prints the version of a project that has its
  manifest; one without, as `docker/lean-core`, gets a manifest in its
  image, and its version is what its workers then say. The rest of this
  list is what the script does, for doing it by hand.
- **The project.** A wrapper, like `docker/lean-project`: a `lakefile.toml`
  that requires the library at a commit, its `lake-manifest.json` and
  `lean-toolchain`, and a library file importing the modules agents will
  import, which the image builds. Not the library's own checkout: an
  [export](#comparator) requires the project's packages, and a checkout's own
  code isn't one. For a checkout, set `PISA_PROJECT_GIT` and
  `PISA_PROJECT_REV` on its workers to where its code comes from, or exports
  of runs that import it are refused.
- **The image.** `docker build -f docker/Dockerfile --target worker
  --build-context project=<dir> --build-arg LEAN_TOOLCHAIN=<its toolchain>
  --build-arg REPL_REV=<the same Lean> -t pisa-worker:<name> .` takes the
  project from `<dir>` in place of `docker/${PROJECT}`. Mathlib, if the
  manifest has it, comes from its cache; the rest compiles once. Workers
  replay the builds read-only, so build what agents import. Pisa's own Lean
  tool (`docker/pisa-lean`) is built on the project's toolchain, which has
  to be Lean 4.34.1 or later: the kernel re-check uses
  `Lean.Kernel.Environment.replay`. Lean Beam comes with the toolchain from
  Lean 4.36: a pool on one before serves checks and snapshots, and no
  documents or sessions, so give it no Beam slots (`PISA_BEAM_SLOTS=0`, the
  chart's `beamSlots: 0`).
- **Running it.** A Compose service for it extends `worker` with that image
  (Compose resolves `extends: file:` against the project directory), and on
  Kubernetes it is one of the chart's `pools` (see [Kubernetes](#kubernetes)).
  Its workers register their version: `/health` lists each pool's in `pools`,
  with the toolchain and the Lake packages its project pins, so an agent
  can pick the pool that has a library,
  and requests reach it by naming that `version`, or by
  [`PISA_ALIASES`](#versions) and `PISA_DEFAULT_VERSION`. Snapshot ids carry
  their version, so requests about a snapshot need none, and
  [`GET /environments`](#environments) keeps the project of every version
  served, live or not.
- **What it lacks.** Search indexes one version (see [Limits](#limits)), so
  agents on the new pool find names with `#check`, `#print` and `exact?`;
  a search that names another version answers `not_found`, saying which
  version search covers and that `/health` lists the pools.
  [Acceptance](#comparator) replays what each pair imports, so a large
  library costs minutes per pair.

## Kubernetes

`deploy/helm/pisa` is a Helm chart containing:
- the server Deployment (`server.replicas`, 2), with its Service and a
  PodDisruptionBudget;
- the worker Deployment;
- a KEDA `ScaledObject`.

**Cluster prerequisites.** Like most application charts, it uses the cluster's
own KEDA, which reads the job queue from the database (see Scaling below).

**Storage.** The job queue and the metadata (documents, sessions and their
states, snapshots and module records) live in PostgreSQL, and blobs (snapshot
modules) in an S3-compatible bucket.
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

**Pools.** `worker` is the release's pool, and each entry of `pools` another,
for [another environment](#another-project): a Deployment and a ScaledObject
of its own, which scales on the jobs of its version alone.
```yaml
worker:
  version: v4.36.0-54500a67   # required with pools: what its image serves
pools:
  - version: v4.35.0-1a2b3c4d # as scripts/env.py printed it
    minReplicas: 0
    maxReplicas: 2
```
- A pool has `worker`'s settings, and overrides those it names, `slots` or
  `prewarm` say. Its image is `worker.image.repository` tagged with its
  version, as the script tags it, unless `image` says otherwise, and its
  Deployment `<release>-worker-<name>`, `name` being its version with dashes
  for dots unless set.
- `minReplicas` and `maxReplicas` are `keda`'s unless set. With 0 a pool
  runs no pod while it has no job: a check or a snapshot request that names
  its version starts one, and waits for it, the image's pull included, so
  keep `router.maxWaitSecs` above that. A session or a document doesn't:
  while no worker of the version serves documents it is a 503
  `unavailable` at once, so send a check first, or keep a pool agents prove
  on at one replica. [`GET /environments`](#environments) lists the pool's
  version meanwhile, without a `pool`, once a worker of it has run.
- With `worker.version` set, `worker`'s pool counts its version's jobs and
  those of no version, and requests that name none go to it
  (`router.defaultVersion` is it by default). Without it, and without
  `pools`, it counts every job, as before.
- A worker is told the version its pool names (`PISA_EXPECT_VERSION`), and
  doesn't start if its image's project has another: a pool that scaled on
  one version's jobs and ran them on another would never drain them.

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
- Building a pool's image, which `scripts/env.py` does, and adding a pool without a `helm upgrade`.
- Auth on the API.
- A tighter security context: workers run privileged so bubblewrap and cgroup limits work.

## Upgrading

To the release that records environments:
- The chart has `pools`, a worker pool per environment, and
  `worker.version` (see [Kubernetes](#kubernetes)). A release that sets
  neither renders as before. `docker/Dockerfile`'s stage `lean` now builds on
  a stage `elan`, and a target `manifest` resolves a project's dependencies
  for `scripts/env.py`.
- The first process of this release adds the `environments` table. A worker
  records its version's project there as it registers, so an environment is
  listed by `GET /environments` (see [Environments](#environments)) once a
  worker of this release has served it; one whose workers all stopped before
  isn't.
- A check's result adds `version`, the version of the worker that ran it.
  A worker of the release before doesn't say, so while both run it may be
  absent.

To the release that shows where a refused type differs:
- A build's export prints each type a third way, `type_explicit`, with every
  argument shown. A refusal of a type that prints alike shows both sides'
  (`was_explicit`, `now_explicit`, see [Replacing](#replacing)) only when
  both modules were built by this release: a spec created before has none,
  so create it again to get them.
- `POST /check` takes `against` (see [Checking against a spec](#check)). A
  worker of the release before ignores it, so while both run, a check
  against a spec may come back without the comparison.
- `/health` adds `pools`, the live workers per version.
- An export whose modules import a library the worker's project builds
  itself requires the project from `PISA_PROJECT_GIT` at `PISA_PROJECT_REV`,
  and without them is refused, where the release before left the library
  out. Workers of a wrapper project, as Compose builds them, are unaffected.
- `docker/Dockerfile` takes the project from a stage named `project`, which
  `--build-context project=<dir>` replaces (see
  [A pool for another project](#another-project)); `PROJECT` still picks one
  of `docker/`. Mathlib's cache is fetched whenever the manifest has
  Mathlib, also through another package.

To the release that shows a running acceptance's progress:
- The first process of this release adds `progress` to the `jobs` table.
- A running acceptance's job has `progress`, and each of its pairs its
  `phases` (see [Progress](#accept-progress)).
- Compose and the Helm chart set `PISA_LEASE_SECS` to 30, from the default
  10: a dead worker's job waits up to 30 s for another worker.
- A worker of this release stores an acceptance only with its job's
  answer, and stops a judgment whose claim moved to another attempt. So a
  lost accept stored nothing, and its `worker_lost` has `resend_safe`
  true; one that a worker of the release before ran may have stored its
  acceptance.

To the release that accepts runs on pisa's own builds:
- `POST /snapshots/accept` builds nothing: comparator judges exports of the
  modules pisa built when the snapshots were created (see
  [Accepting a run](#comparator)). `build_timeout` is gone from the request
  and from the clients' accept options, and `timeout` is a pair's exports
  and comparator together.
- An acceptance's pairs name the modules comparator judged by their hash:
  `challenge_module` and `solution_module` are `spec_module` and
  `head_module`, a named run's too. A pair whose module couldn't be exported
  is `export_failed`; `build_failed` is only in acceptances stored before.
- Workers run the toolchain's `leanexport` (`PISA_LEAN_EXPORT` names
  another) and keep exports in their work directory's `exports/`. They
  remove `runs/`, where comparator built runs, as they start.
- A server of this release still sends the 1800 s that a worker of the
  release before builds a run in, which a worker of this release ignores:
  an accept queued while both releases run is judged by either.

To the release that restarts after a reboot and names its commit:
- Compose keeps Garage's and PostgreSQL's data in the named volumes
  `garage-meta`, `garage-data` and `postgres-data`. They start empty: a stack
  that kept its data in anonymous volumes starts from empty stores, so
  rebuild the catalog (create the snapshots again) or restore a backup into
  the new volumes.
- `/health` adds `revision`, the server's commit, and `worker_revisions`, the
  distinct commits of the live workers, `null` if the queue can't be read.
  A worker registers its commit, and `unknown` is any that doesn't say.
- `/health` adds `dependencies`, `{database, store}`, each `ok`, `unreachable`
  or `unknown`. With the database stalled it answers within about 2 seconds,
  with `database: unreachable` and `null` for what it couldn't read.
- `/health` adds `hosts`, the live workers' memory limits summed per host: a
  worker registers `PISA_SLOTS` x `PISA_MEMORY_MAX_MB` plus `PISA_BEAM_SLOTS`
  x `PISA_BEAM_MEMORY_MAX_MB`, the memory of its machine (or the limit of its
  container, if less) and the machine's boot id, and workers with one boot id
  are one host. A host whose limits add up to more than it has is
  `overcommitted`: Lean processes may be killed for memory their limits
  promised. The server and the worker each log a warning. `uncapped_workers`
  of a host aren't in the sum. A worker that can't read its boot id is in no
  host, and one that can't read its memory has `memory_mb` 0 and isn't judged.
  Reading the container's limit looks at `/sys/fs/cgroup/memory.max`.
- An `out_of_memory` error has `limit`, `{option, value}`: the setting
  (`PISA_MEMORY_MAX_MB`, or `PISA_BEAM_MEMORY_MAX_MB` for documents and
  tactic sessions) and its value in MiB. An accept's `out_of_memory` pair
  error names them in its message instead.

To the release that answers jobs in an envelope and retries in the clients:
- `GET /jobs/{id}` of a done job answers 200 with `{id, kind, status:
  "done", http_status, body}`: the status its request would have had, and
  its answer, errors included. Read those two fields, not the response's
  own status. Earlier clients misread it, and the earlier TypeScript client
  returns the envelope as the answer, so upgrade the clients with the
  servers.
- 429 and 503 answers carry `Retry-After: 2`, which browsers may read.
  `/health` adds `queued`.
- A create that keeps nothing says what to do next in `refused`. A create
  whose worker failed answers 200, with a `check.error` of kind `internal`,
  as `/check` does, rather than a 500.
- The clients send requests again by themselves: `with_retry(RetryPolicy::none())`,
  `retry=False` or `retry: false` turns it off. A create without a `key` is
  sent with one of the client's, `client:` and 32 hex digits. React's
  queries no longer run again on top of a retrying client.
- The clients read a gateway's 502, 503 or 504 as `unavailable`, which
  Python raises as `CapacityError`, rather than `internal` or
  `bad_response`.
- `wait_job` and `waitJob` look every 25 s, and with retries off they no
  longer try a dropped connection again.

To the release that checks old modules for clashing imports:
- A worker of this release clears `verified`, once per version, on each
  module verified before creates refused a header whose imports declare one
  name twice, and on everything built on it, with the clash in its
  `check.reasons`. Verifying one again fails with the clash. `/check`,
  documents and sessions refuse such a header too.
- A re-check with `again` that the kernel rejects clears `verified` on the
  module and on what builds on it.
- A spec is amended once: a second amendment of a spec is a `conflict`.
  Specs amended more than once before keep their amendments; the newest is
  current.
- A rebuild leaves the heads of a run whose spec was amended out, in
  `superseded`, rather than failing on them.
- A module-system snapshot whose export can't be made again in today's
  format (the export fails, runs out of time or doesn't fit) is recorded so,
  and no worker tries it again until a newer format; the review list says
  why a value behind it is missing. One the worker couldn't start, as with a
  broken sandbox, isn't recorded, and is tried again by the next pass.

To the release that keeps requests to their scope and runs to their names:
- The first process of this release migrates the catalog: `labels` on
  documents, sessions, jobs, acceptances and refutations, `runs` on
  snapshots and documents, and `run_names` with a `scope` column (SQLite
  builds the table again). Earlier releases can't read the migrated
  database.
- What was made before has no scope's labels but those its labels have: a
  snapshot labelled `user=42` is in the scope `user=42`, while earlier
  documents, sessions, jobs and acceptances, and run names, are reached only
  by requests without a scope (see [Scopes](#scopes)). Idempotency keys are
  per scope too, so the retry of an earlier create sends no scope.
- Upgrade the servers and the workers together: a job carries its scope,
  which an earlier worker would drop, running it unscoped.
- Browsers calling from another origin may now send `Pisa-Scope` and `PUT`.
- A run's name now follows every amendment of its spec, without `name`
  (see [Naming a run](#run-names)), and copies made by a rebuild or a
  settle keep the code as sent. Snapshots copied before have no `source`.
- Exports name only the audited pairs by their run, so the files of a
  lemma run the pairs import are `PisaSnap/s….lean` again. Acceptances
  stored before have no `challenge_module` and `solution_module`.

To the release that supersedes amended specs and refuses clashing imports:
- A module built earlier whose header imports two snapshots that declare
  the same name no longer verifies: its re-check fails with the clash, and
  it stays as it was.
- An amendment now supersedes the spec it amends, those made before
  included: an audit against an amended spec isn't `clean`, and its run
  takes no more submissions. Carry what should stay over with
  `POST /snapshots/resubmit`.
- A spec created with `amends` and no `holes` keeps the amended spec's.
  Specs amended earlier keep the holes they were created with, and their
  records show no change of holes.
- Workers export module-system snapshots built earlier again, as their
  builds left out the declarations that aren't public, where a hole's value
  can hide: one worker at a time, as it starts and every hour, one module at
  a time in a slot whose Lean process it stops first. Until then, the review
  list says such a value's module export doesn't have it. Search reindexes
  each module exported again.
- A refutation now outlives the snapshot that made it, as the record that
  its target is false: the migration drops its cascade (SQLite builds the
  table again). Those whose snapshots were deleted before are gone. Those
  made before have no statement hash, so a spec that amends their spec
  isn't shown them.

To the release that re-checks snapshots by default and answers catalog reads
on the servers:
- Back up the database first, then stop the servers and the workers, and
  start the new release: its first process migrates the catalog, links to a
  table of their own and module records to columns, in time that grows with
  the `modules` table. Earlier releases can't read the migrated database.
- Rebuild the images, and run the servers and the workers of this release
  together: the catalog's read jobs are gone, as the servers answer reads
  themselves.
- The worker image builds the `pisa` tool beside the `Pisa` library, and
  `PISA_TOOL` (`/opt/pisa/.lake/build/bin/pisa`) replaces `PISA_VERIFIER`.
- `POST /snapshots/comparator` is gone: `POST /snapshots/accept` audits the
  run and runs comparator on the same pairs, stores the result, and answers
  `accepted` only when the audit is `clean` too. Its pairs' `skipped` is now
  `left_out`, with a reason each, and a pair not accepted has an `error`
  with its kind. The async job kind `comparator` is `accept_run`.
- Accept classifies comparator's failures by the messages Lake prints, as of
  Lean 4.36.0-rc2. On a toolchain bump, check `classify` in
  `crates/pisa-worker/src/runs.rs` against Lake's `CLI/Check.lean`.
- Snapshots of plain code made before the upgrade keep the `verified` they
  had, which is false unless they were verified on request, and an audit
  against a spec isn't `clean` until its spec snapshots and heads are. The
  audit's `unverified` lists them: verify each with
  `POST /snapshots/{id}/verify` before relying on an audit. A create of their
  code re-checks them too.
- A worker with `PISA_CGROUP_ROOT` set (the chart's `worker.cgroups`) that
  can't set up its cgroups doesn't start, where it used to warn and run Lean
  with no memory or process limit. On a node without a delegated cgroup v2
  tree, set `worker.cgroups: false`, or leave the variable unset, to run
  without those limits on purpose. Such a worker logs a warning as it
  starts, and `/health`'s `unlimited_workers` names it.
- The store's files of a version are swept only once the store names its
  catalog, which the first create after the upgrade does. Run that first
  create against the database the store belongs to: a store marked by another
  database's worker is never swept by this one's. A worker doesn't mark a
  store that holds files more than an hour old of builds its database has no
  record of; `PISA_CLAIM_STORE=1` makes it (see [Retention](#retention)).
- The worker image's entrypoint is `tini -- pisa-worker`. A deployment that
  sets its own command keeps an init as PID 1: without one, the sandboxes
  the worker kills stay as zombies in their slots' cgroups until Lean can't
  start a thread there.
- Creates re-check their code through the kernel unless the request sends
  `verify: false`, so they take longer, about 2 s for code on `import
  Mathlib`, and refuse what the kernel rejects. A
  re-check that runs out of time keeps code outside the module system with no
  macros, unverified, and says so in `unverified`; other code is refused as a
  `timeout` or `out_of_memory`. `POST /snapshots/{id}/verify` takes
  `?timeout=`, per module.
- Set `PISA_ALLOWED_AXIOMS` on the servers wherever the workers set it
  (`allowedAxioms` in the chart sets both). A version that no worker serves
  now answers reads, a 404 or an empty page, where it answered 429.
- The `pisa` tool now says, of each declaration, which axioms are its own
  and which theorems of other snapshots it inherits others through. Records
  made before have neither, so the audit reads nothing through them (see
  [Through links](#through-links)): for a run in progress,
  `POST /snapshots/{id}/verify?again=true` on its heads describes them
  again. Heads that use no other snapshot's lemma need nothing.
- A create without a `key` gets one of its own, so keys that start with
  `job:` are refused (see *Retrying a create* in [Snapshots](#snapshots)).
- A build that a create's link refuses is kept, with no snapshot, for a day;
  the hourly sweep removes modules only once they are a day old, a module a
  stopped delete left included.
- The first process of this release adds the table `run_names` and the
  column `entries.source` to the catalog (see [Naming a run](#run-names)).
  Earlier snapshots have neither: name new runs as they start.

To the release that runs tactic sessions on Lean Beam:
- Earlier sessions and their states are gone, and their ids answer 404. The
  database's migration drops their records, and each worker deletes the
  pickled states under the object store's `states/<version>/` when it starts.
- Sessions need workers with a Beam slot. `PISA_BEAM_SLOTS` is 1 by default,
  in Compose and in the chart, whose worker pods request one more CPU and
  `beamMemoryMaxMb` more memory.

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
Every endpoint but `/health` and `/openapi.json` takes a `Pisa-Scope`
header, which keeps the request to one tenant's labels; see
[Scopes](#scopes).

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
| `POST /check` | `{code, version?, timeout?, detail?, at?, against?}` → check result |
| `POST /check/batch` | `{items: [{code}], version?, timeout?, detail?, at?}`, up to 128 items, 32 queued at a time → `{results: [...]}`, in order |
| `POST /sessions` | `{code, version?, timeout?}` with `sorry`s → `{id, states: [{id, goals, pos, end_pos}], messages, runs?, timing}`; see [Tactic sessions](#sessions) and [Naming a run](#run-names) |
| `POST /states/{id}/tactic` | `{tactic, timeout?}` → `{state: {id, goals}, complete, messages, timing}`; earlier states stay valid, and a failing tactic is a 422 |
| `POST /sessions/{id}/check` | `{code, timeout?, detail?}` → check result, also requiring the session's original statements and imports; a `lean` error if the axiom report can't describe a declaration of the session's code, whose statement it couldn't guard |
| `POST /snapshots` | `{code, version?, timeout?, verify?, labels?, key?, replaces?, fork?, holes?, amends?, refutes?, name?, detail?}` → `{snapshot?, refused?, check, cached, timing, replacement?, heads?, unverified?, amendment?, refutation?, refutations?, runs?}`; see [Snapshots](#snapshots), [Replacing](#replacing), [Verifying](#verifying), [Amending a spec](#amending), [Refuting a target](#refuting) and [Naming a run](#run-names) |
| `GET /snapshots` | `?label=k:v` (repeated), `limit?`, `after?`, `heads?`, `version?` → `{snapshots: [{id, version, module, header, imports, labels, replaces?, holes?, status, verified, created, run?}], next?}`, newest first; see [Labels](#labels) |
| `GET /snapshots/{id}` | `?detail=declarations` → the snapshot as in a listing, with `code`, `source?`, `check` and `exports?` |
| `GET /snapshots/{id}/dependents` | `?label=k:v` (repeated) → `{snapshots: [...]}` as in a listing, parents first; see [Rebuilding](#rebuilding) |
| `POST /snapshots/rebuild` | `{from, to, labels, dry_run?, allow_changes?, retire?, verify?, timeout?}` → `{ok, changes, changed_definitions?, plan: [{level, snapshot_id?, module, new_module, new_header, code, labels, key, replaces?, affected?}], replaced?, superseded?, created?, failed?, skipped?, not_deleted?, retired?}`; async with `Prefer: respond-async`; see [Rebuilding](#rebuilding) |
| `POST /snapshots/settle` | `{against, allow?, dry_run?, verify?, timeout?}` → as `POST /snapshots/rebuild`; async with `Prefer: respond-async`; see [Settling a run](#settling) |
| `POST /snapshots/resubmit` | `{from, to, dry_run?, labels?, verify?, timeout?}` → `{ok, items: [{snapshot_id, module, key, passes, reasons?, changes?, replacement?, new_snapshot_id?, check?}]}`; async with `Prefer: respond-async`; see [Amending a spec](#amending) |
| `GET /snapshots/audit` | `?label=k:v` (repeated), `version?` → `{summary: {proved, conditional, sorry, rejected, unverified_modules}, assumptions: [{name, signature?, snapshot_id?, verified, used_by}], not_proved: [{name, snapshot_id, status, axioms}]}`; or `?against=<id>` (repeated), `allow=<name>` (repeated), `review?`, `review_limit?` → the same, the assumptions with `in_spec`, and `{clean, heads?, forks?: [{snapshot_id, replaced_by}], targets?: [{name, spec, head, status, verified, accepted, refuted_by?, contradicted?}], links?: [{snapshot_id, replaces: {snapshot_id, allow}, replacement: {filled, changed_definitions?, affected?}}], comparisons?: [{spec, head, reasons?, changes?, replacement?}], review?: [{spec, head, declarations, library}], review_left_out?, unverified?, amendments?: [{snapshot_id, amends, reason, changes, added, holes?: {before, after}}], superseded?: [{spec, by}], refutations?: [{snapshot_id, spec, target, theorem, gone?}], unsettled?: [{head, swaps: [{snapshot_id, head}], blockers?}]}`, each target with `linked_status?` and `through?: [{name, snapshot_id, head}]`; see [Statuses](#statuses), [Auditing a run](#spec-audit) and [Through links](#through-links) |
| `PATCH /snapshots/{id}` | `{labels: {key: value or null}}` → the snapshot as in a listing; see [Labels](#labels) |
| `DELETE /snapshots/{id}` | `?with_replacers=true` (default `false`) → `{removed: [...], deleted: [...]}`, the modules that went and the snapshots deleted, the asked one first; see [Retention](#retention) |
| `POST /snapshots/{id}/verify` | `?timeout=` (seconds per module, default 60), `again?` → `{snapshot, checked, failure?}`; see [Verifying](#verifying) |
| `GET /snapshots/export` | `?against=<id>` (repeated), `allow=<name>` (repeated) → the run as a Lake project, a tar.gz; with `profile=palomar`, `pair=<n>`, `challenge_module`, `solution_module`, one pair as a Palomar package; see [Accepting a run](#comparator) |
| `POST /snapshots/accept` | `{against, allow?, paranoid?, timeout?}` → `{id, created, against, allow?, paranoid, accepted, clean, heads, audit, truncated?, pairs: [{spec, head, spec_module, head_module, challenge_module, solution_module, config, accepted, error?: {kind, message}, exit_code?, kernels?: [{name, verdict, reason?}], output, theorems, left_out?, phases?: [{name, seconds, peak_memory_mb?}]}]}`; async with `Prefer: respond-async`; see [Accepting a run](#comparator) |
| `GET /snapshots/accept/{id}` | → the stored result of `POST /snapshots/accept`, with `stale?` |
| `GET /snapshots/accept` | `?spec=<id>` → `{acceptances}`: the stored results that judged the spec snapshot, newest first, at most 100, each with `stale?` |
| `GET /runs/{name}` | `?version=` → `{name, root, heads, module?, snapshot_id?}`: the run, and what `import PisaRun.<name>` resolves to when its heads are on one module; see [Naming a run](#run-names) |
| `PUT /runs/{name}` | `{snapshot_id}` → as `GET /runs/{name}`: names the run the snapshot is in |
| `POST /documents` | `{code, version?, timeout?}` → `{id, revision, messages, sorries, runs?, timing}`, each run with `moved_to?`; see [Documents](#documents) |
| `PATCH /documents/{id}` | `{code, revision, timeout?}` → the document at its next revision |
| `POST /documents/{id}/goals` | `{at: [{line, column?}], revision?, mode?, timeout?}` → `{revision, at: [{pos, goals}], reopened, timing}` |
| `POST /documents/{id}/probes` | `{at: {sorry} or {line, column?}, candidates, revision?, timeout?, budget?}` → `{revision, pos, results: [{ok, closed, goals, messages, error?}], reopened, timing}` |
| `DELETE /documents/{id}` | → 204 |
| `GET /jobs/{id}` | `?wait=` (seconds, at most 60) → once its job is done, `{id, kind, status: "done", http_status, body}`, the request's status and answer, else 202 `{id, kind, status, progress?: {phase: {name, pair?, side?}, kernel?, seconds, peak_memory_mb?}}`; see [Async requests](#async-requests) |
| `DELETE /jobs/{id}` | → `{id, kind, status}`, `cancelled` if no worker had started it |
| `GET /environments` | → `{environments: [{version, toolchain, packages: [{name, url?, rev, input_rev?}], files, created, pool?}]}`: every version a worker has registered, with its project's files by name and, while it has live workers, its pool as `/health` lists it; see [Environments](#environments) |
| `GET /health` | Liveness: `{status, workers, queued, unlimited_workers, sweeps_refused, revision, worker_revisions, pools, dependencies, hosts, search}`, the live workers on the queue, the jobs waiting for one, those running Lean without memory or process limits by name, the versions whose sweeps of the store are refused (`[{version, reason, since}]`, see [Retention](#retention)), each `null` if it can't be read, the server's commit (`unknown` if its build didn't say), the distinct commits of the live workers (`null` if the queue can't be read), the live workers by the version they serve, a pool each (`[{version, workers, slots, beam_slots, draining, toolchain, packages: [{name, url?, rev, input_rev?}]}]`, with the toolchain and the Lake packages its project pins, empty for workers from before they said, `null` if the queue can't be read; see [A pool for another project](#another-project)), the memory limits of the live workers per host (`[{boot_id, workers, uncapped_workers, memory_caps_mb, memory_mb, overcommitted}]`, `null` if the queue can't be read), whether the `database` (answering within 2 seconds) and the module `store` (as the live workers last probed it, about every minute: `unreachable` if any can't reach it) are `ok`, `unreachable` or `unknown`, and search's state (`null` if the server doesn't search; its `describe_model` is the model that snapshot code is sent to, [`null` if none](#what-snapshot-search-sends)); 503 with `status: draining` once the server is stopping, and 200 otherwise, whatever a dependency's state |

Browsers may call the API from other origins only when the server allows
them: `PISA_CORS_ORIGINS` (`--cors-origin`, the chart's `server.corsOrigins`)
takes a comma-separated list of origins, or `*`. It is empty by default.

<a id="scopes"></a>
**Scopes.** A request may send `Pisa-Scope: user=42,org=7`: labels that keep
it to one tenant, so that tenants can share a server.
- **The header** is comma-separated `key=value` pairs, each key once, at most
  64, keys and values as for [labels](#labels) with no `,` in a value; a
  malformed one is a 400. No header, or an empty one, is no scope, which
  bounds nothing.
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
  [Snapshot search](#snapshot-search)), and `GET /declarations/{name}` with
  `module=` is `not_found` for a module no snapshot of the scope is on. The
  library's declarations are everyone's, and so are description flags: a flag
  made in a scope carries none, and counts in every scope's hits.
- **Not yet:** fairness between scopes, caps and metrics per scope, and
  listing or deleting a scope's documents, sessions and jobs.

<a id="async-requests"></a>
**Async requests.** A check, a session's start, tactic or check, a
snapshot's create or verify, a rebuild, a settle, a resubmit, and a run's acceptance can take minutes: a wait for a worker, loading the
imports, Lean's `timeout`, a kernel re-check. Behind a proxy that cuts
requests off at 60 s, or over a network that drops, send them with
`Prefer: respond-async`. The server then answers `202 {id, kind, status:
"queued"}` at once, with `Location: /jobs/{id}`, and runs the request as it
would have:
- `GET /jobs/{id}?wait=30` waits up to `wait` seconds (at most 60) and, once
  the job is done, answers 200 `{id, kind, status: "done", http_status,
  body}`: the status and the answer the request would have had, errors
  included, e.g. an `http_status` of 429 when no worker of its version is
  on record, or 503 `worker_lost` when the job was lost (see
  [When a worker is lost](#worker-lost)). While every worker that could
  run it is busy, or none is live, e.g. across a restart, the job waits for
  a slot, up to a day: a sync request on a busy pool gets a 429 after
  `PISA_MAX_WAIT_SECS`, so send long builds and acceptances async.
  Until then it answers 202 with `status` `queued` or `running`, and for a
  run's acceptance `progress`, what its worker last said it was doing (see
  [Progress](#accept-progress)). It answers
  within `wait` and five seconds even if the database does not: the last
  status it read, else a 503. A 404 or a 503 is the look's own, never the
  job's. Any server answers for any job.
  The clients look every 25 s, well short of a proxy's 60.
- An answer is kept for an hour after its job is done, a snapshot's create
  and a run's acceptance for a day; then the job is a 404. For 30 days
  after that, asked in the scope it was made in, the 404 says what the job
  was and how it ended (`expired: a create_snapshot that answered 200 at
  …`, `was given up at …, as no worker took it`, `was lost at …`, or `was
  cancelled at …`), never its answer; see [Recovering
  a lost job](#recovering-a-lost-job). In any other scope, and for an id
  that never was, it says `unknown job`.
- `DELETE /jobs/{id}` withdraws a job no worker has started (`cancelled`); a
  `running` or `done` one stays as it is.
- Anyone with a job's id can read its answer or cancel it, so keep it secret.
  Build its URL from `id` rather than `Location`, which is relative to the
  server's root.
- `/check/batch`, and the snapshot reads and edits, answer at once whatever
  `Prefer` says: send a batch's items to `/check` instead.

A check result is `{ok, time, timing, version?, error?, messages, sorries, reasons, changes?, declarations, runs?, warnings?}`:
- `ok` is the verdict.
- `version` is the version of the worker that ran it (see
  [Environments](#environments)), absent when none did.
- `timing` says where the time went, in seconds: `{queue, load, execution, tracing, verification, total}`.
  Every operation that runs Lean has one, in the same shape, and a phase that
  doesn't apply is 0. `queue` is the wait for a worker, from queueing the
  job to a worker claiming it, and `total` the worker's wall clock from
  claiming it to answering. A check's `load` is the header import, 0 when a
  process had it warm, `execution` its body (`time`) and `verification` the
  axiom report. A create's `execution` is the compile, imports included, and
  its `verification` the kernel re-check. A session's start reports its open
  as `execution`. A tactic's `load` is the reopen and the replay of earlier
  steps, and `execution` its step. A probe's `load` is the reopen and
  `execution` its candidates; goals' `load` is the reopen and `tracing` the
  asking. A check's `tracing` is the run that reads its `tactics` and `at`
  (see [goals](#positions)), 0 when it asks for neither.
- `messages` are Lean's, each `{severity, pos, end_pos?, text}`.
- `reasons` says why `ok` is false, e.g. `axiom: t uses sorryAx`.
- `changes` lists, for a [replacement](#replacing), a session's final
  check or a check `against` a snapshot, the declarations that don't keep the
  statements they must, each also one of `reasons`: `{name, module?, change,
  was?, now?, was_explicit?, now_explicit?, includes?}`, see *Refused* under
  [Replacing](#replacing).
- `declarations` lists each new declaration as `{name, kind, status,
  axioms}`, its axioms being those it uses besides the allowed ones (see
  [Statuses](#statuses)). With `detail: ["declarations"]` each has every
  field: all its `axioms`, the assumptions it uses (`assumes`), and the hashes
  and references the comparisons run on (`type_hash`, `value_hash?`,
  `type_refs`, `value_refs`, `generated?`, `parent?`). `/sessions/{id}/check`
  and `POST /snapshots` take the same `detail`, and `GET /snapshots/{id}`
  takes `?detail=declarations`.
- `runs` says what each import by run name resolved to (see
  [Naming a run](#run-names)).
- `warnings` says what went wrong with goal reporting (see
  [goals](#positions)): that it failed or ran past `timeout`, so the result
  is a plain check's, or that it took over three times as long as the check,
  and a second or more longer.

<a id="check"></a>
**Checking against a spec.** `against` names a snapshot, usually the spec
the code is meant to [replace](#replacing), and the check compares the
statements the code shares with it: each declaration both declare must be the
same kind with the same type. One that isn't is in `changes` and `reasons`,
with the snapshot's signature as `was`, and `ok` is false, so a file that a
replacement would refuse says so before any create. Declarations only one of
them has aren't compared, nor definitions' values, which a create judges with
the spec's holes and `allow`. The check runs on the snapshot's version unless
it names one, which must be that version; an unknown snapshot is a 404.

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

Goals and tactics come from a second run of the code, after the plain check
that gives `messages`, the verdict and `time`. It has a `timeout` of its own,
and its time is `timing.tracing`:
- Without `tactics`, Lean prints only what contains one of the positions,
  terms included. On a 542-line failing file, goals at one position took
  6.5 s, against 6 s for the check and 150 s for the whole tree. A position
  under `all_goals` gets the goals of each of its runs: on a 110-line file
  with 81 branches, 12 s against the check's 6 s.
- `tactics` need the whole tree, and with `terms` every term of the code too:
  several times the check, and minutes for a 1,000-line chunk (see
  `bench/infotree/2026-11-06/report.md`).
- When the second run fails or runs past `timeout`, the result is the plain
  check's, without `tactics` or `at`. `warnings` says so, e.g. `goal
  reporting ran past 90s; diagnostics are from a plain check`. It also says
  when goal reporting took over three times as long as the check, and a
  second or more longer.

Nothing is kept between checks, so ask about every position in one: each
check elaborates the code again.

<a id="documents"></a>
**Documents.** A check elaborates its code again each time. To ask about one
proof many times, open it as a document on Lean Beam, which keeps the
elaborated file. Then ask for the goals at positions, and try candidate
tactics at a `sorry`:
- `POST /documents` opens the code, and answers Lean's errors and warnings
  (`messages`) and the `sorries`, each with its goal. The `id` carries the
  version, as a snapshot's does. Its `timing` says where the open's time
  went: `load` is the startup (waiting for the document's lock, a Lean Beam
  process starting, the file), `execution` Lean elaborating the code and its
  header import, and `tracing` the diagnostics and the goals of its `sorry`s.
  An open that fails, a 422 `timeout` when it runs out of its `timeout`
  included, has the phases up to the failure in the error's `timing`.
- `/documents/{id}/goals` takes 1 to 64 positions, as checks do. A position
  with only a `line` is that line's first tactic, after any `·` or `.`
  bullet, and the answer gives the column it used. There are no goals in the
  imports or outside a proof. `mode: "after"` gives the goals after the
  tactic at the position.
  - Each answer gives the text of the `line` it was asked at, and
    `in_proof`: no goals with `in_proof: true` is a finished proof, and with
    `false` a position outside any proof, as a line off by one can be. Lean
    counts a blank or comment line right after a proof as in it.
- `/documents/{id}/probes` runs 1 to 64 candidates at one place: a `sorry`
  (`{sorry: n}`, an index into `sorries`), or a position in the body.
  - Each runs on the document as it is, in turn, with `timeout` seconds of
    its own, within what is left of the request's `budget` (300 s unless
    given, at most 3,600). Those it doesn't reach are a `timeout` that says
    the budget ran out.
  - When Lean Beam restarts during the request, after three timeouts in a
    row or a failure, the document reopens in the new process and the rest
    run there, with `reopened: true`. The candidate Beam failed on has its
    error. If the document can't reopen, the rest have that error.
  - The policy refuses a candidate as it refuses code, in its `error`, and
    the others still run. A candidate that reaches Lean's recursion or
    heartbeat limit has a `recursion_limit` or `heartbeat_limit` error, with
    `limit` naming the option (`maxRecDepth` or `maxHeartbeats`), its
    `value` and the `phase` Lean was in: it is the candidate's own, as it
    comes again on a retry. A candidate Lean Beam fails to run in any other
    way has an `unavailable` error, which isn't Lean's answer.
  - `closed` means the candidate ran without errors, left no goals, and
    doesn't say `sorry` or `admit`. It is no verdict: Lean Beam checks
    neither termination nor what a tactic elaborated to. Check the finished
    proof with `/check`.
- `PATCH` replaces the code. It names the `revision` it replaces, and is a
  409 `conflict` if the document is at another. Goals and probes may name a
  `revision` too.
- `DELETE` closes the document. An unused one expires after
  `PISA_STATE_TTL_SECS` (a day in Compose and the chart).
- A document's jobs run one at a time, on the worker that holds it open,
  while that worker is live. If it goes, another reopens the stored code,
  which takes as long as elaborating it. A reopen, there or after the
  document was closed to make room or its Lean Beam restarted, has the
  `timeout` of the open or last edit, not the request's, and the request's
  own `timeout` starts after it. Goals and probes then answer
  `reopened: true`, and an error after it says the document was opened
  again first. A reopen that fails says it was opening the document again.
- A document no live worker holds, as a new one, opens on the Beam worker
  with the fewest documents open per Beam slot: the others leave it to that
  worker for up to two seconds. A Lean Beam process that times out three
  times in a row is restarted, as what timed out may still run in it, and
  its documents reopen. A request in flight on another of its documents
  answers `unavailable`, naming the restart, rather than `crash`.
- Only workers with Beam slots serve documents. Without one, the endpoints
  answer 503 `unavailable`; a 429 means no Beam worker took the job in time.
  The worker settings:
  - `PISA_BEAM_SLOTS` (default 1): `lean-beam-mcp` processes beside the REPL
    slots, each in a sandbox and cgroup of its own. They serve documents and
    tactic sessions.
  - `PISA_BEAM_DOCUMENTS` (default 4): the documents each keeps open. To
    open another, it closes the least recently used one that no job holds,
    and logs it at `info`. The worker then no longer holds that one, whose
    next job opens it on the Beam worker with room. Opens at once count each other's place.
  - `PISA_BEAM_MEMORY_MAX_MB` (default 0, none): each one's memory limit.
    Out of memory, the kernel stops the whole process (`memory.oom.group`),
    and the request answers `out_of_memory`.
  - `PISA_BEAM_BIN`: `lean-beam-mcp`, by default the one in Lean's
    toolchain.

<a id="sessions"></a>
**Tactic sessions.** A session is a [document](#documents) for building
proofs one tactic at a time, as a tree of states:
- `POST /sessions` opens the code, and answers a state for each `sorry`,
  with its goals, and Lean's errors and warnings (`messages`). The session's
  `id` is its document's, so `/documents/{id}/goals` and `/probes` work on
  it, but its code stays: a `PATCH` is a 400 `bad_request`.
- `POST /states/{id}/tactic` runs a tactic on a state, in the context of the
  session's code, so it sees what the code declares before the `sorry`.
  - It answers the new state, which can be continued; a tactic that fails is
    a 422 `lean`, with Lean's errors in its message. A tactic that reaches
    Lean's recursion or heartbeat limit is a 422 `recursion_limit` or
    `heartbeat_limit`, with `limit` as for probes. Lean Beam failing on
    its own is not Lean's answer: the state is replayed and the tactic run
    once more, and a second failure is a 503 `unavailable`.
  - A state stays valid after others are made from it, so a client can try
    alternatives from one state.
  - `complete` means no goals are left and no tactic on the way from the
    `sorry` says `sorry` or `admit`. As with probes, it is no verdict.
- A state's id is `<version>~<session>~<state>`, where `<state>` names what
  the state is: the same tactic on the same state gives the same id.
- `POST /sessions/{id}/check` checks the assembled proof from scratch, as
  `/check` does, and also requires the session's original statements and
  imports.
- The worker that holds the session's document keeps the last 256 states it
  made. Another worker, or this one for an older state, replays the state's
  tactics from the nearest state it keeps, or from the `sorry`, reopening
  the document first if it isn't open there. Each replayed tactic has the
  `timeout` the session's start had. The answer says how many tactics it
  `replayed`, and whether it `reopened` the document; an error says so too.
  A replay that fails keeps none of the states it made on the way, so the
  next try replays from where this one started.
- `DELETE /documents/{id}` closes a session, and its states then answer 404.
  A session expires `PISA_STATE_TTL_SECS` after it starts.
- Only workers with Beam slots serve sessions. Without one, `/sessions` and
  `/states/{id}/tactic` answer 503 `unavailable`.

<a id="versions"></a>
`version` selects a worker pool, by label (such as `v4.35.0-1a2b3c4d`) or by
an alias set with `PISA_ALIASES`. Session, state, snapshot and document ids
carry their version, so the requests that take one take no `version`. `timeout` is Lean's
limit in seconds (default 60).

<a id="environments"></a>
**Environments.** A version names an environment: a Lean toolchain and the
Lake project its workers build, whose label is `<toolchain>-<hash of the
project's files>`. A check's result says which ran it in `version`, the
one the request named, the server's default, or, with neither, whichever
pool took it; it is absent only when no worker ran the check.
`GET /environments` lists every version a worker has registered, by
version:
- `toolchain` and `packages` are what its project pins, as `/health`'s
  pools have them.
- `files` has the project's files by name: `lean-toolchain`, the lakefile,
  the manifest and the top-level `.lean` files. Written to a directory they
  are the project again, which builds a worker image of the same version
  (see [A pool for another project](#another-project)).
- `created` is when a worker first registered it.
- `pool` has its live workers, and is absent while it has none.

An environment stays listed after its workers are gone, as its snapshots
stay, so what a snapshot's version was can be read after its pool is scaled
to zero or removed. Workers started with a version of their own choosing
(`PISA_VERSION`) list what the latest of them serves.

**Errors** always look like `{"error": {"kind", "message"}}`.
- **Checks report failures in-band.** A check that times out, crashes or is
  rejected by the policy still returns 200, with `ok: false` and the `error`.
  Batch items work the same way, so one failed item never fails a batch.
  So does a create: one that keeps nothing says in `refused` what to do
  next (see [Snapshots](#snapshots)), a worker that failed it included.
- **Other requests use HTTP statuses:**

| `kind` | Status | Meaning |
| --- | --- | --- |
| `lean`, `policy` | 422 | Lean or the policy refused the input, e.g. an unknown tactic |
| `timeout`, `out_of_memory`, `crash` | 422 | The input hit a resource limit; `out_of_memory`'s `limit` has the memory setting and its value in MiB |
| `recursion_limit`, `heartbeat_limit` | 422 | Lean reached `maxRecDepth` or `maxHeartbeats`; `limit` has the option, its value and the phase. The input fails as it is, so don't retry it |
| `capacity`, `unavailable` | 429, 503 | No worker could take the request, or the metadata store failed; retry later |
| `worker_lost` | 503 | The job was lost; `resend_safe` says whether to send it again, see [When a worker is lost](#worker-lost) |
| `bad_request`, `not_found` | 400, 404 | Malformed request; unknown or expired job, state or session; unknown snapshot or declaration |
| `conflict` | 409 | A create replaces a snapshot that another already replaces, in-band in `check.error` (see [Replacing](#replacing)); or a document is at another revision than the request names |
| `internal` | 500 | A bug |

A 429 or a 503 carries `Retry-After`, in seconds. The clients send a request
again by themselves (see [Clients](#clients)), and read a gateway's 502, 503
or 504, whose body isn't pisa's, as `unavailable`.

<a id="recovering-a-lost-job"></a>
### Recovering a lost job

An [async](#async-requests) job's answer is kept for an hour after it is
done (a day for a snapshot's create and a run's acceptance), a lost job's
too: until then `GET /jobs/{id}` answers 200 with an `http_status` of 503
`worker_lost` (see [When a worker is lost](#worker-lost)). If its client was
away longer, `GET /jobs/{id}` is a 404 that says what the job was and how it
ended, for 30 days, in the scope it was made in only (the `Pisa-Scope` it was
sent with). The answer itself is never kept. What the job made is still
there:
- A create sent again with the same `key` replays: it returns the snapshot
  the first one made (see the table in [When a worker is lost](#worker-lost)). The
  clients always send a key; with `Prefer: respond-async` a server
  gives a create without one its own.
- A create that named a run: `GET /runs/{name}` returns the run and what
  `import PisaRun.<name>` resolves to, with the same `Pisa-Scope` the
  create was sent with.
- An accept is found with `GET /snapshots/accept?spec=<id>`, which lists the
  spec's stored acceptances, newest first. Match yours on `created`,
  `against`, `allow` and `paranoid`. Do not send an accept again unless
  none matches: a finished first run may be stored already.
- Anything else: send it again, which is safe for every other async
  request (see the table in [When a worker is lost](#worker-lost)). The 404
  doesn't carry `resend_safe`; only the lost job's own answer did.

<a id="worker-lost"></a>
**When a worker is lost.** A job whose workers kept disconnecting, that no
worker able to run it came back for in time, or that a server ran itself and
lost as it stopped, answers 503 `worker_lost` with `Retry-After`, and
`resend_safe`. Its message says how many times it was claimed, and whether its
attempts ran out or no worker came back. An async job answers it as its
`http_status` and `body`, as it would any error. The clients' errors carry
it: `Error::Server`'s `resend_safe` in Rust, `PisaError.resend_safe` in
Python and `PisaError.resendSafe` in TypeScript.

| Request | `resend_safe` |
| --- | --- |
| Checks, sessions and their tactics and checks, verify, export, document open, goals, probes | `true` |
| Creates (replace, amend, refute) | `true`: a resend with the same `key` replays; the clients always send one |
| Rebuild, settle, resubmit | `true`: their creates carry the plan's keys |
| Delete, document close | `true`: a second run answers `not_found`, which means done |
| Accept | `true`: an acceptance is stored only with its job's answer, so a lost one stored nothing; a resend judges the run again, from the start |
| Document edit | `false`: check the document's revision first |

The clients send a lost request again by themselves when it is safe to send
twice, as they do after any 503 (see [Retries](#clients)): checks, tactics,
goals, probes, verifies and creates, and a batch's lost items. An async job's
lost answer is returned, not sent again, and so is any other lost request:
those are yours to resend, by this table.

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
  again. Such a create answers `cached: true`: its `check` is the stored
  build's, with the build's `time` and `timing`, and the response's own
  `timing` is what this request cost. `GET`, `PATCH`, `DELETE` and `verify` take the id, which carries the
  version, so they take no `version`; a module name there is a `bad_request`.

A create that keeps nothing has no `snapshot`; `refused` says what to do
next, and its `check` why:
- `proof`: change the code. Lean or the kernel rejected it, the policy
  refused it, its header failed, it changed a statement of what it
  replaces (`check.changes`), or it doesn't refute its target. Code with
  `sorry` is kept: a refusal is never about what isn't proved yet.
- `limit`: it ran out of time or memory, or crashed Lean. It may be right:
  give it more `timeout`, or make it cheaper.
- `request`: change the request. It breaks a rule (`bad_request`), names
  what doesn't exist in its scope (`not_found`), or conflicts in a way no
  resend fixes, as a run name another run has.
- `conflict`: what it replaces is replaced already. Send the same code again
  replacing one of `heads`.
- `retry`: nothing is known to be wrong. No worker took it, or one failed;
  send it again later, with the same `key`.

Code that is already kept but fails the re-check `verify: true` asks for is
refused too: `check.error` has the reason, as new code that fails the
re-check isn't kept. So is a header whose imports Lean reads differently from
its lines, such as an import after a block comment on the same line: the
record would miss what the module builds on, so it is a `bad_request` that
asks for each import on its own line. And so is a header that imports two snapshots
declaring the same name, a `bad_request` that names a snapshot of each. Lean
loads both when the name is a theorem of one statement in each, as a
skeleton and its fill are, and the code then uses whichever comes first in
its imports, so what it proves would hang on their order. When the two are
versions of one run, the error names the run's head to import instead.
`/check` answers such a header with the same `bad_request` in `error`, and
opening a document or starting a session on it is a 400.

**Who answers.** The servers read the catalog themselves: `GET` of a
snapshot, listings, dependents, label edits, audits and their review lists,
and rebuild plans need no worker, so they answer at once even while builds
keep every Lean slot busy, and even when no worker of the version is live.
Creates, deletes, verifies, exports and comparator runs go to a worker. A
listing or a label audit that names no `version` reads the server's default
(`PISA_DEFAULT_VERSION`), or else the one version the live workers serve, and
asks for a `version` when there are several. The servers take
`PISA_ALLOWED_AXIOMS` as the workers do, for what an audit lists of a
declaration that isn't proved; set it on both, or on neither.

`GET /snapshots/{id}` adds the module's `code`, its `check`, and its
`exports`: the declarations that code importing it sees, each `{name, kind,
signature?, error?, docstring?, description?}`, with `error` in place of
`signature` for one that didn't print. `description` is `{title, text,
latex?, aliases, source}`, which search writes when it has a describe model
(`PISA_SEARCH_DESCRIBE_MODEL`; see [what it sends](#what-snapshot-search-sends)), a
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
- A create without a key gets one of its own, `job:` and 32 hex digits, so
  that when its worker is lost after keeping the snapshot, the retry of its
  job returns that snapshot instead of adding another. Keys that start with
  `job:` are the server's: a create that sends one is a 400.
- The clients send a create that has no key with one of theirs, `client:`
  and 32 hex digits, so that sending it again after a lost answer returns
  the snapshot it kept (see [Clients](#clients)).

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
  - `imported_from` is the snapshot module the code builds on that declares an
    identical copy, for a `missing` declaration (same name, kind, type and
    value; the first, when several do). An import doesn't stand in for the
    spec's own declarations: see [Reusing a proof](#reusing-a-proof);
  - `was` and `now` are its signature before and after, for a changed type or
    assumption, from the modules' search exports. Those are what the builds
    printed; the comparison reads the declarations' hashes. Where the two
    print alike, as when an instance is found by another path (see
    [Canonical form](#canonical-form)), `was_explicit` and `now_explicit`
    are the types printed with every argument shown, instances too, cut
    around where they first differ, and the reason says so. Exports have that
    text from this release on; a module built before has none;
  - what Lean makes for an inductive type (its constructors, recursor,
    `casesOn`, `injEq`, projections and the like) is one change, to that
    type, whose `includes` lists them: `derived` unless the type itself
    changed.

  To change a statement on purpose, create the code without `replaces`.
  The build is kept all the same, with no snapshot: nothing lists or
  searches it, and creating the same code again, without `replaces` or
  replacing another snapshot, builds nothing. It goes once it is a day old
  with no snapshot on it (see [Retention](#retention)). So does a build
  refused for what it [refutes](#refuting).
- **Only a head.** A create may replace only a head, a snapshot that nothing
  replaces. Otherwise nothing is built or kept: `check.error` is a `conflict`
  that names what replaces it, and `heads` lists the snapshots to replace
  instead, those nothing replaces that are reached from the replaced one
  through what replaces it, oldest first. A create that loses a race to
  another during its build is refused the same way, and its build is kept,
  as for a refused comparison, which the message ends by saying. So is the
  build of a create whose replaced snapshot was deleted meanwhile. `"fork": true` skips the
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

<a id="reusing-a-proof"></a>
**Reusing a proof.** The unit of reuse is a helper-only snapshot: a snapshot
whose code declares lemmas and definitions that other code may import, and
none of a spec's targets. A replacement must declare the replaced module's
own declarations itself, so code that imports a snapshot declaring the spec's
`t` is refused with `change: missing`, even if that `t` is identical: the
refusal's `imported_from` names the module. An import can't stand in for them
because the comparison reads the replaced module's declarations in the new
module, and the spec is what that module states, not what it builds on.
To reuse a retrieved complete proof:

1. Take the proof's helpers (everything but the spec's targets) into a
   snapshot of their own, and create it.
2. Import that snapshot in the code that replaces the spec's module.
3. State the targets again in that code, locally, and prove them with the
   helpers.

<a id="run-names"></a>
**Naming a run.** A module's name is a hash of its code, so a proof that
replaces a lemma is a new module, and code that imports the lemma by hash
would have to move. A create that starts a run, one without `replaces`, may
name it instead: `"name": "projA.lemma1"`. Code may then `import
PisaRun.projA.lemma1`, with any of `public`, `meta` and `all`, and the server
resolves it to the run's head before anything runs:

```text
POST /snapshots {"code": "import Foo\n\ntheorem lemma1 : 1 = 1 := sorry", "name": "projA.lemma1"}
POST /check {"code": "import PisaRun.projA.lemma1\n\ntheorem t : 1 = 1 := lemma1"}
  → {"ok": true, …, "runs": [{"name": "projA.lemma1", "module": "PisaSnap.s3c…", "snapshot_id": "…~9c0e…"}]}
```

- **Names.** Dotted parts, each a letter or `_` then letters, digits and
  `_`, none a Lean keyword, up to 128 bytes; one name per run, unique in its
  version and [scope](#scopes), so dots give projects namespaces. A name
  another run has, while that run has snapshots, keeps nothing
  (`check.error` is a `conflict` that names that run's first snapshot).
  `name` with `replaces`, or a malformed name, is a 400. Snapshots show the
  name of their run as `run`.
- **Amending.** A create that [amends](#amending) a named run's spec takes
  the run's name for its own run, without sending `name`, and a resubmit
  carries the run there; another `name` alongside is a `bad_request`, as a
  run has one name.
- **Later.** `PUT /runs/{name} {"snapshot_id": …}` names the run that
  snapshot is in, from any of its snapshots: a name it had goes, and one
  another run with snapshots has is a `conflict`. `GET /runs/{name}` reads
  a run by its name: its `root`, its `heads`, and, when they are on one
  module, the `module` and `snapshot_id` the name resolves to now.
- **Resolving.** Each `PisaRun.<name>` import becomes the module of the run's
  head, on the same line, so positions don't move; a message on that import
  line is placed on the line as sent. Answers list what each name resolved
  to in `runs`. A run whose heads are on several modules, a fork, is a
  `conflict` that names them; a name no run has is a `not_found`. The
  resolved imports are checked as hash imports are, so a name and another
  version of its run by hash are a `bad_request` that says which module the
  name is. A create into a run (`replaces`, forks too) that imports that
  run by name is a `bad_request`: it would import the head it replaces. For
  `/check`, a batch and a create these are in `check.error` (a batch
  resolves each header once); for sessions and documents they are HTTP
  errors.
- **When.** `/check` and creates resolve at the request. A session resolves
  when it starts and a document when it opens: the session's final check,
  and each revision of the document, import the same modules, and a name
  the code adds later resolves then. Their answers set a run's `moved_to`
  to its head when the run has moved to another module since; once no
  snapshot is on the module a name was pinned to, they are a `not_found`
  that names the import.
- **What is kept.** A create's module is the code as resolved, so its hash,
  and all of pisa's records, name modules as before. The snapshot keeps the
  code as sent as `source` (`GET /snapshots/{id}`), and what its names
  resolved to: a create with a `key` sent again as the same text resolves
  as the first did, and returns the first snapshot with its `runs`, whatever
  the run did since. Copies made by a rebuild or a settle keep `source`,
  its hash imports moved as the copy's are; a resubmit's keep the head's.
- **Retention.** A run keeps its name while it has snapshots, its first one
  deleted or not; the name goes with the run's last snapshot.

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
proved: the audit shows it unproved, and the run isn't `clean`. A witness is
an ordinary target in every way, so it can be [refuted](#refuting) like one:
a proof of `¬ ∃ x, Pre x` shows the spec's hypotheses can't hold.

<a id="existential-targets"></a>
**Existential targets.** Where what an agent constructs isn't itself the
result, such as an algorithm that solves a stated problem, state the target as
its existence, and leave the construction to the proof:

```lean
theorem main : ∃ c : Circuit, Solves c problem := sorry
```

The statement names only the problem and the model, the agent defines its
candidate in its submission, which may add declarations, and nothing the spec
defines changes: no hole, no `allow`, so `clean` means what it says. Keep
[holes](#replacing) for a spec whose answer is the value, such as a cost a
run minimises, and read their values in the audit's review list (see
[Auditing a run](#spec-audit)).

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

A declaration on a snapshot gets axioms through the theorems of it that it
uses: a finished proof of a lemma on a sorried skeleton is `sorry`. Checks
and creates on snapshots list those theorems in `inherits` (`{name,
module}`), and say in `own_status` what its own axioms make it: `proved` for
that finished proof. An audit reads such a status through the run that
proves the theorem (see [Through links](#through-links)).

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
  spec's statements kept, nothing [refutes](#refuting) the target (its
  `refuted_by`), no amendment supersedes the spec, and the target is `proved`, or `conditional` only
  on assumptions the spec permits, those declared in a spec snapshot's module
  or in a module it builds on. The records of a module the kernel hasn't
  re-checked come from its build's report, which the code being judged could
  influence, so they don't count: `unverified` lists those spec snapshots
  and heads, and their comparison's `reasons` say to verify each with
  `POST /snapshots/{id}/verify`. Each of `assumptions` says whether the spec
  permits it (`in_spec`).
- **Amendments and refutations.** `amendments` lists the
  [amendments](#amending) of the spec snapshots, both those they make and
  those made of them. `superseded` lists each spec snapshot an amendment
  supersedes, `{spec, by}`, with `by` the spec now current, and any one
  makes the run not `clean`: it is judged against the current spec.
  `refutations` lists the
  snapshots that [refute](#refuting) a spec snapshot's target, or a target
  of a spec it amends that it states the same: any one makes the run not
  `clean`. A target refuted that a head proves is `contradicted`: the
  spec's assumptions contradict each other.
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
- **Review.** With `review=true`, `review` lists, for each spec snapshot and
  head it reaches, what a reviewer reads to know what the spec's statements
  say there: `{spec, head, declarations, library}`.
  - **The walk.** It starts from the spec's declarations and follows, on the
    head, each declaration's type and the values of definitions and
    inductive types, through the head and the snapshots it builds on. It
    doesn't follow theorems' proofs, so a candidate built inside a proof
    adds only its statement, and axioms are leaves.
  - **The declarations.** Each is `{name, kind, origin, snapshot_id?,
    signature?, type?, hole?, value?, holes?}`. `origin` is `spec` (the
    spec's own, as the head keeps them), `head` (new in the head, such as
    what a hole's value now uses), or `snapshot` (a snapshot below, with its
    id). `signature` has notation, and `type` has full names and no notation.
    A hole, or a definition `allow` names, has `hole` and its `value` on the
    head, printed with full names and no notation, which the code can't
    dress up, each `sorry` as `sorry`, and cut at 4,000 characters. So has
    each definition the head adds that a hole's value reaches, its own, a
    non-public one of a module-system file included, or one of a snapshot
    it builds on that the spec doesn't, as the value can hide behind one. A
    hole may be a definition of a snapshot the spec builds on, and is
    followed wherever the head declares it. Where such a value is missing,
    `unprinted` says why: it didn't print, its module has no export, or the
    export doesn't have it. `holes` names the other holes a value mentions, to
    read with it. What Lean derives of a declaration, such as a recursor or
    `casesOn`, isn't listed. Snapshots built before values were printed
    have none of those. Module-system ones built before private declarations
    were exported have them once a worker has exported them again, which
    workers do as they start and every hour.
  - **`library`** names the constants of Lean, Mathlib and the like that
    they use, which the version fixes.
  - **The cap.** The list holds at most `review_limit` declarations over all
    its entries, 1 to 10,000 and 1,000 unless given, in their order; a
    value outside that range is a 400. `review_left_out` counts the
    declarations past the cap, and is absent when there are none.
  - **Trust.** The text is what the builds printed, for reading.
    `comparisons`, from verified records, is what acceptance rests on.
  - **What to look for.** A proof can be valid and still miss the claim:
    - a budget defined as the candidate's own cost, so `cost ≤ budget` holds
      for any candidate;
    - an acceptance condition set to `True`;
    - an input promise narrowed until the problem is trivial;
    - an operation added to the model as a native gate.

    Made to a definition that isn't a hole, each is a change the comparison
    refuses, and the run isn't `clean`. Made to a hole, it is allowed, and
    only a reader catches it: so read each hole's value, and prefer
    [existential targets](#existential-targets) where the value isn't the
    result.
<a id="through-links"></a>
- **Through links.** A head's records are what its build found. A theorem
  proved with a lemma of another snapshot, where the lemma was `sorry`,
  depends on `sorryAx` through it, and still does once another snapshot
  proves the lemma, until the head is rebuilt onto that proof. So the audit
  reads such a theorem through the lemma's run: when the run has one head,
  verified, that proves the lemma with the same statement, and changed no
  definition there but those that were `sorry`, the theorem depends on what
  that head's lemma does, recursively. The target's `linked_status` is its
  status read so, when it differs from `status`, and `through` lists the
  lemmas it was read through, each `{name, snapshot_id, head}`. A proof can
  be swapped for any other of the same statement, so this is the kernel's
  claim; `accepted` and `clean` still rest on `status`, the records, until
  the run is [settled](#settling). Where a condition fails, or the lemmas
  justify each other, the lemma counts with the axioms its records name.
  - **`unlinked`** says, for a target that isn't `proved`, why each lemma it
    inherits through wasn't read so, `{name, module, reason}`: nothing
    replaces its snapshot yet, its run forks, a module isn't verified, the
    head states it differently or changes a definition that wasn't `sorry`,
    or it is refuted. A head that isn't verified, or was described before
    builds said what a declaration inherits, says so for itself.
  - **`unsettled`** lists the heads with such a target, each `{head, swaps,
    blockers?}`: `swaps` are the snapshots whose modules the head builds on,
    each with the head of its run that a settle builds on instead.
  - **`blockers`** say why a settle can't rebuild the head as it is: a swap's
    head that needs settling too, on a run whose spec isn't in `against`
    (add it); a name a swap brings in that the head declares too; a
    statement that uses a private name of a swapped module; or a spec
    statement that uses a definition the swap fills, which `allow` doesn't
    name.
  - **Older records.** A head built before builds said which axioms are a
    declaration's own has nothing to read through:
    `POST /snapshots/{id}/verify?again=true` describes it again (see
    [Upgrading](#upgrading)).
- **Counts.** `summary`, `assumptions` and `not_proved` are those of the
  heads, each standing for its module.
- **Parameters.** The ids are of one version, which the audit uses; ids of
  several versions are a 400 that names them, and so is a `version` that
  isn't theirs. `label` doesn't combine with `against` (400), nor does
  `allow`, which can't be empty either, nor `review`, which is `true` or
  `false`, and a spec snapshot that doesn't exist is a 404. A [rebuild](#rebuilding) copies
  only a run's heads, so audit against the same ids after it. A spec
  snapshot that was still a head, with nothing replacing it yet, is copied
  like any head, and with `retire` its old id is deleted if its copy replaces
  it. Its copy in `created` then replaces a snapshot that is gone, so nothing
  shows that it states what the original did, and an audit against it isn't
  `clean`: rebuild a run's spec snapshots without `retire`.

<a id="comparator"></a>
**Accepting a run.** The audit's comparison is pisa's own code.
`lake comparator`, which Lean ships, judges a run with none of it, and
accepting a run takes both. "Accepted" means that comparator judged the
modules pisa built and kernel-checked when the snapshots were created,
exported with Lean's bundled `leanexport`, and found that each head proves
its spec's theorems: acceptance builds nothing. The export is for others to judge the run
with comparator outside pisa, from source; `scripts/e2e.py` checks that it
builds with `lake build` and that comparator accepts it there.
- **The export.** `GET /snapshots/export?against=<id>` (repeated, with
  `allow=` as for the audit) answers the run as a Lake project in
  `pisa-run/`, as a tar.gz:
  - every module the spec snapshots and their heads need, with its stored
    code. A pair whose spec is of a [named run](#run-names) is named by the
    run: its head `PisaRun/<name>.lean` and its spec, if it isn't the head,
    `PisaSpec/<name>.lean`, unless the run is forked, whose heads keep their
    hash names. Other modules, those the pairs import among them, are
    `PisaSnap/s….lean`. Each file imports the others by those names, and the
    configurations name the modules the same way: the export is a renamed
    build of what pisa built. Acceptance judges pisa's builds, with the same
    configurations naming the modules by their hash, which its pairs record
    as `challenge_module` and `solution_module`;
  - the worker project's `lean-toolchain` and `lake-manifest.json`, and a
    `lakefile.toml` that requires what the project requires and builds the
    modules with `autoImplicit` off, as pisa builds them. When the modules
    import a library the project builds itself, because the project is that
    library's own checkout, it requires the project too, from the workers'
    `PISA_PROJECT_GIT` at `PISA_PROJECT_REV`; without them such an export is
    refused (see [A pool for another project](#another-project));
  - a configuration in `comparator/` for each spec snapshot and head the
    audit reaches, listed in the export's `README.md`.
- **A configuration.** The spec snapshot's module is the challenge and the
  head's the solution:
  - `theorem_names` are the spec's theorems, witnesses included, without
    what Lean generates;
  - `definition_names` are the spec's holes and `allow`;
  - `permitted_axioms` are `propext`, `Quot.sound` and `Classical.choice`, and
    the assumptions the spec permits, those declared in its module or in a
    module it builds on.

  Comparator accepts the head when each theorem is proved there with the same
  statement, the definitions those statements use are the same but for the
  holes, an assumption keeps its statement, and the proofs use no other
  axiom. It replays them in Lean's kernel and, when paranoid, in the
  external kernels Lean bundles (for a pair whose spec permits a
  non-standard axiom, only those that support one, as below).
- **Running it.** `POST /snapshots/accept {"against": [...]}` audits the run,
  as `audit?against=` does with the same `allow`, and runs
  `lake comparator` (paranoid, with `external_kernels`, as below) on a worker on
  exactly the pairs the audit compared, each in turn:
  - The worker links the module of each spec snapshot and head, and every
    module it builds on, into a directory of the judgment's own: the files
    pisa built and kernel-checked when the snapshots were created, each
    checked against the SHA-256 its record holds. A snapshot deleted after
    that takes nothing from the judgment.
  - It exports each pair's two modules with the toolchain's `leanexport`, in
    its sandbox, with nothing writable: the targets comparator would export,
    the theorems, axioms and definition holes the configuration names and
    the constants the kernel builds in. The exports are kept by what they are
    made of (the modules' builds, the targets, the exporter and the pool
    version), the 64 used last, so a module judged again isn't exported
    again.
  - Comparator judges the pair on the two exports
    (`--challenge-from-export`, `--solution-from-export`), with the export's
    configuration naming the modules by their hash. It builds nothing and
    loads no project, so no code of the run runs while it is judged, and
    nothing a pair or a judgment does reaches another.

  `paranoid: false` uses Lean's kernel only, and `timeout` (default 600) is
  each pair's limit in seconds, its two exports and comparator together. A
  pair that runs out keeps what comparator printed until then in its
  `output`. A run
  whose audit lists `unsettled` heads, which hold only through links, is a
  409 `conflict` at once, naming the settle to send first: comparator would
  judge records that read `sorry`. Nothing is stored.
  Each pair has comparator's `exit_code` (0 accepted, 1 refused or failed, 2
  couldn't start, none past `timeout`, when a signal killed it or when it
  didn't run) and the end of its `output`. A pair
  not accepted says why in `error.kind`: `refused`, comparator's verdict;
  `export_failed`, pisa couldn't export a module from its build, so
  comparator didn't run; `timeout`;
  `out_of_memory`, the worker's memory limit killed comparator or a kernel;
  `cannot_run`, comparator couldn't start or run its checks, a kernel
  couldn't run or exited with a code that is no verdict, or a signal that
  wasn't the time limit's or the memory limit's killed comparator;
  `unsupported`, comparator's last error line says con-leche or con-ron,
  which pisa ran, exited with 2: they don't support a non-standard axiom;
  or `nothing_to_judge`, a spec that names no theorem comparator can judge,
  for which it doesn't run. `build_failed`, a module that didn't build in
  comparator's sandbox, is only in acceptances stored while comparator built
  the run. Only `refused` is a verdict on the proofs: a kernel that was
  killed or couldn't run never makes one. The run is `accepted` when the
  audit is `clean` (`clean`, with the whole `audit` and the `heads` both
  judged), comparator accepted every pair, and there is one at least. Only
  comparator's exit code decides whether a pair is accepted, never what it
  printed, which a kernel's output and the names in the solution's
  declarations can imitate.
  With `paranoid`, comparator also runs the kernels Lean bundles. Pisa names
  all five in the configuration's `external_kernels`, from the toolchain's
  `bin`, and never passes `--paranoid`, which names the same five but can't
  pass them arguments. Lake runs them sorted by name, Lean's own kernel last:
  Lean paranoid, con-leche, con-ron, lean4lean, nanoda, Lean default.
  con-leche and con-ron check declarations in parallel, a worker for each
  core, and a worker takes gigabytes: con-ron about 6250 MiB, con-leche about
  3790 MiB. On a computation-heavy proof a dozen workers outgrow a slot, and
  the kernel is killed, which Lake prints as `rejected`, as it does for any
  non-zero exit. So when the slot has a
  memory limit (`PISA_MEMORY_MAX_MB`), pisa passes `--jobs=N` to them, one
  argument (`--jobs N` exits 3): the limit less 3072 MiB, over a worker's
  memory, at least 1. A 16000 MiB slot runs con-ron with `--jobs=2` and
  con-leche with `--jobs=3`; a slot without a limit passes no `--jobs`. When
  the spec permits a non-standard axiom, con-leche and con-ron don't support
  one and decline it, so pisa leaves them out of `external_kernels`. A pair's
  `kernels` list each kernel's verdict: for an accepted pair, what pisa
  configured in Lake's order, each `accepted`, and con-leche and con-ron
  `unsupported`, with a `reason`, when they were left out. For a pair
  comparator failed, they are in that order too, as comparator printed and
  exited them, for the reader. Lake's final error line,
  `error: <kernel> exited with N`, reports the code of the first kernel that
  failed only, and pisa reads it from the last such line that names a kernel
  it configured, exactly. For that kernel, 1 is `rejected` and the pair
  `refused`; 2 for con-leche or con-ron is `unsupported`; 128 or more while the
  slot's out-of-memory count moved is `out_of_memory`; any other code
  (3, a kill without it, 42) is `cannot_run`. A kill is never `refused`.
  Kernels before the first failure are `accepted`, and after it `failed` if
  Lake printed that they rejected the solution, as it does for any non-zero
  exit, and no code is known, else `accepted`. A kernel that didn't accept
  has its own first line in `reason`, cut at 200 characters, such as
  `con-ron: declined: non-standard axiom ...`; a kernel that was killed has
  none. A line of the output can be imitated by the solution, so these are
  for the reader and never settle a pair, which only the exit code does.
  The result is stored with an `id`, and `GET /snapshots/accept/{id}` reads
  it again; `GET /snapshots/accept?spec=<id>` lists a spec snapshot's,
  newest first. It names the code it judged, which
  stays named when snapshots go: each pair's `spec_module` and
  `head_module`, and the audit with its review list, at the default cap.
  Read back, it says `stale` once the run's heads are no longer the ones it
  judged, a spec is amended, or a target is refuted that its audit didn't
  list. A result
  over the record's 6 MiB is stored without its pairs' `output`, then
  without the review, and `truncated` says which. A pair that imports
  Mathlib takes about 25 s, or 45 s paranoid, so send it with
  `Prefer: respond-async`. Or untar the export, `lake build` it and run
  `lake comparator --paranoid --config comparator/1.json` there: it needs
  `bwrap`. That runs the same five kernels with their own workers, which
  can outgrow a small machine: to bound them, and for a pair whose spec
  permits an axiom of its own, which `--paranoid` exits 1 for when con-leche
  exits 2, add the kernels pisa names to the configuration
  (the paths are the toolchain's `bin`, `lean --print-prefix`; leave out
  con-leche and con-ron for a spec axiom, and `--jobs=N` for no limit):
  ```json
  "external_kernels": {
    "Lean paranoid": ["<prefix>/bin/leanchecker-paranoid", "--silent", "--from-export"],
    "con-leche": ["<prefix>/bin/con-leche", "--jobs=3"],
    "con-ron": ["<prefix>/bin/con-ron", "--jobs=2"],
    "lean4lean": ["<prefix>/bin/lean4lean", "--import"],
    "nanoda": ["<prefix>/bin/nanoda_bin"]
  }
  ```
  and then run `lake comparator --config comparator/1.json`.
- <a id="accept-progress"></a>**Progress.** While an acceptance runs, its
  job's 202 at `GET /jobs/{id}` has `progress`, which the worker writes each
  time it renews its claim, every third of `PISA_LEASE_SECS` (10 s with
  Compose's 30): `{phase, kernel?, seconds, peak_memory_mb?}`. All of it is
  pisa's own, never read from what comparator prints:
  - `phase` is pisa's step: `{"name": "linking"}` while it links the
    modules, `{"name": "waiting"}` while it waits for a slot and for the
    slot's memory to be freed, `{"name": "exporting", "pair": n, "side":
    "challenge"}` or `"solution"` while it exports one, and
    `{"name": "judging", "pair": n}` while comparator runs, pairs counting
    from 1 in the order of `pairs`.
  - `kernel` is the kernel comparator is running, named as in `kernels`,
    which the worker finds by its executable (`leanchecker` is Lean
    default) among the processes in the slot's cgroup.
  - `seconds` is the time in the phase, with that kernel, and
    `peak_memory_mb` the most memory the slot's cgroup held in it, page
    cache included as its memory limit counts it: the worker starts
    `memory.peak` again at each phase and each kernel, which needs Linux
    6.12 or later. Linking and waiting, before the slot, have no
    `peak_memory_mb`.

  A worker without cgroups gives neither `kernel` nor `peak_memory_mb`.
  Each pair keeps its phases in `phases`, each `{name, seconds,
  peak_memory_mb?}`: `exporting challenge`, `exporting solution`, `judging`
  for comparator reading and comparing the exports and running any kernel
  the worker didn't see, and each kernel the worker saw by its name. The
  worker looks at the slot about each second, so a kernel shorter than that
  may have no phase of its own and count in `judging`, a kernel's `seconds`
  are good to about a second, and a peak right at a boundary may count in
  the next phase. If the job's claim moves to another attempt meanwhile, as
  when a stall outlasts the lease and another worker claims the job, the
  first worker stops comparator as soon as it sees that, kills what is left
  in its slot, and stores nothing: the run has the other attempt's
  acceptance only.
- **Where the audit and comparator differ.** Each pair's `left_out` and
  `theorems` say what its configuration couldn't name. The full list:
  - *Private names* differ between two modules, so a configuration leaves out
    private theorems, holes and assumptions (`private`), and a target whose
    statement uses a private name of the spec's module, directly or through
    the spec's definitions (`private_in_statement`): comparator would refuse
    it. The audit compares them, reading a module's own private names alike.
  - *Holes that aren't plain definitions.* Comparator takes as a hole only a
    `def` that both sides declare. A hole or `allow` name that is an inductive
    type, a structure or an `opaque` is left out
    (`not_a_definition_in_spec`), and so is one the head no longer builds on
    (`not_a_definition_in_head`). The audit allows them.
  - *Definitions no statement uses.* Comparator compares what the theorems'
    statements reach; the audit compares every declaration of the spec.
  - *A spec of definitions only* has no theorem for comparator to judge: the
    pair is `nothing_to_judge`, and the run isn't accepted. The audit
    compares its statements.
  - *Heads it refuses that keep the statements*, such as code that opens
    `Classical` in its header, or whose auxiliary declarations differ: the
    audit is `clean` and the pair `refused`.
  - *Axioms.* Comparator permits the standard three and the spec's
    assumptions; the audit's `not_proved` uses `PISA_ALLOWED_AXIOMS`. A spec
    assumption the head no longer builds on is left out
    (`axiom_not_in_head`): comparator needs each on both sides, and the head
    can't use it.
  - *Links' `allow`s* count for neither: both compare under the spec's holes
    and the request's `allow`.

  So comparator adds to the audit, and doesn't replace it.
- **The export, outside pisa.** Its `README.md` gives the toolchain, says to
  run `lake exe cache get` first when the project requires Mathlib (comparator
  builds in a sandbox without network, and would compile Mathlib from
  source), and lists each pair with its theorems and what it leaves out, and
  each file with the snapshots on its module and their labels. Then
  `lake build` and `lake comparator --paranoid --config comparator/<n>.json`
  judge it as pisa does, but that pisa names its kernels in `external_kernels`
  and bounds con-leche's and con-ron's workers by the slot's memory, and leaves
  out those two for a spec axiom (see above).
- **A Palomar package.** `profile=palomar` exports one pair of the run as a
  Palomar package instead, in `palomar/`, as the Erdős campaign's
  `pisa_to_palomar.py` made one. `pair=<n>` names the pair, from 1 as
  `comparator/<n>.json` numbers them; a run of one pair needs none. `pair`,
  `challenge_module` and `solution_module` need the profile.
  - The pair's spec module is `Challenge.lean` and its head's
    `Solution.lean`, or as `challenge_module` and `solution_module` name
    them; the modules they import of the run keep their names. Each plain
    file becomes a module-system file, as Palomar requires: `module` first,
    its imports `public`, and `@[expose] public section` after them. A
    module-system file stays as it is. Imports follow the new names.
  - `lakefile.toml` is named by the run (its name with `_` for `.`, or
    `pisa_run`), with a `version`, the challenge's and solution's libraries
    as `defaultTargets`, `autoImplicit` off, and Mathlib alone required, at
    the commit the worker's manifest resolved. `lake-manifest.json` is the
    worker's without its other packages (Physlib), each package's `inputRev`
    its commit. A package that isn't a GitHub repository at a commit is a
    500 `internal`: it is the worker's project, not the request.
  - `comparator.json` is the pair's configuration, with the new names.
  - `formalization.yaml` (v0.4, as JSON) has every key of a Palomar
    package's: the run's name and the solution's theorems, and in `status`
    no `sorry` and no axiom beyond the standard three, which
    `comparator.json` makes true of a solution it accepts. Everything else,
    the licence, authors and maintainers, description, classification,
    sources and the rest, reads `REVIEW`. `REVIEW.md` lists what to decide
    before publishing: the licence, authors, attribution, fidelity to the
    source, the rest of the metadata, and validation. `conversion.json` names the pair and each module's SHA-256
    before and after, and the packages removed.
  - A `bad_request` names what a package can't hold: a spec that permits an
    axiom beyond the standard three, a challenge that imports anything but
    `Mathlib`, `Init` and `Lean`, a spec that is its own head, with no
    solution yet, or a pair that imports a module that is neither the run's,
    Lean's (`Init`, `Lean`, `Std`, `Lake`), Mathlib's nor of a package
    Mathlib builds on (`Batteries`, `Aesop`, `Qq`, `ProofWidgets`,
    `Plausible`, `ImportGraph`, `LeanSearchClient`, `Cli`). That is an
    allowlist: the worker's Physlib, `QuantumInfo`, `PhyslibAlpha` and
    `Cslib`, and whatever else the worker's project adds, are refused.

  New headers and names change what Lean builds: build the package, judge
  it with comparator, and cross-check it in pisa before publishing it.
- **The worker.** A judgment takes a REPL slot. The exporter runs in pisa's
  sandbox and the slot's cgroup, with the judgment's modules on its search
  path in the snapshot cache's place, `LEAN_SYSROOT` the toolchain's prefix,
  so that it never runs a `lean` it finds to ask for one, and
  `LEAN_ABORT_ON_PANIC=1`, so that a target it can't find fails the export
  instead of leaving it out. `PISA_LEAN_EXPORT` names it (default
  `<prefix>/bin/leanexport`, `<prefix>` being `lean --print-prefix` in the
  project). Comparator runs in the slot's cgroup and outside pisa's sandbox,
  as it sandboxes the kernels itself with `bwrap`, with only `PATH`, `HOME`,
  `ELAN_HOME`, `LANG` and `TMPDIR` of the worker's environment, in the
  judgment's directory, where a copy of the project's `lean-toolchain` has
  elan pick its `lake`. It needs `/run/user`, which the worker image
  creates. `PISA_LAKE` names `lake` (default `lake`). A judgment's directory
  goes when it is done, and any left at once when the worker starts; the
  exports are kept in the work directory's `exports/` across restarts.

<a id="agent-runs"></a>
**Running agents on snapshots.** A leader hands targets to agents, and
[replacing](#replacing) keeps their submissions in one run:
- **The leader.** It creates the spec snapshots, skeletons with `sorry` and
  one or a few targets each, with their [witnesses](#witnesses), with
  `verify: true` (see [Verifying](#verifying)) and, where agents are to fill
  in definitions as well as proofs, with those definitions as `holes` (see
  [Replacing](#replacing)), labels the
  run (see [Labels](#labels)) and holds the spec ids. They stay valid through
  [rebuilds](#rebuilding) without `retire`. It names each spec's run (see
  [Naming a run](#run-names)), so that agents can import a lemma's run by
  name.
- **Definitions.** A spec's targets usually state facts about definitions
  of its own, such as a paper's objects. Put those definitions in a base
  snapshot of their own, with no `sorry`, and have each spec import it: a
  replacement must declare the spec module's own declarations itself (see
  [Reusing a proof](#reusing-a-proof)), so a helper snapshot that needs a
  definition declared in the spec can't be imported next to the
  replacement, which declares it again. With the definitions below the spec,
  helpers import the base, never the spec, and the replacement imports the
  base and the helpers and re-declares only the targets. Specs that build on
  another's definitions import its base too, and a target proved from
  another spec's target imports that spec's run (see *Lemmas* below):

  ```text
  defs.A (definitions, proved)  ←  spec.A (targets, sorry)  ←  head of spec.A
        ↑                                                       ↑ imports
        └──────────────  helpers (lemmas, as named runs)  ──────┘
  ```
- **Imports.** Give a spec the imports its proofs will have. A replacement
  that imports helpers built with `import Mathlib` sees all of Mathlib's
  instances, and a statement of the spec's whose instances Lean then finds by
  another path has another type: the replacement is refused although each
  file checks (see [Canonical form](#canonical-form)). So a spec whose
  helpers will import Mathlib imports it too, or helpers keep to the spec's
  imports.
- **Agents.** Each gets its target's spec id and labels every snapshot with
  the run's labels. Scratch work goes to `/check` (see
  [Snapshots](#snapshots)), which keeps no snapshot; a check with `against`
  the spec says whether the code keeps the spec's statements before it is
  submitted (see [Checking against a spec](#check)). To submit, an agent
  creates its code with `replaces` the snapshot it last submitted for the
  target, or the spec snapshot at first. It imports another target's run as
  `import PisaRun.<name>`, which builds on that run's head. [Search](#snapshot-search) shows
  heads only, so what an agent imports from a hit is a head.
- **On a `conflict`** (see [Replacing](#replacing)). The run moved under the
  agent: a rebuild copied its last version onto a new base, or another agent
  extended it. Nothing was built. The agent takes a head from the answer's
  `heads` and submits again, replacing that head. Code that imports runs by
  name is sent as it is, since each name resolves to its run's head again.
  Code that imports modules by hash moves onto the head's imports first,
  those of its `code` (`GET /snapshots/{id}`, see [Snapshots](#snapshots)).
- **Parallel attempts.** Agents on one target pass `"fork": true` on their
  first submission and extend their own branches. The leader keeps one and
  drops the others with `DELETE /snapshots/{id}?with_replacers=true`, on the
  ids [`forks`](#spec-audit) names (see [Retention](#retention)). Every fork
  in the audit is one of these.
- **Lemmas.** A target proved with a lemma that another agent is still
  proving needs no rebuild when that proof lands: the audit reads the target
  through the lemma's run (see [Through links](#through-links)), and the
  leader [settles](#settling) the run once, before accepting. Agents go on
  meanwhile on the heads they hold. Until then such a target is `sorry`; its
  `own_status` says whether its own proof is finished, and `inherits` names
  the lemma it waits for (see [Statuses](#statuses)).
- **One target, several agents.** A target too large for one agent splits
  through an interface: a helper run, published first, that only defines the
  pieces' statements as `Prop`s (`def PartA : Prop := …`). Each agent proves
  its piece, `theorem partA : PartA`, in a run of its own that imports the
  interface, and one proves the target from `PartA → PartB → …`. The
  interface is a definition base like any other: agents import it and leave
  it alone, and a change to it is a new head that the others move onto.
- **Re-checks.** When something below the run changes, such as a base
  edited, the leader [rebuilds](#rebuilding) without `retire` while agents
  work. Agents meet the copies as `conflict`s and move onto them. A rebuild
  item that loses a race to an agent's submission fails; send the rebuild
  again and it copies the agent's version. The run doesn't fork, and no
  submission is lost. `retire` would delete versions agents hold, so their
  submissions would answer `not_found` without `heads`.
- **Accepting.** Once [settled](#settling), `POST /snapshots/accept` with the run's spec ids accepts
  the run when its audit is `clean` (see [Auditing a run](#spec-audit)) and
  comparator, which shares no code with the audit, accepts every pair the
  audit compared (see [Accepting a run](#comparator)), with `allow` each
  definition the leader decided later may change, such as one its own base
  edit changed; the spec's holes may change already. What the agents' links
  allowed doesn't count; `comparisons` and `links` show what each head
  changed, and `review=true` what the targets' statements depend on, each
  hole with its value, for the leader to read before accepting.
- **Afterwards.** The leader may retire and delete superseded versions.
  Deletes splice, so the run stays whole, and its links merge once reviewed
  (see [Retention](#retention)).
- **On a `not_found`** for its `replaces`, the create lost a race with a
  delete (see [Retention](#retention)). The agent finds the run's heads with
  `audit?against=<its spec id>` (see [Auditing a run](#spec-audit)) and goes
  on as for a `conflict`.
- **On a false target.** An agent that finds a target false proves it:
  `theorem r : ¬ type_of% @t := …` on the spec, created with `refutes` (see
  [Refuting a target](#refuting)). The audit shows the target refuted, and
  the run isn't `clean`. The leader then [amends](#amending) the spec, with
  the reason, and resubmits the run onto the new spec.

<a id="amending"></a>
**Amending a spec.** A spec can be wrong: a target false as stated, a
hypothesis missing, a hole that should be a definition. A leader fixes it
during a run by creating the corrected spec with `amends: {snapshot_id,
reason}`, the spec it amends and why (1 to 4,096 characters):
- **The new spec** is a spec of its own that replaces nothing (`replaces`
  with `amends` is a 400). It keeps the amended spec's `holes` unless it
  names its own, and `holes: []` leaves none. It is always
  re-checked through the kernel (`verify: false` is a 400).
- **The record.** Its create compares it with the amended spec as a
  replacement that may change nothing would be compared (see
  [Replacing](#replacing)), holes included, and records what changed
  (`changes`, as a refused replacement's) and the declarations it adds
  (`added`), with the reason, and the holes before and after
  (`holes: {before, after}`) when they differ. The response's `amendment` has it, and an
  audit against either spec lists it in `amendments`. Deleting the new spec
  deletes the record; deleting the amended one keeps it.
- **It supersedes the amended spec.** The current spec is the amendment,
  followed through the amendments made of it. An audit against an amended
  spec says so in `superseded` and isn't `clean`, and a create that replaces
  into its run, a fork included, is a `conflict` whose message names the
  current spec and whose `heads` are that spec's run's heads.
- **Once.** A spec is amended once: a second amendment of it is a `conflict`
  that names the current spec, which is the one to amend.
- **Carrying the run over.** `POST /snapshots/resubmit {from, to}`, with
  `to` a spec that amends `from`, compares each head of `from`'s run with
  `to`, as an audit against `to` would, allowing `to`'s holes to change. A
  head that keeps `to`'s statements is created again: the same code, so the
  same module, replacing `to`, with its labels or the request's `labels`.
  A head comes over forked once `to` has a replacer, an earlier head's or
  another create's, so the heads of a forked run come over forked. A head that changes one of
  `to`'s statements is listed with its `reasons` and `changes`, and one
  whose record isn't verified with that reason; neither is created.
  `dry_run` only compares. Each create has a key of the resubmit's, so
  sending it again replays what is done. `from` may be deleted by then.
  Agents then work on `to`'s run, and the leader audits and accepts against
  `to`.

<a id="refuting"></a>
**Refuting a target.** A run can show that a target is false, and a
snapshot records it: a create with `refutes: {snapshot_id, target}`, the
spec snapshot and its target.
- **What it must be.** The code imports the spec's module and proves, with
  no `sorry` and no assumption, a theorem whose statement is `¬` the
  target's statement exactly: `theorem r : ¬ type_of% @t := …` states it so.
  The theorem's `negates` is the hash of that statement's canonical text,
  every name written whole, which equals the target's `type_hash`; a
  universe parameter counts by its position, and a private name of the
  refuter never reads as the spec's. It is always re-checked through the
  kernel (`verify: false` is a 400), and it is a snapshot of its own:
  `replaces`, `holes` and `amends` are 400s.
- **What it can't be.** A target whose statement uses one of the spec's
  holes, which a head may fill, directly or through a definition of the
  spec's or of a snapshot it imports, or a private name of the spec, which
  no other module can state, can't be refuted (`check.error` is a
  `bad_request`).
- **Otherwise** nothing is kept, and `check.reasons` says why: the code
  doesn't build on the spec, no theorem states `¬` the target exactly, or
  the one that does isn't proved.
- **The record.** The response's `refutation` names the theorem. An audit
  against the spec lists it in `refutations`, marks the target
  `refuted_by` the snapshot and not `accepted`, and isn't `clean`, so
  neither is an acceptance. A create that replaces into the run answers
  the run's `refutations` too, so the agents on it learn the target is
  false. The record outlives the refuting snapshot: once that is deleted,
  the audit marks it `gone`, and it still counts.
- **The way on** is to [amend](#amending) the spec. A spec that amends the
  refuted one and states the target the same, by its statement's hash,
  is shown the refutation, under the spec and target it was made against.

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
- **Runs whose spec was amended.** A head of such a run isn't copied either,
  as the run takes no more snapshots: `superseded` lists it as
  `{snapshot_id, by}`, with the spec now current. It doesn't change `ok`.
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
  If that server stops first, the job reads as lost (503 `worker_lost`,
  `resend_safe`): send the rebuild again. `DELETE /jobs/{id}` leaves a running rebuild running. A dry run
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
- **Limits.** At most 10,000 items, carrying at most 64 MiB of code in all.

<a id="settling"></a>
**Settling a run.** `POST /snapshots/settle` rebuilds the heads the audit
lists in `unsettled` onto the heads they are read through (see
[Through links](#through-links)), in one plan, after which their targets
hold from their own records:

```text
POST /snapshots/settle {"against": ["…~9c0e…", "…~5d1f…"], "dry_run": true}
  → {"ok": true, "changes": [], "plan": [
       {"level": 1, "snapshot_id": "…~4b7a…", "module": "PisaSnap.sT…", "new_module": "PisaSnap.sT′…",
        "new_header": "…", "code": "…", "labels": {…}, "key": "settle:…", "replaces": {…}}, …]}
```

- **The request.** `{against, allow?, dry_run?, verify?, timeout?}`:
  `against` and `allow` as for the [audit](#spec-audit), whose `unsettled`
  the settle plans from, then `verify` and `timeout` for the creates. Only
  the heads of the runs `against` reaches are rebuilt.
- **The plan.** Each item copies a head, its imports of each swapped module
  replaced by the swap's head, as a [rebuild](#rebuilding) copies it, and
  replaces it, allowing what the swaps changed. A module between a head and
  a swapped one is a support item; a swapped module is never rebuilt. A
  swap's head that is itself unsettled is copied first, and the heads that
  build on it build on its copy. Keys are `settle:<snapshot or module>:<new
  module>`, so sending the settle again replays what is done.
- **Refused.** Nothing to settle is an empty plan, `ok: true`; the audit's
  `unlinked` says why a target that isn't `proved` has nothing to settle. A head with
  `blockers`, or swaps that would make a cycle, refuse the whole plan:
  `ok: false`, and `changes` says why, one line each.
- **Running it.** As a rebuild runs, and answers, with `created`, `failed`
  and `skipped`; async with `Prefer: respond-async`, a job of kind
  `rebuild_snapshots`. The kernel's claim covers proofs, not elaboration: a
  head's tactics can see what a swap brings in besides the lemma, such as
  `simp` lemmas, instances or notation, and fail. The item then fails with
  its create's `check`, and the head stays as it was.
- **Afterwards.** Audit against the same ids: the targets are read from the
  copies' records, so `status` is what `linked_status` was, and `clean`
  follows.

**How it works.**
- **Building.** A module is its code compiled to the module
  `PisaSnap.s<hash>`, where `hash` is a hash of the code, by `pisa build`
  (`docker/pisa-lean`): it writes what `lean -o` writes, and in the same
  process reports the module's declarations and their exports.
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

**Several files.** A request is one module, so send a project's files one at
a time, imports first, each as a snapshot or a [named run](#run-names) that
the files using it import. Pasting them into one request instead makes them
one file: one's `private` names, `open`s and scoped notation reach the next,
and two snapshots that each paste a file's code declare its names twice, so
no code can import both.

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
**Verifying.** The kernel re-check runs in a clean process (`pisa verify`): it
imports the snapshot's dependencies again and re-runs the kernel over its
declarations, so its statuses don't depend on the build being honest. It adds
about 2 s to a small chunk on `import Mathlib`. Every create runs it, unless
`verify` in the request says otherwise:

| `verify` | Kernel re-check at creation |
| --- | --- |
| omitted | This chunk, or kept code that isn't verified yet; if the kernel rejects it, it isn't kept |
| `true` | This chunk and any unverified imports; the snapshot is `verified`, or `check` has an `error` |
| `false` | None, and the statuses are the build's own report; module-system code is re-checked all the same, as only the re-check can describe it |

`verified` means the snapshot and everything it imports have passed the
re-check. So a chunk re-checked on top of unverified snapshots isn't `verified`
yet. A module that loses it, as one the kernel rejects when verified `again`
does, takes it from everything built on it, and its `check.reasons` say why.

The re-check at creation has the request's `timeout` for each module, once
their imports are loaded. If it runs out of time or memory, code outside the
module system with no macros, its own or an import's, is kept, not
`verified`, with the statuses of the build's own report, and the response's
`unverified` says why; `POST /snapshots/{id}/verify` finishes the re-check.
Other code, and code asked to be verified, isn't kept, and `check.error` is a
`timeout` or `out_of_memory`: module-system code exports its theorems as
axioms to what it imports, so only the kernel re-check can say what it
proves, and macros could expand into what the policy forbids. A re-check
that fails, rather than run out of time or memory, keeps nothing: `check.error`
is a `crash` with what the verifier said. A create with `verify: true` whose
imports' re-check doesn't finish answers with that kind, `timeout` included,
and `lean` only when the kernel rejected one.

`POST /snapshots/{id}/verify?timeout=60` re-checks the snapshot's module and
everything it imports, imports first, in one process that loads their imports
once, and records each module as it passes. A later run only covers new
chunks, and a retry resumes where the last one stopped. `verified` and
`status` belong to modules, so every snapshot of a module shows the result.
- `timeout` (default 60) is the seconds each module may take once the
  imports are loaded.
- `snapshot` is the snapshot as the re-check left it, and `checked` lists the
  modules this run re-checked and passed.
- If the kernel rejects a module or the re-check doesn't finish, the run
  stops, still with a 200, and `failure` has the module, the reason and its
  `kind`: `lean` when the kernel rejected it; `timeout` or `out_of_memory`,
  worth a retry with more time or memory; `crash` when the verifier failed.
  Nothing is deleted, because other code may import it: the module stays usable
  and not `verified`. Delete its snapshots if you don't want it.
- `status` and the module's `check` are updated from the re-check, whose axioms
  don't depend on the build being honest.
- `again=true` re-checks the modules that are verified already too, and
  records them anew, as for records made before a release that describes
  more, such as which axioms a declaration has of its own (see
  [Through links](#through-links)).

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
Once an hour one worker of each version sweeps them, a worker that has just
started included: the modules no snapshot
is on and no module imports go as a delete would remove them, then the
files no module's record names, once a day old, as a build uploads its files
before its record. A module goes only once it is a day old too, as a build
that a create's link refused is kept with no snapshot for the create that
follows (see [Replacing](#replacing)). Files are swept only from a store that names the worker's
own catalog: each version's catalog has a random id, which a worker writes to
`catalogs/<version>` in the store with its first upload, unless the store
names one already, or holds files more than an hour old of builds its catalog
has no record of. A worker on an empty, scratch or other database leaves the
store's files alone and logs an error, and so does a sweep that would delete
most of them (16 or more); `/health`'s `sweeps_refused` says why, until a
sweep goes through. A new store, which names no catalog until the first
upload, has nothing to sweep, which isn't a refusal.

**A database reset on a kept store.** The store names the old catalog, or
none, and holds builds the new one has no record of, so no worker sweeps it.
To have the new catalog own the store, start one worker with
`PISA_CLAIM_STORE=1`, which makes the store's marker name its catalog, then
unset it. Its sweeps then delete the old builds' files, unless they are most
of the store's: delete those by hand, under `snapshots/<version>/`.

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
Mathlib/Physlib oleans and the kernel are the trusted base. Acceptance judges
these same files, exported, not the stored code (see
[Accepting a run](#comparator)): what it accepts is what the store holds.

**Searching them.** The servers can search snapshot declarations, by
label, next to Mathlib's; see [Snapshots in search](#snapshot-search).

## Clients

Each client takes a default version and timeout. A create that names a spec,
with `replaces`, `amends` or `refutes`, runs on that spec's version and sends
none; an acceptance has a limit of its own, 600 s a pair, which a call can
set (`timeout`), and doesn't take the client's timeout.

A client may also take a [scope](#scopes), which it sends with every request:
`Client::with_scope` in Rust, `Client(url, scope={...})` in Python and
`scope` in TypeScript's options. Runs are read and named by name with
`get_run` and `name_run` (`getRun`, `nameRun` in TypeScript); see
[Naming a run](#run-names).

**Retries.** Every client sends a request again by itself, after a random
wait that grows each time, from 1 s to 30 s, and is at least the server's
`Retry-After`: up to 4 attempts within 5 minutes.
- **What is sent again.** A 429 is always sent again, since nothing ran. A
  request safe to send twice is also sent again after a 502, 503 or 504, or
  a connection that failed or dropped: reads, checks, tactics, goals,
  probes, verifies, creates and dry runs.
- **Creates.** A create without a `key` gets one of the client's, `client:`
  and 32 hex digits, so a retry can't keep it twice.
- **Batches.** A batch's items that no worker took are sent again alone.
- **Turning it off.** `with_retry(RetryPolicy::none())` in Rust,
  `Client(url, retry=False)` in Python, and `retry: false` in TypeScript send
  each request once.

**One call per kind of create.** `POST /snapshots` is five things, by the
fields it is sent with. Each has its own call:
- `create_snapshot` keeps plain code.
- `create_spec` keeps a spec, with its `holes`.
- `create_proof` keeps a proof that replaces a spec or a head of its run. On
  a `conflict` with one head, it sends the same code again replacing that
  head, with the same key, up to 3 times. It returns the last answer, whose
  `refused` says what to do next.
- `amend_spec` amends a spec, with a reason.
- `refute` refutes a target of a spec.

TypeScript names them `createSnapshot`, `createSpec`, `createProof`,
`amendSpec` and `refute`.

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

`Client`'s calls block. They release the GIL, so one `Client` can be shared
across threads, and they stop at Ctrl-C, dropping the request. `AsyncClient`
takes the same arguments, and its calls return asyncio futures instead,
which start at once; cancelling one drops its request. Either way, a job a
worker took runs on.

```python
import asyncio
from pisa_client import AsyncClient

async def main(codes):
    client = AsyncClient("http://localhost:8000", timeout=60)
    return await asyncio.gather(*(client.check(code) for code in codes))
```

Results are plain dicts; the type stubs describe their shapes. Exceptions
carry `.kind` and `.status`, and their class says what to do:
- `LeanError`: Lean or the policy refused the input.
- `LimitError`: the input hit a timeout, the memory limit, Lean's recursion
  or heartbeat limit (`recursion_limit`, `heartbeat_limit`), or crashed Lean.
- `CapacityError`: no worker was free, the server is unavailable, the job
  was lost (`worker_lost`; see [When a worker is lost](#worker-lost)), or a
  gateway answered 502, 503 or 504, once the client's retries ran out;
  retry later.
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
// Throws a PisaError (kind "lean") if the tactic fails, with Lean's errors.
const step = await client.applyTactic(session.states[0]!.id, "simp");
```

React (`@aqora/pisa-react`, in `web/packages/react`) has hooks over TanStack
Query: `useCheck`, `useLiveCheck` and `useCheckBatch` for checks,
`useProofSession` for a tree of tactics, `useDocument` with
`useDocumentGoals` and `useProbeDocument` for open documents, the snapshot
queries and mutations with `useDependents` and `useAudit`, `useLongJob` for
requests run as jobs, `useRebuildPlan`, `useRebuildSnapshots`,
`useAcceptRun` and `useExportRun` for runs, and `useSearch`, `useResolve`
and `useDeclaration`. The packages' READMEs have examples.

The playground (`web/apps/playground`) has a tab for each part of the API:

- **Check**: code checked as you type, or with Ctrl/⌘-Enter, and the goals
  at the cursor. Declarations in full, a timeout, positions pinned for their
  goals and terms, a check run as a job, and a batch of code split at
  `-- ---` lines.
- **Prove**: a session's tree of tactics from each `sorry`, candidates tried
  at a `sorry`, and the finished proof checked against the session.
- **Documents**: code kept open, saved a revision at a time, with its
  sorries, the goals at the cursor or at lines, and candidates tried at a
  `sorry` or at the cursor.
- **Snapshots**: creates that may replace a snapshot, fork a run or leave
  holes; the list, or its heads; and a snapshot with its link, holes, stored
  check and dependents, to verify, replace or delete.
- **Runs**: audits of a project by its labels or of a run against its spec,
  the export and acceptance, and rebuilds planned and run.
- **Search**: several fields at once, a declaration's details, and names
  resolved, when the server searches.

Prove and Documents need a worker with a Lean Beam slot (`PISA_BEAM_SLOTS`).
Links carry code and ids between tabs: a check's code to keep or prove, a
snapshot's header to build on, a hit's imports to try, and any snapshot id
to open. The version box applies to every request once it settles, except
those about a snapshot, document or session, whose id carries its version.

`vite dev` proxies `/api` to `PISA_URL`, by default the compose stack's
`http://localhost:8000`:

```sh
cd web && pnpm install
pnpm dev                                  # http://localhost:5173
PISA_URL=http://localhost:8001 pnpm dev   # another server
```

A Playwright walk through every tab (`web/apps/playground/e2e`) drives the
dev server, which it starts, against a stack whose data doesn't matter, and
deletes the snapshots it made. Building a second compose project retags the
`pisa-server` and `pisa-worker:$PROJECT` images, so give it other `image:`
names in an override file when another stack runs from them:

```sh
export COMPOSE_PROJECT_NAME=pisa-e2e PROJECT=lean-core PISA_PORT=9100
docker compose up -d --build
PISA_URL=http://localhost:9100 pnpm -C web --filter playground e2e
docker compose down -v
```

With `COMPOSE_PROJECT_NAME` set, it stops the stack's workers for a moment to
withdraw a job no worker has taken. `PLAYWRIGHT_CHROMIUM` names a Chromium to
run instead of Playwright's own. Screenshots land in `e2e/out`.

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

Search finds Mathlib, Physlib and CSLib declarations from a description,
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

**How it ranks.** Each of five lanes is a Qdrant query, and Qdrant fuses them
with weighted reciprocal rank fusion:
- **lexical:** BM25 over names, descriptions, signatures and docstrings, as a
  sparse vector with a token per field and term;
- **name:** the name's trigrams, plus an exact match, which always comes first;
- **structural:** constants the query names or implies, matched against each
  type's constants. A Lean word counts only when it names a declaration, so a
  goal's hypothesis names and variables don't; `consts` names constants
  exactly. LaTeX symbols, notation and quantum-information words (`|ψ⟩`,
  `ᴴ`, partial trace, fidelity) map to constants through `search/symbols.toml`, which can also carry a hint that comes back in the result's `hints` when its words are in the query, e.g. how to compare logarithms of different bases;
- **conclusion:** a goal's target, after its `⊢`, or a `pattern`, against what
  each theorem concludes: the constants of its conclusion's
  discrimination-tree key, which an index of format 5 or later holds. A
  collection loaded from an earlier bundle has no room for it: drop it and
  load the new bundle. Its default weight is 0, so it isn't asked: on the
  physics sets it lowered MRR, even for premises that conclude like their
  goal (see `search/eval/v4.36.0-rc2-1e541805-physics.md`);
- **dense:** Qwen3-Embedding-8B vectors for every declaration, from Scaleway's
  serverless API, which also embeds each query. Qwen3-Embedding-0.6B is a
  free, local alternative (TEI on CPU, or llama.cpp on a GPU) that retrieves
  somewhat worse; see `search/bench/2026-10-22/report.md`. A declaration is
  embedded as its kind and name, its description (or docstring) and its
  signature, in at most 2,000 characters: the description is cut so that the
  signature fits.

Qdrant's weights scale a lane's ranks rather than its scores: a weight below 1
makes a lane count only for its first few hits. The tuned defaults (`Weights`
in `crates/pisa-search/src/engine.rs`, tuned on the physics sets' dev splits
with MathlibQR as a guard) lean on the dense lane, with lexical and
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
- It asks the same model, as a judge, which hypotheses a description leaves
  out, showing it the signature and the type with full names: premises such
  as `(hb : 1 < b)` or `n ≤ m →`, and typeclass assumptions that restrict
  the statement, such as finiteness, an order or nontriviality, but not one
  that only gives a type the structure the description names. One that
  leaves out `1 < b` claims more than the theorem proves. A declaration is
  judged if it has anything like a hypothesis: an instance argument, an
  argument whose name starts with `h` or whose type has a relation or a
  connective, or, for a theorem, an arrow in its type; variables alone don't
  count. A description that leaves some out is asked for again, once, with
  them named, and replaces the first only if the judge passes it. Judging
  adds a call or two per such declaration; the run's summary counts them.
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
gh release download index-v4.36.0-54500a67 -R aqora-io/pisa -p bundle.tar.zst -O - \
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
LIBS=Mathlib,Physlib,PhyslibAlpha,Batteries,Init,Std,QuantumInfo,Cslib
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

**Validating descriptions.** LeanSearch v2's descriptions, and those written
before the judge, can leave hypotheses out. `pisa-search validate-descriptions`
judges stored descriptions as `informalize` judges new ones, describes again
those that leave hypotheses out, and writes the replacements the judge passes
as a new batch, which wins over the old descriptions; rebuild and load the
bundle after it. It prints a line per description judged, and the judge calls
and cost. `--source`, `--names` (names, or files of one name per line) and
`--flagged` (descriptions readers flagged, from the database at
`--database-url`) narrow what it judges, and `--dry-run` lists it:

```sh
pisa-search validate-descriptions --corpus search/corpus --informal search/store \
  --source lsv2-v4.28.0-rc1 --names Real.logb_le_logb_of_le,Real.logb_lt_logb
pisa-search validate-descriptions --corpus search/corpus --informal search/store \
  --flagged --database-url "$PISA_DATABASE_URL"
```

**API** (types in `crates/pisa-protocol/src/search.rs`):

| Endpoint | Request → response |
| --- | --- |
| `POST /search` | `{queries: [{text?, description?, latex?, pattern?, goal?, name?, consts?, libraries?, kinds?, limit?, include_sorried?, snapshots?, format?}], version?}` → `{version, results: [{hits, text?, warnings?, hints?, dense_pending?}]}`, at most 64 queries |
| `POST /resolve` | `{names, snapshots?, version?}` → `{version, results: [{name, exists, deprecated_to?, imports?, suggestions}]}`; `snapshots` as a query's, default `"none"` |
| `GET /declarations/{name}` | `?module=&version=` → `{version, declaration}`: signature, value, description, flags, dependencies and users; with a snapshot hit's `module`, that snapshot's declaration, else the bundle's |
| `POST /declarations/{name}/flags` | `?module=&version=`, `{reason}` → `{version, description_flags}`: flags the declaration's description as wrong, e.g. for leaving out a hypothesis; a 404 for a declaration without one |

Each hit carries:
- name, kind, library, module and signature;
- for a definition, abbreviation or opaque, its `value`: what it is defined
  as, with notation, past the parameters the signature names, cut at 1,000
  characters (` …`). Recursive definitions have none, since Lean keeps only
  their compiled form, and a snapshot's is as its build printed it;
- a description's `title` and `summary`, when one exists: whole sentences, two
  at most within 240 characters. `description_source` says where they come
  from: `lsv2-v4.28.0-rc1` for LeanSearch v2's, the model that wrote it, or
  `docstring` when the summary is the docstring's. A description is a search
  aid; the signature says what holds;
- flags: `uses_sorry`, `informal`, `deprecated_to`, `additive_sibling`, and
  `description_flags`, how many times readers flagged the description as it
  is now (one written again starts with none), which `text` mentions too;
- a score, comparable only within one query, and a source link;
- `text`, about 150 tokens ready for a model's context: the signature (past
  400 characters, binders go from the middle so that the conclusion stays),
  the start of the value, and the summary;
- for a snapshot declaration, `snapshot_ids` (the scope's heads on its
  module), `status` and `verified` (see [below](#snapshot-search)).

With `format: "text"`, a result has no `hits`, only `text`: each hit's `text`,
separated by blank lines.

A flag's `reason` is 1 to 2,000 characters. Flags are kept in the database the
servers share, each on the description it flagged, by a hash of its text, and
`validate-descriptions --flagged` (above) judges the flagged ones. A flag
ignores `Pisa-Scope`: it is shared across scopes and counted for every client.
Each request adds one, and nothing records who made it. Pisa has no
authentication, so anyone who can reach the search service can flag; review a
`--flagged` run's targets with `--dry-run` before paying for it.

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
false; `/health` waits for Qdrant for 2 seconds, while it reads the database,
so it still answers within about 2 seconds. The server stays ready, as the
rest of its API works and every replica shares the one Qdrant.

```python
from pisa_client import Client

client = Client("http://localhost:8000")
for hit in client.search(description="trace of a density matrix is one", libraries=["Physlib"])["hits"]:
    print(hit["text"])
client.resolve(["Real.sqrt_mul_sefl"])[0]["suggestions"][0]["name"]  # "Real.sqrt_mul_self"
client.flag_description("Real.logb_le_logb_of_le", "leaves out `1 < b`")  # the flags on it now
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
  bundle only unless it is sent `snapshots` too, as a query is: then a name
  of a snapshot it reaches in the scope exists, with `imports` the header
  that loads its module, and suggestions come from those snapshots as well.
  `/declarations/{name}` without `module` covers the bundle only.
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
  embedder answer. A module-system snapshot's declarations are exported as
  its module declares them, theorems as theorems and definitions with their
  values and docstrings, though importers see a theorem as an axiom and an
  unexposed definition without its value.
- **Descriptions.** With `PISA_SEARCH_DESCRIBE_MODEL` set to a Claude model
  (with `ANTHROPIC_API_KEY`) or an OpenAI one (with `OPENAI_API_KEY`), the
  leader describes every snapshot declaration as `informalize` describes the
  libraries, judge included.
  So all snapshot code goes to that provider (see [what is
  sent](#what-snapshot-search-sends)), at about $0.0003 a declaration
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

<a id="what-snapshot-search-sends"></a>
**What snapshot search sends to a model.** When `PISA_SEARCH_DESCRIBE_MODEL` is
set, the search replica that leads the index sends each snapshot declaration
to that model, so code that a client submitted leaves your deployment. With it
unset, nothing goes to a model. `GET /health` says which: `search.describe_model`
is the model, or `null` if no snapshot code is sent to one (also when
`PISA_SEARCH_NO_SNAPSHOTS` is set).
- **Who receives it.** A model name starting with `claude-` goes to Anthropic
  (`ANTHROPIC_API_KEY`, or `ANTHROPIC_BASE_URL` for another endpoint); any
  other name goes to OpenAI (`OPENAI_API_KEY`, or `OPENAI_BASE_URL`). The
  provider's own retention terms apply.
- **Per declaration**, one request describes it, with: its Lean name, kind and
  module name; its signature as Lean prints it (the name, arguments and type,
  not the proof or the value of a definition); its docstring, up to 2,000
  characters; and, for at most 30 declarations it uses outside the Init, Std
  and Lean libraries (including, for a definition, those its value uses), the name and the description, or if that isn't written
  yet the signature (300 characters) and docstring (200 characters). Those it
  uses can be Mathlib or Physlib declarations, or other declarations of the
  same snapshot or of the snapshots it imports.
- **Judging.** A declaration with a hypothesis, such as an assumption, a
  premise or a typeclass argument, is sent again, to the same model, with its
  signature, its type printed with full names, and the description just
  written, to check that the description states the hypothesis. If it leaves
  one out, the declaration is described once more.
- **Not sent.** The text of proofs and of definition values (only the names of
  what a definition's value uses go out, as above), a module's `code` as a whole,
  its imports, comments, labels, scopes, the snapshot's id, and who created it.
- **Instructions.** Each request also carries the fixed instructions and
  glossary of `informalize.rs`.
- **Another provider.** The embedder (`PISA_SEARCH_TEI`, Scaleway by default)
  gets each snapshot declaration's signature, docstring and description,
  whether or not a describe model is set, while snapshots are searched; it is
  a separate setting.
- **Turning it off.** Leave `PISA_SEARCH_DESCRIBE_MODEL` empty (compose
  `PISA_SEARCH_DESCRIBE_MODEL`, the chart's `search.describeModel`), or set
  `PISA_SEARCH_NO_SNAPSHOTS`; restart the servers, and check `/health`.
  If you only emptied the model, descriptions already written stay in the
  database and keep being searched; with `PISA_SEARCH_NO_SNAPSHOTS`, snapshots
  aren't searched at all.

**Quality.** These reports track it:
- `search/eval/<env_id>-qdrant.md`: recall, MRR and nDCG on LeanSearch v2's
  MathlibQR benchmark, per lane and per query style, with
  `<env_id>-before-qdrant.md` for the in-memory engine it replaced. Run it with
  `pisa-search eval --qdrant URL --env-id ENV --data MathlibQR.json`; several
  `--weights` are compared in one run;
- `search/eval/<env_id>-physics.md`: the same on three physics sets in
  `search/eval/sets/`, each split in a `dev` half to tune on and a `test` half
  to report on (`--set FILE`, repeatable, and `--split dev|test`):
  - `physics-synthetic`: 300 declarations from each of Physlib, QuantumInfo and
    PhyslibAlpha, asked with the searches the describer wrote for them, in
    words and as a formula. The same call wrote the indexed description, so
    these score high and favour the lexical lane;
  - `physics-premises`: 300 theorems from each, their statement sent as a
    `goal`, with the theorems their proof uses as the answers (core logic and
    instances left out, and the theorem itself left out of the hits);
  - `physics-papers`: statements as papers state them, from a set of 50 drawn
    from alphaXiv papers, the Palomar registry's physics entries, and statements
    written for areas the libraries cover. Each answer was chosen from the
    corpus by hand; a statement with no counterpart in the libraries is listed,
    not scored.

  The first two are written by `pisa-search eval-sets --corpus DIR --informal
  STORE --out search/eval/sets`, the same for the same seed; R@k counts the
  share of a query's answers in the first k hits;
- `search/pilot/2026-10-22/report.md`: the informalization pilot, with cost,
  a full-run estimate, check pass rates and samples to review.

**Imports.** Most of the index loads with `import Mathlib` and `import Physlib`.
Mathlib, Physlib, QuantumInfo (density matrices, entropies, channels),
PhyslibAlpha (Physlib's lightly reviewed, often AI-written contributions) and
CSLib (automata, computability, semantics, algorithms) import together, so the
index is exported from one environment. A QuantumInfo, PhyslibAlpha or CSLib
hit carries the header that loads it, one import a line, in `flags.imports`
(`import Mathlib`, `import Physlib`, `import QuantumInfo`; `import Mathlib`,
`import Cslib` for CSLib) and in its `text`, and `/resolve` gives it for a name
and each suggestion.
PhyslibAlpha is searched by default; `libraries` narrows a search to the
reviewed ones. An index exported before this, from three environments, has
`import Physlib; import QuantumInfo` there, until it is exported again.

## Verification policy

A check is `ok` only when all of these hold:

- The code elaborated without errors.
- Every new declaration depends only on `propext`, `Classical.choice` and
  `Quot.sound`. This rejects `sorry`, `admit`, `native_decide` and new axioms.
- The code contains no metaprogramming: `#eval`, `#eval!` and `#guard`
  (which evaluates its term as `#eval` does), `run_cmd`, `elab`,
  elaborator attributes, `unsafe`, `implemented_by`, simprocs, attributes
  that install code to run later (`@[init]`, `@[tactic]`, delaborators,
  `norm_num` and `positivity` extensions) and similar. Attributes are refused
  where they are applied, in `@[…]` and `attribute […]`, so `extern` or
  `init` can still name a variable, and a name in `«»` is never a command.
  A command's word is refused even as a field, as in `(c).initialize`: when
  Lean recovers from a parse error before it, it runs the command there.
  Write such a name `«initialize»`, as the refusal's hint says.
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
"Unknown identifier `d`" here, with Lean's note that the option would have
bound `d`. When code that doesn't set the option has such an error (or an
unknown universe level), a check's or a snapshot build's `messages` add an
`info` hint at it, naming the option. An unknown name the option wouldn't
bind, such as a misspelt lemma in a proof, gets no hint.

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
or nothing. <a id="canonical-form"></a>The canonical form reads code as the
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
- an instance found by another path after new instances or imports, such as
  a helper's `import Mathlib` that the spec doesn't have: the refusal's
  `was_explicit` and `now_explicit` show the instance arguments (see
  [Replacing](#replacing)), and a spec avoids it by importing what its proofs
  will (see [Running agents on snapshots](#agent-runs));
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
in a separate clean process (`pisa verify`, built from `docker/pisa-lean`):
it re-adds the module's own declarations to an environment of just its
dependencies, with the kernel checking each one, and collects their axioms by
walking the re-checked constants. So a declaration added during the
build with the kernel check skipped is caught even though no forbidden keyword
appears in the source. Because that verdict doesn't depend on the build being
honest, snapshot creation allows the **macro family** (`macro`, `macro_rules`,
`syntax`, `declare_syntax_cat`) — pure syntax transformers whose only risk is
what they expand into, which the re-check covers. It runs by default, and
`verify: false` opts out, but for module-system code, whatever `verify` says:
its build exports its theorems as axioms, so only the re-check can describe
it. `/check` and
`/sessions`, which don't produce a module to re-check, keep the scan as-is.

## Limits

- The REPL never frees environments, so workers replace each process after
  `PISA_MAX_USES` jobs or at a memory high-water mark.
- A session doesn't write a finished tactic script back into the `sorry`.
  Clients submit the assembled proof to `/sessions/{id}/check`.
- A state that another worker continues is replayed from its `sorry`, so
  that step takes as long as the tactics before it, and the reopening.
- Not built yet: auth and per-tenant fairness, `/jobs` async batches, and
  human workspaces (stage 4).
- A check that runs past its time limit reports no declarations, and doesn't
  say which one Lean was elaborating: bisect with smaller checks.
- An acceptance judges its run's pairs one after another, on one worker
  slot, and each of the six kernels replays every constant a pair's theorems
  reach, the library's included: on a large library, minutes per pair.
- Search serves one version, `PISA_SEARCH_ENV_ID`; on another pool,
  `search`, `resolve` and `/declarations` answer `not_found`.
