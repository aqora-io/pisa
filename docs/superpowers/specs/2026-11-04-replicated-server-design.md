# One replicated server, with workers fed from a job queue in the database

## Context

Search already runs as stateless replicas over Qdrant. The router can't be replicated: it holds everything in memory.
- the global queue;
- the workers' TCP connections and their in-flight jobs;
- the load counts behind rendezvous hashing;
- the header counts it prewarms from;
- the map from proof state to the worker holding it.

The chart runs exactly one.

**Goal (user's):** one service to run instead of two (one image, one Deployment), replicated for availability, the router part included. Metadata now lives in PostgreSQL (`pisa-db`), which also carries the work: "something like pgmq for communication with the workers".

### Decisions made with the user
- **One `pisa-server`**, in N replicas (2 by default). It serves the check, session and snapshot API and search.
- **Jobs go through a queue table in `pisa-db`, in the style of pgmq, not pgmq itself.**
  - pgmq exists only on PostgreSQL, while tests and single-process runs use SQLite.
  - It has no priorities, and its only filter is an experimental exact-match `conditional` read, so it can't prefer the worker holding a proof state or one with the header warm.
- **KEDA scales workers from PostgreSQL**, with its `postgresql` trigger. Prometheus becomes optional, for dashboards.
- **Accepted costs:**
  - Checks depend on the database, which they didn't before.
  - Each job pays a few database round trips and two notifications.

### Non-goals
- pgmq or another broker.
- Serving metadata-only snapshot jobs (get, list, labels, delete) in the server. It would need S3 credentials there; they still go to workers.
- Rendezvous hashing for cold headers.
- Exactly-once results for a client whose replica died. The job still runs, and its result is swept.

## Design

### The queue (`pisa-db`: `queue.rs`, `queries/jobs.rs`, a second migration)
- **`jobs`**: one row per job.
  - The queue it belongs to, and the pool version that runs it (`NULL` for any).
  - Its priority (0 interactive, 1 batch).
  - The header's key and text, and `affinity`: the worker holding the proof state it continues.
  - The `Job` as JSONB.
  - Its enqueue time, deadline and `max_attempts`.
  - The claiming worker, its claim time and lease.
  - `attempts`, `result` (JSONB), `restarts` (JSONB), and the time it finished.
- **`workers`**: the registry. Each row has the worker's queue, version, slots, warm header keys, `draining` flag, and when it was last seen.
- **`states.holder`**: the worker that produced the state. `put_state` writes it before every tactic returns. A tactic job's `affinity` is read from it when the job is inserted.
- **Queue names.** Jobs and workers carry a queue name (`PISA_QUEUE`, default `pisa`), so deployments, and test clusters, can share one database. Versions already keep the catalogs apart, but a job without a version would otherwise go to anyone's worker.
- **Every statement is portable.**
  - A claim is an optimistic `UPDATE … WHERE (worker IS NULL OR lease lapsed) AND result IS NULL`, so it needs no `SKIP LOCKED`.
  - The other statements are single conditional updates, deletes or inserts, none of them serializable.
- **Wake-ups.**
  - `pisa_jobs` (payload: the queue) when a job is inserted, and `pisa_done` (payload: its id) when it is done.
  - On PostgreSQL these are `pg_notify`. Each process has one `LISTEN` connection, taken out of the pool, and reconnects it when it breaks.
  - On SQLite they are an in-process broadcast.
  - Waiters also poll, so a missed notification only costs latency. Across SQLite processes, polling is all there is.

### Workers claim
- **Startup.** A worker registers. That releases what an earlier process under the same name had claimed. It then prewarms `PISA_PREWARM` plus the most common headers among its version's open jobs, and heartbeats every lease/3, which renews the leases of the jobs it runs.
- **The loop.** While it has a free slot and isn't draining, the worker reads the oldest open candidates (by priority, then id) and the live registry. A pure `pick` chooses what to claim, oldest first. It leaves a job to another worker only:
  - when the job's `affinity` names a live worker with a free slot;
  - or when this worker isn't warm for the job's header, no holder can take it, and a live warm worker with a free slot can.

  Either way, only for `PREFER` (500 ms) after enqueue, so a stale registry can't strand a job.
- **A finished job.** The worker publishes newly warmed headers first, then writes the result along with the REPL restarts since its last result.
- **Draining.** The worker marks itself draining, claims nothing more, finishes what it has, and unregisters.
- **A job that doesn't decode** for this release fails alone, as it did over TCP.

### Servers enqueue and wait
- **`Dispatcher::submit` keeps its signature.** It inserts the job and waits for `pisa_done` or a poll, then takes the result.
  - If the job is still unclaimed at its deadline (`max_wait`), the server gives it up: a 429.
  - If its lease lapsed after its last attempt, it is `WorkerLost`.
  - A client that goes away gives up its unclaimed job.
- **Metrics.** Queue wait, retries, expiries and restarts come from the row. The gauges (queue depth, slots, workers) are read from the database at scrape time, so every replica reports the same values.
- **Sweeping.** Each replica sweeps every 30 s: results nobody took after 10 minutes, open jobs a minute past their deadline, and workers unseen for an hour.
- **Shutdown.** On SIGTERM, a replica stops accepting after a 5 s grace, then waits up to a drain timeout for its in-flight requests.

### One process
- **`pisa-server`** (a new crate) flattens the router's config, a drain timeout and, optionally, search's.
- **Search starts in the background**, retrying Qdrant, and answers 503 until it is ready. `/health` is 200 whenever the API is up and reports search's state.
- **Removed:**
  - `pisa-router`'s binary;
  - the TCP listener and worker protocol (`RouterMessage`, `WorkerMessage`, the framing);
  - `pisa-search serve`.

### Deployment
- **Compose:** one `server` service in two replicas replaces `router` and `search`. Workers no longer reach the server.
- **Helm:**
  - `server.yaml` (Deployment, Service, PDB, optional HPA, topology spread) replaces `router.yaml` and `search.yaml`;
  - `PISA_DATABASE_URL` comes from `metadata.database.urlSecret` for both the server and the workers;
  - the ScaledObject uses a `postgresql` trigger with a `TriggerAuthentication`.

## Verification
- **Queue tests** on SQLite and on `PISA_TEST_DATABASE_URL`:
  - claim races;
  - a lapsed lease is retried, then lost;
  - deadlines and giving up;
  - a completion after a re-claim is refused;
  - the holder;
  - notifications.
- **`pick` unit tests:** warm, holder, versions, priority, the `PREFER` window.
- **`pisa-tests`:**
  - the suite on the queue, with `stage1`'s raw-router tests ported;
  - a new `replicas.rs`: two servers on one database, a server killed mid-request, a session moving between servers, drains, shutdown, search not ready.
- **Compose and minikube:** two servers and two workers; killing a server or a worker mid-job; KEDA scaling from the queue.

## As built
- **Waiting limit.** Jobs carry how long they may wait (`max_wait_micros`),
  not a deadline.
  - An unclaimed job may wait that long from being enqueued, and a lapsed
    claim that long from lapsing, so a worker lost late in a long job still
    leaves a full retry window.
  - The server gives up an unclaimed job with a 429, and a lapsed one with
    `WorkerLost`.
- **No `replica` column.** Every server hears every `pisa_done`, and wakes
  only the request waiting for that id.
- **The registry is read afresh on every claim round.** A cached read sent a
  check to a cold worker in `checks_go_to_the_builder_and_corrupt_modules_are_refused`:
  the builder had published its new header, but the other worker hadn't
  read it yet.
- **Busy counts are computed from live claims**, not stored.
- **A worker restarted under the same name.** Registering lets its
  predecessor's claims lapse, and its heartbeats renew only the claims it
  made since: `register` returns the time, and `heartbeat` takes it.
- **The router side.**
  - `dispatch.rs` keeps its name, and `submit` its signature.
  - `Running` keeps serving when dropped, and stops only on `shutdown`.
  - The router's binary stayed for one commit, taking `--database-url`.
- **Search** is on when `PISA_SEARCH_QDRANT` is set.
  - `PISA_SEARCH_NO_SNAPSHOTS` turns snapshot search off, as the database is
    always there.
  - Without Qdrant, `/v1/*` is a 404 saying the server doesn't search.
- **The chart's `queue`** defaults to the release's name.
- **No metrics.** Prometheus went afterwards, to keep things simple:
  - no `/metrics`, metrics crates or chart integration;
  - no `restarts` column, and no `warm` flag on check outcomes;
  - `/health` reports the live workers instead (`workers`), which the
    scripts and tests wait on.
- **Overhead, measured in Compose.**
  - A job that does almost nothing on the worker (a listing) takes 8 ms at
    the median end to end, against 0.6 ms for `/health`: about seven round
    trips to PostgreSQL and two notifications.
  - A tactic on a held state takes 22 ms, including pickling the new state.
- **Checked end to end** in Compose with lean-core workers:
  - `scripts/e2e.py` through each server;
  - a worker killed mid-job: the job was retried once its lease lapsed, and
    its 28 s job answered in 43 s;
  - a server killed mid-request: the job finished, and the other server
    answered;
  - a server stopped with SIGTERM: it answered its request first.
- **Not checked:** minikube, and KEDA's `postgresql` trigger.
