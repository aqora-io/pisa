# Metadata in PostgreSQL instead of TiKV

## Context

The workers' metadata lived in TiKV, behind a transactional KV layer (`pisa-kv`):
- snapshot modules and entries, their imports, idempotency keys and the label index;
- the change log and the leader lease the search service uses;
- search's `described` records;
- proof states and sessions.

The catalog kept its own secondary indexes as keys, and TiKV brought PD, GC-safepoint management and a second gRPC stack.

**Goal (user's):** keep it all in PostgreSQL with diesel for queries and migrations, JSONB for the records, and SQLite for tests and local runs.

### Decisions made with the user
- **Labels** are a `JSONB` column on `entries`, matched with `@>` and served by a GIN index (`jsonb_path_ops`).
- **No migration from TiKV.** Deployments start with an empty database; all TiKV code, dependencies and manifests are gone.
- **diesel-async**:
  - PostgreSQL goes through tokio-postgres, so there is no libpq and the images don't change.
  - SQLite goes through diesel-async's `SyncConnectionWrapper` over a bundled libsqlite3.
  - Migrations run through `diesel_migrations`.
- **`pisa-kv` is renamed `pisa-db`.**
- **TLS** uses rustls when the URL says `sslmode=require`.

### Non-goals
- Data migration from TiKV.
- Deploying PostgreSQL from the chart; it reads the URL from a Secret, as it reads S3 credentials.
- A scan budget on listings, as SQL and the indexes bound the work.

## Design

### `pisa-db`
- **`db.rs`**: `Db` is `Pg(deadpool pool)` or `Sqlite(Arc<Mutex<SyncConnectionWrapper<SqliteConnection>>>)`. The SQLite variant is one connection, which keeps an in-memory database alive and serializes queries as the old in-memory KV did.
- **`Db::connect(url)`**:
  - `postgres://` or `postgresql://` goes to PostgreSQL. Anything else is a SQLite path. `None` or empty is in-memory SQLite.
  - It runs the embedded migrations: `migrations/postgres`, or `migrations/sqlite` with `BLOB` for JSONB and no GIN index.
  - On PostgreSQL that happens in `spawn_blocking` on an `AsyncConnectionWrapper`, under `pg_advisory_lock`, so processes starting together migrate one at a time. `AsyncMigrationHarness` is avoided because it calls `block_in_place`, which panics on current-thread runtimes.
  - SQLite connections get `foreign_keys = ON`, a 250 ms `busy_timeout`, and WAL for files.
- **The pool:** 8 connections per process, with 10 s wait and create timeouts.
- **`DbError::Conflict`** covers serialization failures, unique violations, deadlocks and SQLite's "database is locked". `retry` (unchanged from the KV layer) runs a transaction again on one.
- **Transactions that write:**
  - PostgreSQL: `BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE`.
  - SQLite: `BEGIN IMMEDIATE`.
  - Every transaction reads before it writes, so SSI catches what TiKV's explicit `lock()` did: a removal racing an entry's creation, two removals of a module's last two entries, two creates with one idempotency key.
- **The queries are written once and compiled twice**, in `queries/catalog.rs` and `queries/records.rs`. diesel checks a query against one backend's types, and diesel-async's transaction closures have a private bound, so generic code isn't practical.
  - `backend.rs` includes the files in `mod pg` and `mod sqlite`. Each module gives them `Handle`, `conn`, `begin` and `label_filter`.
  - The backend-specific label filter: PostgreSQL uses `labels @> {…}`, and SQLite uses one bound `json_extract(labels, '$."k"') = v` per label.
  - A `tx!` macro runs a body between `begin` and commit or rollback.
  - `dispatch!` picks the module.
  - Before each use, the SQLite connection rolls back a transaction a dropped future left open.
- **`Db::rows(version)`** lists every row but the change log's, sorted, for tests.

### Tables
All keyed by pool version first:
- `modules` holds `meta` (ModuleMeta) and `decls` (SnapshotDecls).
- `module_imports`: deleting the importing module cascades its rows. A parent can't be deleted while a row imports it.
- `entries`: `labels`, a unique `idempotency_key`, and `created_micros`. Indexes cover newest-first order, the module, and (on PostgreSQL) GIN over labels.
- `described` cascades from `modules`.
- `changes` has an autoincrement id.
- `leases`.
- `states` and `sessions`, each with `meta` and `created`.

### Catalog
- **What stays:**
  - The public API.
  - The serde types.
  - The 6 MiB record limit.
  - The listing cursor's format: 16 hex digits of `u64::MAX - created` plus the entry id.
- **NUL characters:** `to_record` also refuses records with one, which PostgreSQL can't store, as a `bad_request`; `record_fits` checks both rules.
- **`list`:**
  - It dedupes its filters, and a key given two values matches nothing.
  - It returns `next` exactly when a page is full.
  - The cursor decodes with saturation.
- **Search-facing APIs:** those that handed out raw KV keys became typed:
  - `entries_after` and `modules_with_decls_after` page by id;
  - `changes` returns `Logged { id, logged_micros, change }` and pages after `(logged_micros, id)`;
  - `described` and `put_described` use `serde_json::Value`.
- **`update_module`**'s closure is now `Sync`.

### Elsewhere
- **Worker:**
  - `--database-url` / `PISA_DATABASE_URL` replaces `PISA_METADATA` and `PISA_TIKV_PD`.
  - `store.rs` keeps the object-store side and writes records through `pisa_db::records::Records`.
  - The TiKV GC task is gone.
- **Search:**
  - `PISA_DATABASE_URL` replaces `PISA_TIKV_PD`.
  - The indexer pages by id and dedupes changes by id.
  - The page-halving workaround for TiKV's response size is gone.
- **Protocol:** label values and filters with NUL are a `bad_request`.
- **Compose:**
  - A `postgres:18` service with a `pg_isready` healthcheck.
  - Workers and search wait for it to be healthy.
- **Helm:**
  - `metadata.database.urlSecret.{name,key}` (required) replaces `metadata.tikv.pdEndpoints`.
  - Minikube gets `postgres.yaml`.
- **TLS:**
  - With `sslmode=require`, tokio-postgres-rustls checks the server against the native roots, which honour `SSL_CERT_FILE`.
  - Other modes connect without TLS: libpq's `prefer` doesn't verify, and a verifying `prefer` would refuse servers that only `prefer` reaches.
  - Connection errors carry their causes, e.g. why a handshake failed.

## Verification
- **pisa-db unit tests:** they run on in-memory SQLite, and also on the PostgreSQL at `PISA_TEST_DATABASE_URL`, each catalog under a version of its own. Racing tests (`multi_thread`) cover:
  - one key across eight creates;
  - eight keys at once;
  - removals of a module's last two entries;
  - a removal against a new entry;
  - lease contention.
- **The rest of the suite:** the worker, integration (`pisa-tests`) and search tests use `Db::memory()`, or PostgreSQL when `PISA_TEST_DATABASE_URL` is set. The listing tests run unchanged.
- **TLS:** a server certificate signed by a test CA connects with `SSL_CERT_FILE` naming the CA, and fails with `UnknownIssuer` without it.
- **End to end:** `docker compose up --build` with `scripts/e2e.py`, whose session failover needs the shared database.
