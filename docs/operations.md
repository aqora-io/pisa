# Operations

Running pisa in production: how the job queue and its leases behave,
capacity and memory, scaling, draining, what `GET /health` reports,
backups, logs, and security. It is for whoever runs a deployment. The
settings themselves are in [Configuration](configuration.md); the
deployments in [Docker Compose](deploy-compose.md) and
[Kubernetes](deploy-kubernetes.md).

## What runs

- **Servers** (`pisa-server`) serve the API and search. They keep no state:
  each request's job goes on the database's job queue, so any server answers
  any request, and any number of them serve one database.
- **Workers** (`pisa-worker`) take jobs from the queue and run them on Lean.
  Each serves one version, and the workers of a version are its pool (see
  [Environments and pools](environments.md)).
- **PostgreSQL** holds the job queue, the workers' registrations, and the
  records: documents, sessions and their states, snapshots and their
  modules, run names and acceptances.
- **An S3-compatible store** holds the snapshot modules' files. Only workers
  use it.
- **Qdrant**, for search only, holds the index: the loaded bundle's
  declarations and the snapshots'.

## The database

- `PISA_DATABASE_URL` is a PostgreSQL URL, or the path of a SQLite file,
  which only one host can use. Every server and worker of a deployment
  shares it.
- Each process creates or updates the tables when it starts, one process
  at a time.
- Each process opens at most 8 connections, plus one that `LISTEN`s for the
  queue's notifications: allow 9 per server and worker in PostgreSQL's
  `max_connections`, and a few for KEDA.
- Connect directly, or through PgBouncer in session mode, not transaction
  mode: the tables are created under a session-level advisory lock, and
  notifications need a session of their own.
- `sslmode=require` in the URL connects with TLS and checks the server's
  certificate against the system's roots; for a private CA, set
  `SSL_CERT_FILE` to its certificate. Other modes connect without TLS.
- Jobs and workers are kept apart by `PISA_QUEUE` (default `pisa`), which
  the servers and their workers must agree on. Deployments that share a
  database need queues of their own.

## The queue and leases

A server puts each request's job on the queue, a worker claims and runs
it, and the server answers with its result.

- **Leases.** A worker's claim on a job lasts `PISA_LEASE_SECS` (30)
  without a heartbeat, which renews it every third of that. A job whose
  worker died runs elsewhere once its claim lapses, up to
  `PISA_MAX_RETRIES` (2) more times.
- **The lease's trade-off.** A dead worker's job waits up to the lease for
  another worker. A shorter lease hands it on sooner, but also lets a stall
  of a live worker, such as a host short of memory, hand its job to another
  worker while it still runs it.
- **Attempts.** A lapsed claim stays its worker's until another attempt
  claims the job. A claim is its worker's name and the time it claimed, so
  a worker that claims its own job again makes a new attempt, and the old
  one can't answer.
- **Acceptances.** A run's acceptance stops as soon as it sees that its
  claim moved, within a third of the lease, and stores nothing: it stores
  its acceptance only with its answer, in one transaction.
- **A request waiting on its answer** gets a 429 `capacity` if no worker
  claims its job within `PISA_MAX_WAIT_SECS` (60). If its worker is lost,
  it gives another worker `PISA_MAX_WAIT_SECS` after the lapse, then
  answers 503 `worker_lost`.
