# Pisa survives restarts, loses no answer silently, and says what it runs on

## Context

The Erdős campaign (12–13 Nov 2026) lost one result to operations, not mathematics. The brief's "Reliability and operations" section lists six problems, all seen on a local compose deployment: two servers, two workers, Postgres, Garage, Qdrant and Traefik.

1. **Dependencies don't restart after a host reboot.** Postgres, Garage, Qdrant and Traefik stayed stopped (exit 255). The server and workers came back under `restart: on-failure`, but the API refused connections until the dependencies were started by hand.
2. **Running jobs fail when their workers restart.** The Fan acceptance (13,399 s trial) and two snapshot creates ended `unavailable: job failed on 2 workers that disconnected`, and nothing was requeued.
3. **Job answers expire before a lost client returns.** Two `create_snapshot` jobs read 404 "it never was, or it has expired" 3.5 h later, so the job API couldn't say whether they succeeded.
4. **Long-polls overrun their `wait`.** 6 of 3,352 `GET /jobs/{id}?wait=30` took 46 s to 1,115 s, all during heavy load or memory pressure.
5. **Memory caps exceed host RAM.** 2 workers × (2 × 16,000 MB + 8,000 MB) = 80 GB on a 46 GiB host. One check died `out_of_memory`, and an outside process later triggered the kernel OOM killer.
6. **Running images have no provenance.** Neither image carries a revision, so every finding's commit pin rests on build timestamps.

Exploration found the causes, and two more compose problems.

- **(2) Requeuing exists, but not across an outage.** A lapsed job is offered again only while `lease + max_wait > now` (`claimable!`, `crates/pisa-db/src/queries/queue.rs:34-47`; `max_wait` is 120 s in compose). The reboot took Postgres down too and outlasted that window, so the Fan accept closed as lost with an attempt still left (attempts 2 of 3).
- **(3) The m32/m33 creates were recoverable.** `GET /runs/<their names>` says whether each kept a snapshot. Only the docs and the 404 fail to say so.
- **(4) The deadline is checked only between database reads** (`Dispatcher::look`, `crates/pisa-router/src/dispatch.rs:350-389`). After it passes, one more `queue.status()` runs, and nothing on that path has a timeout: no pool recycle timeout, no statement timeout, no HTTP timeout. The 1,089/1,115 s pair was mostly the client process stalling, but a stalled Postgres would hold every poll the same way.
- **(5) The overcommit is only visible per host.** Each worker alone asks for 40 GB, which fits in 46 GB. The server knows each worker's slots, but not its caps or its host.
- **Two compose problems beyond the brief:**
  - **Garage has no data volume**, so `docker compose down` or recreating the container loses every stored module.
  - **Workers have Docker's default 10 s stop timeout**, which cuts their 600 s drain short. A plain `docker compose restart` therefore loses running jobs.

## Goals

- After a host reboot, the stack comes back by itself, and `/health` says whether the database and the module store are reachable.
- A job whose worker is lost is run again when a worker comes back. One that is finally lost says so with its own error kind, and says whether resending it is safe.
- A client that comes back after an answer expired learns that the job existed, what it was, and how it ended. The README says how to recover a lost create or accept.
- `GET /jobs/{id}?wait=N` answers within `N + 5` seconds, whatever Postgres or the host is doing.
- An operator learns at startup, in `/health` and in the server log, when a host's workers can ask for more memory than it has. An `out_of_memory` error names the cap that was hit.
- Every image and every running server and worker says which commit it was built from.

## Non-goals

- Exactly-once execution. A job may still run twice across a lost worker, as today. `resend_safe` says what that costs.
- Metrics or Prometheus.
- Authentication or rate limits.
- Changing Helm's restart behaviour. Kubernetes restarts pods already; Helm only gets the `REVISION` build arg and the worker stop grace where it applies.
- Fixing clients that stall themselves (the 1,089 s pair).
- A host-level memory reservation or a cap on comparator processes outside Pisa.

## Anti-goals (failure even if it works)

- **A dependency hiccup takes every server out of Traefik.** `/health` must stay 200 while not draining. Dependency state is a field, not a status code, so one slow Postgres read can't empty the load balancer.
- **A lost job runs again where that is unsafe without saying so.** Accept and document edits must keep answering `resend_safe: false`.
- **A tombstone leaks across scopes.** An expired job's details are shown only to a request in the scope that made the job, as for a live job.
- **A tombstone becomes a second result store.** It keeps a few fields, never the answer body.

