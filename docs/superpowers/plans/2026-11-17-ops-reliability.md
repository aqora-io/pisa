# Ops Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pisa comes back by itself after a reboot, runs lost async jobs again when workers return, explains expired and lost jobs, bounds its long-polls, warns about memory overcommit per host, and reports the commit it was built from.

**Architecture:**
- Compose and Dockerfile changes cover restarts, volumes and the revision.
- Workers report more about themselves when they register: revision, store state, memory caps, host memory and boot id.
- `/health` summarizes those reports.
- The queue's give-up rule and the jobs API learn about live workers, tombstones and a hard poll deadline.

**Tech Stack:** Rust (axum, tokio, diesel and diesel-async on Postgres and SQLite, deadpool, utoipa), Docker Compose, TypeScript and Python clients, jj.

**Spec:** `docs/superpowers/specs/2026-11-17-ops-reliability-design.md`

## Global Constraints

- No backwards compatibility. Naming follows `docs/superpowers/specs/2026-11-06-api-review-design.md` "Naming rules": no abbreviations, and `<resource>_id` for references.
- Doc comments are the OpenAPI reference. After any wire change:
  - run `PISA_UPDATE_OPENAPI=1 cargo test -p pisa-server --test openapi`, then `pnpm -C web generate`;
  - keep the hand-written `crates/pisa-python/python/pisa_client/_native.pyi` equal to the wire;
  - update the README section for the slice in the same task;
  - add `#[schema(required = true)]` to every always-sent field.
- Every schema change ships with a Postgres migration and a SQLite migration. Prefixes are assigned per task below; never reuse one.
- A new `ErrorKind` follows the limit kinds' checklist:
  - api.rs `status()`, `retry_after()`, `refusal()` and their tests;
  - the router's `DispatchError` → `ErrorKind` mapping (`crates/pisa-router/src/http.rs:147-154`);
  - Python `errors.py` and `_native.pyi`;
  - the README error table;
  - TS `ERROR_KINDS` (`web/packages/client/src/client.ts`), `ERROR_STATUS` (`types.ts`) and the `errors.ts` helpers;
  - the Rust and TS clients' batch retry match.
- `/health` answers 200 unless the server is draining. Dependency and host state are fields, never a status code.
- Spec values:
  - long-poll margin 5 s;
  - pool recycle timeout 5 s;
  - `/health` database probe bound 2 s;
  - store probe about every 60 s;
  - lost-job ceiling 24 h after enqueue;
  - tombstones kept 30 days;
  - answers of `create_snapshot` and `accept_run` kept 24 h, other kinds 1 h;
  - worker `stop_grace_period` 630 s (`PISA_DRAIN_TIMEOUT_SECS` 600 + 30);
  - image label `org.opencontainers.image.revision`;
  - `ARG REVISION=unknown`;
  - compose build arg `REVISION: ${PISA_REVISION:-unknown}`.
- Build every Rust step with `CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0`. Test against SQLite and Postgres (`PISA_TEST_DATABASE_URL`).
- Commit each task as one jj change: `jj describe -m "<type(scope): …>" && jj new`. Never squash, rebase or push.

## Review Focus

1. **Postgres is down when `/health` is called.** It must answer 200 within about 2 s with `database: unreachable`, not hang on the pool. *Task 3 test.*
2. **A worker restarts under the same `PISA_WORKER_ID` after a reboot.** Its old claims lapse at registration. The lost-job window must still count only live-worker time and run the job again. *Task 4 test.*
3. **A lost sync request.** It must keep today's bound (lease + `max_wait`, then 503) even when no worker is alive, and must not wait for workers. *Task 4 test.*
4. **A host mixing limited and unlimited workers.** An unlimited worker has no cap to add. The host entry marks it, and `overcommitted` is computed from the limited workers' caps alone. *Task 7 test.*
5. **A tombstone asked for from another scope or with no scope.** It answers the generic 404 and never reveals the kind or outcome. *Task 6 test.*

---

### Task 1: Compose restarts, named volumes, and the build's revision