- **An [async](api.md#async-requests-and-jobs) job** waits longer, as its
  client looks again later:
  - Claimed and lapsed, it waits until a worker that can run it, of its
    version and with Beam slots for a document, has been live for
    `PISA_MAX_WAIT_SECS` since the lapse. So it outlasts a restart of every
    such worker, or of the whole host.
  - Not yet claimed, it waits while a worker that can run it is on record,
    live, busy or restarting: its turn comes when a slot is free, however
    long the jobs ahead of it take. A worker is on record from when it
    registers until an hour after it was last seen, even once it stopped
    cleanly, so a reboot doesn't clear it. With none on record, the job
    gets a 429 after `PISA_MAX_WAIT_SECS`.
  - Either way, it is given up 24 hours after it was queued at the latest.
- **Sweeps.** Every 30 s each server deletes the queue's leftovers: results
  nobody took, after 10 minutes; async jobs' answers, once they have been
  kept as long as [Async requests and jobs](api.md#async-requests-and-jobs)
  says; and jobs and workers nobody has touched for an hour.

How a client sees a lost job, and recovers it: [Lost jobs](api.md#lost-jobs).

## Capacity and memory

- A worker runs `PISA_SLOTS` Lean REPL processes, for checks, snapshots and
  acceptances, and `PISA_BEAM_SLOTS` Lean Beam processes, for documents and
  tactic sessions. The chart requests a CPU for each by default.
- With `PISA_CGROUP_ROOT`, each process has a cgroup of its own:
  `PISA_MEMORY_MAX_MB`, or `PISA_BEAM_MEMORY_MAX_MB` for Beam, is its
  `memory.max`, and `PISA_PIDS_MAX` its `pids.max`. A job that runs out of
  memory answers `out_of_memory`, with the setting and its value in its
  `limit`.
- `PISA_CGROUP_ROOT` must be a delegated cgroup v2 directory. A worker with
  it set that can't set up its cgroups doesn't start. On a node without
  one, unset it (`worker.cgroups: false` in the chart) to run without the
  limits on purpose: the worker logs a warning as it starts, `/health`'s
  `unlimited_workers` names it, and an out-of-memory kill reads as a
  `crash`.
- Without cgroups, only `PISA_MAX_RSS_MB` bounds memory: a REPL process is
  replaced once its resident memory exceeds it. Each REPL process is also
  replaced after `PISA_MAX_USES` (200) jobs, as the REPL never frees
  environments.
- **Overcommit.** A worker's limits add up to `PISA_SLOTS` ×
  `PISA_MEMORY_MAX_MB` + `PISA_BEAM_SLOTS` × `PISA_BEAM_MEMORY_MAX_MB`. It
  registers that sum, the memory of its machine (or its container's limit,
  `/sys/fs/cgroup/memory.max`, if less) and the machine's boot id, and
  workers with one boot id are one host. A host whose workers' limits add
  up to more than it has is `overcommitted` in `/health`'s `hosts`: Lean
  processes there may be killed for memory their limits promised.
  - A worker logs a warning as it starts if its own limits exceed its
    memory, and the servers log one whenever the set of overcommitted hosts
    changes.
  - A host's `uncapped_workers`, those without a memory limit, aren't in
    the sum. A worker that can't read its boot id is in no host, and one
    that can't read its memory has `memory_mb` 0 and isn't judged.
  - Compose's defaults overcommit a machine with less than 80000 MiB; see
    [Memory](deploy-compose.md#memory).
  - The chart sets a worker pod's memory limit to the sum plus
    `worker.overheadMemoryMb`, but requests only `overheadMemoryMb`, so the
    scheduler may place more pods on a node than their limits fit. Set
    `worker.resources` to request more.
  - On Kubernetes, each pod reports its own limit as the host's memory, as
    it is less than the node's. Two or more worker pods on one node then
    read as `overcommitted` whatever the node has: judge a node by its
    memory against `memory_caps_mb` instead.
- **The init.** The worker image's entrypoint is `tini -- pisa-worker`. A
  deployment that sets its own command must keep an init as PID 1: without
  one, the sandboxes the worker kills stay as zombies in their slots'
  cgroups, counting against `pids.max`, until Lean can't start a thread
  there.

## Scaling

- **Servers.** Any replica serves any request: `--scale server=N` in
  Compose; `server.replicas`, or `server.autoscaling` on CPU, in the chart.
  One replica at a time, holding a lease in the database, indexes snapshots
  for search.
- **Workers** scale on their pool's outstanding jobs, queued plus running.
  The chart's KEDA `ScaledObject` counts them in the database every
  `keda.pollingInterval` seconds, with
  `SELECT count(*) FROM jobs WHERE queue = '<queue>' AND result IS NULL`
  narrowed to the pool's version (for `worker`'s pool, also jobs of no
  version). It aims at `keda.jobsPerWorker` jobs per pod, the pool's own
  `slots` by default, and removes pods only after `keda.scaleDownStabilizationSecs` of
  low demand. Removed workers drain.
- **Scaling to zero.** A pool with `minReplicas: 0` runs no pod while it has
  no job. A check or a snapshot request that names its version starts one
  and waits for it, the image's pull included, so keep `router.maxWaitSecs`
  above that. A session or a document doesn't: while no worker of the
  version serves documents, it is a 503 `unavailable` at once.
- **Cold workers.** A new worker imports each header the first time a job
  needs it. `PISA_PREWARM` imports headers, such as `import Mathlib`, into
  idle slots as it starts.

## Draining

- **A worker** that receives SIGTERM claims no more jobs, counts as
  `draining` in `/health`'s `pools`, finishes the jobs it has for at most
  `PISA_DRAIN_TIMEOUT_SECS` (600), and exits. The claims of jobs still
  running then lapse, and those jobs run elsewhere, as a dead worker's do.
  Its grace period must be longer than the drain: the chart's is
  `worker.drainTimeoutSecs` plus 30 s, and Compose's `stop_grace_period`
  630 s.
- **A server** that receives SIGTERM answers `/health` with a 503
  `draining` but keeps taking requests for 5 s, while its readiness probe
  (or Traefik's health check, in Compose) takes it out of rotation. It then
  stops taking connections and waits up to `PISA_DRAIN_TIMEOUT_SECS` (360)
  for the requests in flight. Keep that above `PISA_MAX_WAIT_SECS` plus the
  longest job, or rollouts cut some off; the chart's grace period is
  `server.drainTimeoutSecs` plus 30 s. A request it cuts off still runs, as
  its job is in the queue, and its result is swept.
- Scale-downs and rollouts therefore don't cut jobs off.
- Both exit 0 once drained, so restart them on any exit, not only on
  failure, as Compose's `unless-stopped` does.

## Health

`GET /health` answers 200 while the server is up, whatever the state of
the database, the store or search, and 503 with `status: "draining"` once
it is stopping. Use it as a readiness probe, not a liveness probe: the
chart has none, as it would fail during a drain and kill the server before
it finished. Each read of the database, and Qdrant's health check, waits at
most 2 s, so a stalled dependency delays the answer by about that much.

| Field | What it says |
| --- | --- |
| `status` | `ok`, or `draining` with a 503 |
| `workers` | The live workers on the queue: those seen in the last 30 s |
| `queued` | The jobs waiting for a worker, of every version |
| `pools` | The live workers by version, a pool each: `version`, `workers`, their summed `slots` and `beam_slots`, those `draining`, and the `toolchain` and `packages` their project pins |
| `revision` | The server's commit, `unknown` if its build didn't say |
| `worker_revisions` | The live workers' commits, each once. More than one means a rollout is under way, or stuck. |
| `dependencies` | `database` and `store`, each `ok`, `unreachable` or `unknown`: the database as it answers within 2 s, the store as the live workers last probed it, about every minute (`unreachable` if any can't reach it) |
| `unlimited_workers` | The live workers that run Lean without memory or process limits, by name |
| `hosts` | The live workers' memory limits per host: `boot_id`, `workers`, `uncapped_workers`, `memory_caps_mb`, `memory_mb` and `overcommitted` (see [Capacity and memory](#capacity-and-memory)) |
| `sweeps_refused` | The versions whose sweeps of the store's snapshot files are refused, with `reason` and `since` (see [Sweeps](snapshots.md#sweeps)) |
| `search` | `null` on a server without search; else `ready`, `version`, `qdrant`, `leader`, `describe_model`, the model snapshot code is sent to (`null` for none), and `error`, why the index isn't open yet |

Each field that reads the queue is `null` when it can't be read. The store
is `unknown` until a live worker has probed it, or while the database can't
be read. The full schema is in [GET /health](api.md#get-health).

## Observability

- **Logs.** Each binary logs lines of text: the server and the worker to
  stdout, `pisa-search` to stderr. `RUST_LOG` filters them (default
  `info`), e.g. `RUST_LOG=info,pisa_worker=debug`. Messages worth an alert:
  - from a worker: "PISA_CGROUP_ROOT is unset", "this worker's memory
    limits add up to more than its host has", "bwrap can't make
    comparator's sandbox", "drain timed out with jobs unanswered", and the
    error "not sweeping the store's snapshot files";
  - from a server: "the workers on a host are allowed more memory in total
    than it has", "requests still in flight after", "reading … took more
    than 2s" and "search isn't ready".
- **No metrics endpoint.** pisa exports no Prometheus or other metrics.
  Poll `/health`, and watch for:
  - `status` other than `ok` outside a rollout, or an `unreachable`
    dependency;
  - `workers` at 0, or a version missing from `pools` while requests name
    it;
  - `queued` growing;
  - an `overcommitted` host, or `unlimited_workers` or `sweeps_refused` not
    empty;
  - `worker_revisions` with more than one commit once a rollout is done;
  - `search.ready` false on a server that searches.
- The KEDA query above counts a queue's outstanding jobs, if you want that
  number from the database. An async job's progress, an acceptance's
  phases included, is in `GET /jobs/{id}` (see
  [Async requests and jobs](api.md#async-requests-and-jobs)).

## Backups

- **The database** holds the only copy of the snapshot records: the
  entries, the modules' records with their code and check results, links,
  run names, amendments, refutations, acceptances, and the descriptions
  written for snapshot declarations. None of them expire. Run PostgreSQL
  durably, and back it up with continuous archiving or your provider's
  backups. The jobs it also holds expire, and so do documents, sessions and
  their states, after `PISA_STATE_TTL_SECS` if it is set.
- **The store** holds the modules' built files, under
  `snapshots/<version>/`, and `catalogs/<version>`, the marker that names
  the database's catalog. A record whose files are gone can't be imported:
  code that imports its snapshot is `not_found`, and nothing builds the
  files again. Back up the bucket too, or keep its deleted objects, as S3
  versioning does.
- **Take both, and restore both.** A build uploads its files before it
  writes its record, so a copy of the store taken after the database's has
  the files of every record in it, but for a snapshot deleted in between.
  A restored database keeps its catalog's id, so its workers go on sweeping
  the store: the files of builds made after the backup, which it has no
  record of, go once they are a day old, unless they are most of the
  store's (see [Sweeps](snapshots.md#sweeps)). To start a new database on a
  kept store, see
  [A database reset on a kept store](snapshots.md#a-database-reset-on-a-kept-store).
- **Qdrant** needs no backup. Load the bundle again with `pisa-search load`
  (see [Loading](search-index.md#loading)); the server that leads the
  snapshot index puts the snapshots' declarations back from the database,
  which keeps their descriptions and vectors, as it takes the lead and
  every 6 hours.
- **Compose** keeps its data in named volumes, `postgres-data`,
  `garage-meta`, `garage-data` and `qdrant-data`, which
  `docker compose down -v` deletes.

How long snapshots and modules are kept, and how deletes and sweeps remove
them: [Retention](snapshots.md#retention).

## Security

- **No authentication.** pisa checks no credentials: whoever reaches a
  server can check code, create and delete snapshots, accept runs and read
  every record. Keep the servers on a private network, and put your own
  authentication in front of them.
- **Scopes aren't access control.** A client sends its own `Pisa-Scope`
  (see [Scopes](api.md#scopes)). To keep tenants apart, have your proxy set
  the header from the identity it authenticated, replacing any the client
  sent. Description flags ignore scopes and record nobody's identity.
- **Browsers.** No other origin may call the API from a browser unless
  `PISA_CORS_ORIGINS` names it; `*` allows any.
- **Workers run code that clients send.** Three things contain it:
  - The [verification policy](checking.md#the-verification-policy) refuses
    metaprogramming, such as `#eval`, before running anything, unless
    `PISA_ALLOW_METAPROGRAMMING` is set.
  - `PISA_SANDBOX=bwrap` runs each Lean process with no network, a private
    `/proc` and `/tmp`, and a read-only filesystem. The binary's default is
    `none`; Compose and the chart turn it on. Keep it on wherever clients
    aren't trusted.
  - Cgroups bound each process's memory and processes.
- **Workers run privileged.** bubblewrap creates namespaces and the worker
  writes to its own cgroup subtree, so Compose runs workers `privileged`
  in a private cgroup namespace, and the chart makes worker pods
  `privileged` whenever `worker.sandbox` is `bwrap` or `worker.cgroups` is
  true. A privileged container has nearly the host's privileges: run
  workers on nodes of their own (`worker.nodeSelector`, `tolerations`,
  `affinity`), and limit their network access. The chart offers no tighter
  security context.
- **The store is trusted.** Workers import the store's `.olean` files
  without checking them again; the recorded SHA-256 catches corruption,
  not a malicious write. Give the bucket's credentials to the workers
  alone, as the chart does: the servers don't use it. See
  [Trust](snapshots.md#trust).
- **The database** holds the code of every snapshot and document.
  Keep its URL, with its password, in a Secret, as the chart's
  `metadata.database.urlSecret` does.
- **Code can leave the deployment.** With `PISA_SEARCH_DESCRIBE_MODEL` set,
  the server that leads the snapshot index sends each snapshot
  declaration, its name, signature and docstring but not its proof, to
  that model's provider, Anthropic or OpenAI. The embedder, Scaleway's by
  default in Compose and the chart, receives search queries and, while
  snapshots are searched, their declarations' signatures, docstrings and
  descriptions. `/health`'s `search.describe_model` names the model, or is
  `null` when none gets snapshot code. What goes where:
  [What snapshot search sends to a model](search.md#what-snapshot-search-sends-to-a-model).
- **Keys** go in Secrets: `search.secret`, `search.qdrant.apiKeySecret` and
  `store.s3.credentialsSecret` in the chart.