## Constraints

- No backwards compatibility: nothing outside this repo depends on the API yet.
- The API review's naming rules (`docs/superpowers/specs/2026-11-06-api-review-design.md`, "Naming rules").
- Doc comments are the OpenAPI reference. Each wire change regenerates `openapi.json` and the web client and updates the hand-written `_native.pyi`.
- Every schema change ships as Postgres and SQLite migrations, keyed by a unique timestamp prefix. Today's dirs already use `2026-11-17-000000`, `-000001` and `-000002`.
- The server is replicated, and any replica can answer any job.
- A new `ErrorKind` follows the checklist from the limit kinds:
  - `api.rs` `status()`, `retry_after()`, `refusal()` and their tests;
  - the router's `DispatchError` mapping;
  - Python `errors.py` and `_native.pyi`;
  - the README error table;
  - TS `ERROR_KINDS`, `ERROR_STATUS` and the `errors.ts` helpers;
  - the Rust client's retry match.

## Approaches considered

| Problem | Chosen | Rejected and why |
|---|---|---|
| Lost jobs | **Wait for workers, plus a clear kind.** The lost-job window only runs while a worker of the job's queue is alive. A finally lost job answers `worker_lost` with `resend_safe`. | *Clear kind only*: leaves the reboot case losing work. *Longer window only*: still answers an `unavailable` that clients read as "retry anything". |
| Expired answers | **Tombstones plus longer retention for creates and accepts.** A small record kept 30 days, and answers kept 24 h for `create_snapshot` and `accept_run`. | *Tombstones only*: the campaign's gap was 3.5 h. *Docs only*: the 404 would still not say whether the job existed. *UUIDv7 tokens*: an age doesn't prove existence, and they halve the token's randomness. |
| Memory caps | **Per host, via the server.** Workers register caps, host memory and host boot id, and the server sums them per host. | *Per worker only*: wouldn't have warned in the campaign. |
| Garage data | **A named volume.** | Leaving it: one `compose down` loses the catalog's modules. |
| Long-polls | **One hard deadline around the whole look.** | Tuning statement timeouts alone: every database step would still need its own bound, and the client would still see the sum. |
| Provenance | **A build arg, used both as the OCI label and compiled in.** | Reading `.git` at build time: `.dockerignore` excludes it, and jj's working copy is not `HEAD`. |

## Design

### 1. Reboot recovery and dependency health

**Compose (`docker-compose.yml`):**
- Add `restart: unless-stopped` to traefik, garage, postgres and qdrant. Leave the opt-in tools (`search-load`, `tei`) alone.
- Give Garage a named volume `garage-data`, mounted on its meta and data dirs (`docker/garage.toml:2-3`).
- Give Postgres a named volume `postgres-data`. Today it sits on an anonymous volume, which `docker compose down` followed by `up` doesn't reattach, so the catalog and the modules could part ways.
- **Existing data isn't carried over (Julian's decision).** Switching to the named volumes starts from empty stores. The README's *Upgrading* section says so: rebuild the catalog after upgrading.
- Give workers `stop_grace_period` = drain timeout + 30 s (`PISA_DRAIN_TIMEOUT_SECS` is 600 by default), so a stop drains instead of killing.

**`/health`** (`pisa_protocol::api::Health`, `crates/pisa-router/src/http.rs:302-329`) gains:
- `revision`: the server's build (§6).
- `dependencies`:
  - **`database`**: `ok` or `unreachable`, from one bounded `SELECT 1` of at most 2 s.
  - **`store`**: whether live workers could reach the module store at their last report: `ok`, `unreachable`, or `unknown` when no live worker has reported.
    - Workers probe the store at registration and about once a minute (a cheap list or head on the bucket), and record the result in their `workers` row.
    - The server never talks to Garage itself.
  - Search keeps its existing `search.qdrant` field.
- **Status:** `/health` stays 200 unless draining (anti-goal).
- The existing exact-shape test in `crates/pisa-server/tests/server.rs:74-100` changes accordingly.

### 2. Lost jobs