**Files:**
- Modify: `docker-compose.yml`:
  - `restart: unless-stopped` on traefik, garage, postgres, qdrant;
  - volumes `garage-data` (Garage's meta and data dirs per `docker/garage.toml:2-3`) and `postgres-data` (`/var/lib/postgresql/data`);
  - worker `stop_grace_period: 630s`;
  - build args `REVISION: ${PISA_REVISION:-unknown}` for the server and worker builds.
- Modify: `docker/Dockerfile`:
  - `ARG REVISION=unknown` in the `build` and `build-server` stages, exported as `PISA_REVISION` for `cargo build`;
  - `LABEL org.opencontainers.image.revision=$REVISION` in the `server` and `worker` stages.
- Create: `crates/pisa-protocol/src/revision.rs`, exported from `lib.rs`.
- Modify: `crates/pisa-db/src/queue.rs` (`register`), `crates/pisa-db/src/queries/queue.rs`, `crates/pisa-db/src/schema.rs`.
- Create migrations `2026-11-17-000003_worker_revision/{up,down}.sql` under both `crates/pisa-db/migrations/postgres/` and `.../sqlite/`, adding `workers.revision TEXT NOT NULL DEFAULT 'unknown'`.
- Modify: `crates/pisa-worker/src/claims.rs` (registration call), `crates/pisa-protocol/src/api.rs` (`Health`), `crates/pisa-router/src/http.rs` (`App::health`).
- Modify: `.github/workflows/ci.yml`, adding a `docker compose config -q` step in the e2e job.
- Modify: `deploy/helm/pisa/values.yaml` and the chart's README (if it has one): say images should be built with the `REVISION` build arg. The chart builds no images itself.
- Modify: `README.md`:
  - *Upgrading*: switching to the named volumes starts from empty stores; rebuild the catalog;
  - the build section: the revision recipe;
  - the `/health` row.
- Test: `crates/pisa-db/src/queue.rs` tests, `crates/pisa-server/tests/server.rs`, `crates/pisa-tests/tests/client.rs`.

**Interfaces:**
- **Produces:**
  - `pisa_protocol::revision::REVISION: &str`, defined as `option_env!("PISA_REVISION").unwrap_or("unknown")`.
  - `pisa_db::queue::Registration { worker: String, version: String, slots: u32, beam_slots: u32, unlimited: bool, revision: String }`.
  - `Queue::register(&self, r: &Registration)` replaces the positional arguments. Tasks 3 and 7 add fields to `Registration`.
  - `Health.revision: String`, the server's `REVISION`.
  - `Health.worker_revisions: Option<Vec<String>>`: the distinct revisions of the live workers, sorted, or `null` if the queue can't be read.
  - `Queue::live_revisions(live) -> Result<Vec<String>>`.

- [ ] **Step 1: Write the failing tests**
  - `queue.rs`: `a_worker_registers_its_revision`. It registers `Registration { revision: "abc".into(), .. }` and checks that `live_revisions` returns `["abc"]`.
  - `server.rs`: update the exact-shape `/health` test. The body now has `"revision": "unknown"` and `"worker_revisions": []`.
  - `client.rs`: the Rust client's `health()` returns `revision`.
- [ ] **Step 2: Run them and see them fail**
  - Run: `cargo test -p pisa-db a_worker_registers_its_revision` and `cargo test -p pisa-server --test server`.
  - Expected: compile errors or assertion failures.
- [ ] **Step 3: Implement**
  - The migrations, `Registration`, `live_revisions` and the `Health` fields, with doc comments.
  - The worker registers `REVISION`.
  - The compose, Dockerfile, CI and README changes listed above. In the README's revision recipe, use:
    - jj: `PISA_REVISION=$(jj log -r 'if(@ ~ empty(), @, @-)' …)`, or, if that revset doesn't parse, an equivalent that picks `@` when it has changes and `@-` otherwise. Check it runs.
    - git: `$(git rev-parse HEAD)$(git diff --quiet || echo -dirty)`.
- [ ] **Step 4: Verify**
  - Run the tests above, plus:
    - `docker compose config -q`;
    - `docker build --build-arg REVISION=test -f docker/Dockerfile --target server .`, then `docker inspect` showing the label.
  - The image build is optional if disk is short; say so in the report.
  - Regenerate openapi and the web client, and update `_native.pyi`.
  - Run `cargo test --workspace`, the web tests and the Python tests.
- [ ] **Step 5: Commit** — `feat(ops): the stack restarts after a reboot, keeps its data in volumes, and says which commit it runs`

### Task 2: Long-polls answer within their bound

**Files:**
- Modify: `crates/pisa-router/src/dispatch.rs` (`look`), `crates/pisa-router/src/jobs.rs` (the handler and its doc), `crates/pisa-db/src/db.rs` (pool recycle timeout; stall seam).
- Modify: `README.md`, the async section (~461-483).
- Test: `crates/pisa-tests/tests/jobs.rs`, `crates/pisa-db/src/db.rs` tests.

**Interfaces:**
- **Produces:**
  - `pub const POLL_MARGIN: Duration = Duration::from_secs(5)` in `jobs.rs`.
  - `Db::stall(&self) -> StallGuard`: a test seam that holds the SQLite connection, or the pool's slots on Postgres, until dropped.
    - Mark it `#[doc(hidden)]`, behind no feature flag, since the cluster tests live in another crate.
    - Task 3 uses it.
  - `POOL_RECYCLE_TIMEOUT = 5 s`, set as the deadpool `Timeouts.recycle`.

- [ ] **Step 1: Write the failing tests** in `jobs.rs`:
  - `a_wait_answers_within_its_bound_while_the_database_stalls`. A `SLEEP 5000` async job is polled once and reads `running`. The database is then stalled (`db.stall()`) and `GET /jobs/{id}?wait=1` is sent. The answer comes within 1 + 5 s + 1 s slack, as a 202 with `status: "running"`.
  - `a_wait_with_no_status_read_answers_503`. The database is stalled before the first read of a job. The answer is a 503 with `Retry-After`, within 1 + 5 s + 1 s.
- [ ] **Step 2: Run them and see them fail**
  - Run: `cargo test -p pisa-tests --test jobs a_wait_`.
  - Expected: a compile error (no `stall`), then a timeout over the bound.
- [ ] **Step 3: Implement**
  - Bound the whole look by `wait + POLL_MARGIN`.
  - `look` keeps the last status it read, and at the deadline answers it as `Pending(Job)` without another database read.
  - With no status read at all, it answers the existing 503 look failure.
  - Set the pool recycle timeout.
  - Update the doc comment and README to state the bound.
- [ ] **Step 4: Verify** — the new tests, then `cargo test -p pisa-tests --test jobs` and `cargo test -p pisa-db`, both on SQLite and on Postgres. Run the openapi check if the doc changed.
- [ ] **Step 5: Commit** — `fix(jobs): a long-poll answers within its wait and five seconds, whatever the database does`

### Task 3: `/health` reports its dependencies

**Files:**
- Modify: `crates/pisa-protocol/src/api.rs` (`Health`), `crates/pisa-router/src/http.rs` (`App::health`), `crates/pisa-db/src/queue.rs` and `queries/queue.rs`.
- Create migrations `2026-11-17-000004_worker_store/{up,down}.sql`, both backends, adding:
  - `workers.store_reachable BOOLEAN`, nullable;
  - `workers.store_checked_micros BIGINT`, nullable.
- Modify: `crates/pisa-worker/src/claims.rs` (probe at registration and every 60 s), `crates/pisa-worker/src/snapshots/mod.rs` (a cheap probe on the object store, e.g. listing with limit 1 under the store's prefix).
- Modify: `README.md`, the `/health` docs.
- Test: `crates/pisa-server/tests/server.rs`, `crates/pisa-tests/tests/client.rs`, `crates/pisa-db/src/queue.rs` tests.

**Interfaces:**
- **Consumes:** `Registration` (Task 1); `Db::stall` (Task 2).
- **Produces:**
  - `DependencyState`, an enum `{ Ok, Unreachable, Unknown }` serialized snake_case.
  - `Dependencies { database: DependencyState, store: DependencyState }`.
  - `Health.dependencies: Dependencies`, required.
  - `Queue::report_store(worker: &str, reachable: bool)`.
  - `Queue::store_state(live) -> Result<DependencyState>`:
    - `Unreachable` if any live worker's last report is false;
    - `Ok` if all live workers' reports are true;
    - `Unknown` if no live worker has reported.

- [ ] **Step 1: Write the failing tests**
  - `server.rs`: the shape test includes `"dependencies": {"database": "ok", "store": "unknown"}`.
  - `queue.rs`: `the_store_is_as_the_live_workers_last_saw_it`, covering ok, unreachable, and unknown when there are no reports.
  - `client.rs` or `server.rs`: `health_answers_quickly_while_the_database_stalls`. With `db.stall()`, `/health` answers 200 within 3 s with `"database": "unreachable"`.
- [ ] **Step 2: Run them and see them fail.**
- [ ] **Step 3: Implement**
  - The database state: a `SELECT 1` bounded by 2 s, or the existing count queries bounded by 2 s.
  - The other `/health` queries get the same 2 s bound, so a stall can't hold the handler.
  - The worker probe and report.
- [ ] **Step 4: Verify** — the tests on SQLite and Postgres, openapi and the web client regenerated, `_native.pyi`, `cargo test --workspace`, web tests and Python tests.
- [ ] **Step 5: Commit** — `feat(health): it says whether the database and the module store are reachable`

### Task 4: A lost async job waits for a worker to come back

**Files:**
- Modify: `crates/pisa-db/src/queries/queue.rs`:
  - the `claimable!` macro (~34-47);
  - `status()`'s give-up (~282-302);
  - the sweep's give-up (~436-446);
  - `give_up_at` (~136-153).
- Modify: `crates/pisa-db/src/queue.rs` (constants, tests), `README.md` (the queue and retries text ~95-97).
- Test: `crates/pisa-db/src/queue.rs` tests, `crates/pisa-tests/tests/jobs.rs`.

**Interfaces:**
- **Produces:** `pub const LOST_CEILING: Duration = Duration::from_secs(24 * 3600)` in `queue.rs`.
- **The rule, async jobs only** (rows with a `token`):
  - A lapsed job with attempts left stays claimable until its queue has had live workers for `max_wait` since the lapse, or until enqueue + `LOST_CEILING`.
  - The formula is the implementer's choice, e.g. from the queue's live workers' registration times and `seen_micros`.
- **Sync jobs** (no token) keep today's rule exactly.

- [ ] **Step 1: Write the failing tests**
  - `queue.rs`, `an_async_job_lost_while_no_worker_is_live_waits_for_one`:
    1. Claim and lapse a token job.
    2. Advance time past `max_wait` with no live worker.
    3. Check it is still `Queued` and claimable.
    4. Register a worker.
    5. Check it is claimed with attempts 2.
  - `queue.rs`, `an_async_job_is_lost_after_max_wait_with_a_live_worker`: a live worker is registered but never claims the job. Past `max_wait` the job is `GaveUp`.
  - `queue.rs`, `an_async_job_is_lost_at_the_ceiling`: no worker for `LOST_CEILING`, so `GaveUp`.
  - `queue.rs`, `a_sync_job_keeps_its_bound_with_no_worker_live` (Review Focus 3): a lost sync job past lease + `max_wait` reads `Lost`.
  - `jobs.rs`, `an_async_job_runs_again_when_a_worker_comes_back`:
    1. A `SLEEP` async job with `max_wait` 2 s.
    2. Kill its only worker.
    3. Wait 5 s.
    4. Start a new worker. Also restart one with the same `PISA_WORKER_ID` (Review Focus 2).
    5. The job answers 200 done.
- [ ] **Step 2: Run them and see them fail.**
- [ ] **Step 3: Implement** the rule in all four places, so that they can't disagree. Update the README.
- [ ] **Step 4: Verify** — `cargo test -p pisa-db` and `cargo test -p pisa-tests --test jobs`, on SQLite and Postgres, then `cargo test --workspace`.
- [ ] **Step 5: Commit** — `fix(queue): a lost async job waits for a worker to come back before it gives up`

### Task 5: `worker_lost` says what was lost and whether to send it again

**Files:**
- Modify:
  - `crates/pisa-protocol/src/api.rs`: `ErrorKind::WorkerLost` and `Error.resend_safe`;
  - `crates/pisa-protocol/src/wire.rs`: `Job::resend_safe()`;
  - `crates/pisa-router/src/dispatch.rs` (`DispatchError::WorkerLost`, the held-job message ~107-110), `crates/pisa-router/src/http.rs` (the mapping), `crates/pisa-router/src/jobs.rs` (`done()`).
  - Every file on the new-`ErrorKind` checklist in Global Constraints.
  - `README.md`: the error table, and a new section *When a worker is lost* with the spec's per-kind table.
- Test: the api.rs tests, `crates/pisa-tests/tests/jobs.rs`, the web client tests, `crates/pisa-python/tests`.

**Interfaces:**
- **Produces:**
  - `ErrorKind::WorkerLost`: status 503, `retry_after` 2 s, `refusal()` → `Retry`.
  - `Error.resend_safe: Option<bool>`, skipped when none and set only with `WorkerLost`.
  - `Job::resend_safe(&self) -> bool`: false for `SnapshotAccept` and `DocumentEdit`, true otherwise. This follows the spec's table.
  - `DispatchError::WorkerLost { attempts: u32, attempts_exhausted: bool }`. Its message says how many claims there were, and whether attempts ran out or no worker came back in time.
  - `DispatchError::ServerStopped` for held jobs: "the server running it stopped". It maps to `worker_lost` with `resend_safe: true`.

- [ ] **Step 1: Write the failing tests**
  - `api.rs`: a status/refusal table row for `WorkerLost`, and serialization of `resend_safe`.
  - `jobs.rs`: update `a_job_whose_worker_is_lost_says_so`. It now expects `kind: "worker_lost"`, `resend_safe: true` for a check, and a message naming the attempts.
  - `jobs.rs`: add `a_lost_accept_is_not_safe_to_resend`, which expects `resend_safe: false`.
  - The `dispatch.rs` unit test (~513-524) expects `ServerStopped`.
  - Web `client.test.ts`: a 503 `worker_lost` keeps its kind, and the batch retry resends it.
  - Python: `worker_lost` maps to `CapacityError`, keeping `.kind`.
- [ ] **Step 2: Run them and see them fail.**
- [ ] **Step 3: Implement**, following the checklist.
- [ ] **Step 4: Verify** — openapi, web generate, `_native.pyi`, `cargo test --workspace`, web typecheck and tests, Python tests.
- [ ] **Step 5: Commit** — `feat(jobs): a lost job answers worker_lost and says whether sending it again is safe`

### Task 6: Expired jobs leave a tombstone, and creates and accepts are kept a day

**Files:**
- Create migrations `2026-11-17-000005_expired_jobs/{up,down}.sql`, both backends. Table `expired_jobs`:
  - `token TEXT PRIMARY KEY`
  - `queue TEXT NOT NULL`
  - `kind TEXT NOT NULL`
  - `labels`, typed as the `jobs.labels` column is
  - `outcome TEXT NOT NULL`
  - `http_status INTEGER`, nullable
  - `finished_micros BIGINT NOT NULL`
  - `swept_micros BIGINT NOT NULL`
  - an index on `swept_micros`
- Modify: `crates/pisa-db/src/schema.rs`, `crates/pisa-db/src/queries/queue.rs` (`sweep_jobs` ~407-457, `cancel` ~314-338, `status`), `crates/pisa-db/src/queue.rs` (constants, types).
- Modify: `crates/pisa-router/src/jobs.rs` (`unknown()` ~164-167, `GET` and `DELETE`), `crates/pisa-router/src/dispatch.rs` (`Looking`).
- Modify: `README.md`, with a new section *Recovering a lost job*, and the retention text (~475).
- Test: `crates/pisa-db/src/queue.rs` tests, `crates/pisa-tests/tests/jobs.rs`.

**Interfaces:**
- **Consumes:** the job kinds' snake-case names (`JobKind` in api.rs).
- **Produces:**
  - `pub const TOMBSTONE_RETENTION: Duration = 30 days`.
  - `pub fn answer_retention(kind: &str) -> Duration`: 24 h for `create_snapshot` and `accept_run`, `ASYNC_RETENTION` (1 h) otherwise.
  - `Stage::Expired { kind: String, outcome: ExpiredOutcome, http_status: Option<u16>, finished_micros: i64 }`.
  - `ExpiredOutcome { Done, Lost, Cancelled }`.
  - `Looking::Expired(..)`.
  - The 404 text in scope: `"job {id} expired at {RFC 3339 time}: a {kind} that {answered {status} | was lost | was cancelled}; see the README's Recovering a lost job"`.
  - Outside the scope, for an unknown id, or with a scope mismatch, the text stays `"unknown job {id}: it never was, or it has expired"`.
  - Retention, tombstone retention and the sweep are configurable for tests the way the existing retention constants are. If they are plain constants today, add a `QueueConfig` field with these defaults.

- [ ] **Step 1: Write the failing tests**
  - `queue.rs`, `a_swept_job_leaves_a_tombstone`: done, swept, `status()` → `Stage::Expired { outcome: Done, http_status: Some(200) }`.
  - `queue.rs`, `a_cancelled_job_leaves_a_tombstone`.
  - `queue.rs`, `tombstones_go_after_thirty_days`.
  - `queue.rs`, `a_create_is_kept_a_day`, with an `accept_run` too: after 1 h it is still readable, after 24 h it is swept.
  - `jobs.rs`, `an_expired_create_says_so_and_its_key_recovers_it`, with retention shortened:
    1. An async create with `key`.
    2. Sweep.
    3. `GET /jobs/{id}` in the same scope is a 404 whose message has "expired at" and "create_snapshot".
    4. The same GET with another `Pisa-Scope` gets the generic text (Review Focus 5).
    5. Resending the create with the same `key` returns the same snapshot id.
- [ ] **Step 2: Run them and see them fail.**
- [ ] **Step 3: Implement**
  - The tombstone write and the delete in one transaction.
  - The lookup only when the row is gone, with the scope check reusing `token_in_scope`'s rule.
  - Per-kind retention.
  - The README section: resend creates with the same `key`; `GET /runs/{name}` for named creates; `GET /snapshots/accept?spec=` matched on `created`, `against`, `allow` and `paranoid`; and the retention times.
- [ ] **Step 4: Verify** — `cargo test -p pisa-db` and the jobs tests on SQLite and Postgres, then `cargo test --workspace`. Run the openapi check if docs changed.
- [ ] **Step 5: Commit** — `feat(jobs): an expired job says what it was and how it ended, and creates and accepts are kept a day`

### Task 7: Memory caps are checked per host, and `out_of_memory` names the cap

**Files:**
- Create: `crates/pisa-worker/src/host.rs`: `/proc/meminfo`, `/proc/sys/kernel/random/boot_id`, and the container's own `memory.max`.
- Create migrations `2026-11-17-000006_worker_memory/{up,down}.sql`, both backends, adding:
  - `workers.memory_caps_mb BIGINT`, nullable, null when unlimited;
  - `workers.host_memory_mb BIGINT NOT NULL DEFAULT 0`;
  - `workers.boot_id TEXT NOT NULL DEFAULT ''`.
- Modify: `crates/pisa-db/src/queue.rs` and `queries/queue.rs` (`Registration` fields, `hosts`), `crates/pisa-worker/src/claims.rs` and `lib.rs` (registration, the startup warning), `crates/pisa-worker/src/cgroup.rs` and `launcher.rs` (a getter for a slot's cap).
- Modify every `out_of_memory` site:
  - `check.rs` ~309-315;
  - `pool.rs` ~328-334;
  - `documents.rs` ~320-334;
  - `snapshots/build.rs` ~179-182;
  - `launcher.rs` ~117-133, 178-179, together with `snapshots/verify.rs`;
  - `runs.rs` ~293-301, 631-633.
- Modify: `crates/pisa-protocol/src/api.rs` (`Health.hosts`, the `LeanLimit` doc), `crates/pisa-router/src/http.rs` (`App::health`, and the overcommit warning once per change), `README.md` (`/health`, memory settings).
- Test: `host.rs` unit tests, `crates/pisa-db/src/queue.rs` tests, `crates/pisa-server/tests/server.rs`, `crates/pisa-worker` unit tests for the OOM error.

**Interfaces:**
- **Consumes:** `Registration` (Task 1).
- **Produces:**
  - `host::memory_mb() -> Option<u64>`: the minimum of `MemTotal` and the container's `memory.max` when that is a number.
  - `host::boot_id() -> Option<String>`.
  - `parse_meminfo(&str) -> Option<u64>` and `parse_memory_max(&str) -> Option<u64>`, pure, for the tests.
  - `Registration { memory_caps_mb: Option<u64>, host_memory_mb: u64, boot_id: String, .. }`. Here `memory_caps_mb = slots × PISA_MEMORY_MAX_MB + beam_slots × PISA_BEAM_MEMORY_MAX_MB`, or `None` if either cap is 0 while that pool has slots.
  - `HostMemory { boot_id: String, workers: Vec<String>, unlimited_workers: Vec<String>, memory_caps_mb: u64, memory_mb: u64, overcommitted: bool }`.
    - `memory_caps_mb` sums the limited workers only.
    - `overcommitted = memory_caps_mb > memory_mb`.
  - `Health.hosts: Option<Vec<HostMemory>>`, `null` if the queue can't be read.
  - `Queue::hosts(live) -> Result<Vec<HostMemory>>`.
  - `Launcher::memory_max_mb(slot: usize) -> Option<(&'static str, u64)>` returns `("PISA_MEMORY_MAX_MB", n)` or `("PISA_BEAM_MEMORY_MAX_MB", n)`.
  - `Error.limit = Some(LeanLimit { option: <setting>, value: <MiB>, phase: None })` on every `out_of_memory` `Error`.
  - An accept's `PairError` message ends ` (PISA_MEMORY_MAX_MB = {n} MiB)`.

- [ ] **Step 1: Write the failing tests**
  - `host.rs`: `parse_meminfo` reads `MemTotal: 48167364 kB` → 47038; `parse_memory_max` reads `"max\n"` → None and `"17179869184\n"` → 16384.
  - `queue.rs`, `hosts_sum_the_caps_of_workers_on_one_boot`:
    - two workers on boot `b1`, each with caps 40000 and memory 47038 → one host with caps 80000, overcommitted;
    - a third worker on `b2` → its own host;
    - a fourth on `b1` with `memory_caps_mb: None` → listed in `unlimited_workers`, sum unchanged (Review Focus 4).
  - `server.rs`: the shape test includes `"hosts": []`.
  - Worker unit tests: `an_out_of_memory_error_names_its_cap`, at the check site via a helper. The error has `limit.option == "PISA_MEMORY_MAX_MB"` and `limit.value == 16000`. Pick the narrowest seam the code allows, and add one for Beam.
- [ ] **Step 2: Run them and see them fail.**
- [ ] **Step 3: Implement**
  - Registration fields.
  - The worker's startup warning when its own caps exceed `host::memory_mb()`.
  - `hosts`.
  - The server's warning: log once per change of the set of overcommitted hosts, from the sweep loop or the health path.
  - The OOM limit at every site.
  - The README.
- [ ] **Step 4: Verify** — the tests above on SQLite and Postgres, openapi, web generate, `_native.pyi`, `cargo test --workspace`, web and Python tests.
- [ ] **Step 5: Commit** — `feat(health): memory caps are summed per host and an out_of_memory names the cap it hit`

## Final verification (after all tasks)

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` on Postgres
- web typecheck and tests
- the Python tests
- `docker compose config -q`

The live-stack reboot and campaign reproducers need Julian's go-ahead: they restart the shared :9000 stack and start it from empty stores.