**Queue rule.** A lapsed job stays claimable while attempts remain and its queue has had no live worker for `max_wait` since the lapse.
- Only time during which a worker of the job's queue was live counts toward `max_wait`.
- The exact formula is the builder's. One option is to compare the lapse with the queue's live workers' registration and `seen_micros`.
- An absolute ceiling of 24 h after enqueue still closes the job, so a queue whose workers never return doesn't keep jobs forever.
- **Async jobs only (my call).** The rule applies only to jobs with a token. A sync request keeps today's bound: lease + `max_wait`, then 503 `worker_lost`.
  - Otherwise its client would hold an HTTP connection through Traefik for hours.
  - Its job would also run again after a reboot with nobody waiting, because the server that held the request is gone too.
- The async rule applies in the same places: `claimable!` (keyed on the token), `status()`'s give-up check and the sweeper's give-up, so they can't disagree.

**`ErrorKind::WorkerLost`**, `worker_lost` on the wire:
- Status 503, with `Retry-After`.
- It replaces today's `unavailable` for `DispatchError::WorkerLost`.
- Its message says how many claims the job had, and whether attempts ran out or no worker came back in time.
- A job a server ran itself (rebuild, settle, resubmit) and lost because that server stopped says so. It doesn't claim "N workers disconnected".

**`Error.resend_safe: Option<bool>`**, set only for `worker_lost`. Per job kind:

| Kind | `resend_safe` |
|---|---|
| Checks, sessions and their tactics and checks, verify, export, document open, goals, probes | `true` |
| Creates (replace, amend, refute) | `true`. A resend with the same `key` replays; the clients always send one. |
| Rebuild, settle, resubmit | `true`. Their creates carry the plan's keys. |
| Delete, document close | `true`. A second run answers `not_found`, which means done. |
| Accept | `false`. A finished first run may already be stored; check `GET /snapshots/accept?spec=` first. |
| Document edit | `false`. Check the document's revision first. |

The README gets a "When a worker is lost" section with this table.

### 3. Expired answers

**Tombstones.** A table `expired_jobs`, with Postgres and SQLite migrations. One row per swept or cancelled async job:
- `token`;
- `queue`;
- `kind`;
- the scope labels;
- when it finished;
- its outcome: `done` with the answer's HTTP status, `lost`, or `cancelled`;
- when it was swept.

How it is written and read:
- `sweep_jobs` writes the tombstone and deletes the job row in one transaction. Cancel writes one too.
- Tombstones are swept after 30 days.
- `GET /jobs/{id}` and `DELETE /jobs/{id}` look up a tombstone when the job row is gone. Within the job's scope they answer 404 with a message such as "job {id} expired at {time}: a `create_snapshot` that answered 200; recover it as the README's *Recovering a lost job* says".
- Outside the scope, and for unknown ids, the 404 keeps its generic text without "it has expired".
- The body stays an ordinary error, kind `not_found`. The time and outcome are in the message. Adding structured fields is the builder's call under the naming rules.

**Retention.** Answers of `create_snapshot` and `accept_run` jobs are kept 24 h; other kinds keep 1 h. That is a per-kind filter in the sweep (`queries/queue.rs:419-426`).

**README "Recovering a lost job":**
- **Creates:** resend with the same `key`. Without one, `GET /runs/{name}` says whether a named create kept a snapshot.
- **Accepts:** `GET /snapshots/accept?spec=`, matched on `created`, `against`, `allow` and `paranoid`.
- Plus the retention times.

### 4. Long-polls

`GET /jobs/{id}` bounds its whole look by `wait + 5 s`:
- **Deadline:** use a `tokio::time::timeout` around `Dispatcher::look`, or a deadline passed into it. The builder's choice, so long as nothing after the deadline can block.
- **Last known status:** the look remembers the last status it read.
- **At the deadline:**
  - If it read a status, it answers 202 with that status.
  - If it never read one, it answers 503 with `Retry-After`, as a look failure already does today.
- **Pool:** `pisa-db` sets a recycle timeout on its pool (5 s), so a checkout's `SELECT 1` can't hang. That matters for every caller, not just polls.
- **Docs:** the jobs doc comment and the README's async section state the bound.

### 5. Memory caps

**Registration.** The worker adds these to its `workers` row (new columns, both backends):
- **its caps:** `slots × PISA_MEMORY_MAX_MB + beam_slots × PISA_BEAM_MEMORY_MAX_MB`, or "unlimited" when a cap is 0;
- **the host's memory:** `MemTotal` from `/proc/meminfo`, or the container's own memory limit if that is lower;
- **the host's boot id:** `/proc/sys/kernel/random/boot_id`. It is the same in every container on one host and changes at each boot.

**Worker startup.** The worker warns in its log when its own caps exceed the host's memory.

**Server.**
- `/health` gains `hosts`: one entry per boot id among live workers, with the workers, the summed caps, the host memory, and whether the caps exceed the memory.
- The server logs a warning when a host is overcommitted, once per change rather than every sweep.

**`out_of_memory` errors.**
- Every site that builds one fills `Error.limit` with the setting that was hit and its value:
  - `PISA_MEMORY_MAX_MB` for REPL, build and re-check;
  - `PISA_BEAM_MEMORY_MAX_MB` for Beam;
  - for accepts, a `PairError` rather than an `Error`, with no `limit` field: comparator runs in the slot's cgroup, so its message names `PISA_MEMORY_MAX_MB` and the value. No new wire field (my call).
- `LeanLimit`'s doc says it is now also used for memory. Renaming the type is the builder's call under the naming rules.
- The sites today: `check.rs:309-315`, `pool.rs:328-334`, `documents.rs:320-334`, `snapshots/build.rs:179-182`, `launcher.rs` and `snapshots/verify.rs`, and `runs.rs`.
- The cap needs a getter from `Cgroups` through `Launcher`.

### 6. Provenance

- `docker/Dockerfile` takes `ARG REVISION=unknown` in the build stages. It puts the value in the environment for `cargo build`, which reads it with `option_env!`.
- The image labels it `org.opencontainers.image.revision` on the server and worker images.
- Compose passes `REVISION: ${PISA_REVISION:-unknown}` as a build arg.
- **The README's recipe names the commit actually built (my call).** That is jj's working-copy commit `@` when it has changes, and `@-` when it is empty. With git: `HEAD`, plus `-dirty` when the tree has changes. That way an image built from uncommitted edits can't claim a clean commit.
- The server reports its revision in `/health`.
- Workers register theirs. `/health` lists the distinct worker revisions, so a mixed fleet shows.
- The Helm chart passes the same arg where it builds or documents images.

## Tests

The brief's three new tests are ported to what CI can run.

1. **Restart.** A cluster test:
   1. A job is claimed.
   2. Its worker is killed.
   3. No worker is alive for longer than `max_wait`.
   4. A new worker starts.
   5. The job completes, with attempts 2.

   A second test: past the 24 h ceiling (shortened by config), it answers `worker_lost` with `resend_safe`.

   CI also runs `docker compose config -q` to check the compose file.
2. **Long-poll under stall.** The database is made to stall: on SQLite, hold its connection mutex; on Postgres, hold a lock or pause. `GET /jobs/{id}?wait=1` still answers within `1 + 5 s`, with 202 and the last status, or 503 if none was read.
3. **Expired answer.** With retention shortened by config:
   - an async create's answer is swept;
   - `GET /jobs/{id}` answers the tombstone's 404 in scope, and the generic one out of scope;
   - resending the create with its `key` returns the existing snapshot.

Plus:
- unit tests for the per-host cap sum;
- unit tests for the boot-id and meminfo parsing;
- an `out_of_memory` error carrying `limit`;
- `/health` shape tests.

## Calls made without Julian (check these)

- The lost-job rule covers async jobs only; sync requests keep today's bound.
- An accept's `out_of_memory` names the cap in its message, with no new field.
- The revision recipe names the working-copy commit when it has changes.
- `/health` stays 200 when a dependency is down. Dependency state is a field.
- The 24 h ceiling on a lost job, 30 days for tombstones, 24 h retention for create and accept answers, and the 5 s long-poll margin.

## Left to the builder

- The exact queue formula for "time with a live worker".
- Field names, under the naming rules.
- The test seams for stalling the database.
- The store probe's operation and its period.
- Whether `/health`'s `dependencies.database` reuses the existing count queries or runs its own `SELECT 1`.
