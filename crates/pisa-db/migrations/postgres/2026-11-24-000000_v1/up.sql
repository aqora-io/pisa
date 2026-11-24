-- pisa's schema. Every key starts with the pool version where a row
-- belongs to one, so versions share one database.

-- A snapshot module: `meta` keeps its code, its check result and `macros`,
-- `decls` its declarations for the search service (`SnapshotDecls`). The rest
-- is what listings show without the code: its own build and files, its
-- header, status, whether it is verified, and when it was built.
CREATE TABLE modules (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    meta JSONB NOT NULL,
    decls JSONB,
    build TEXT NOT NULL,
    files JSONB NOT NULL,
    header TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('proved', 'conditional', 'sorry', 'rejected')),
    verified BOOLEAN NOT NULL,
    created BIGINT NOT NULL,
    PRIMARY KEY (version, id)
);

-- `module` imports `parent`, which can't be removed while it does;
-- `position` is the import's place in the header.
CREATE TABLE module_imports (
    version TEXT NOT NULL,
    module TEXT NOT NULL,
    parent TEXT NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (version, module, parent),
    FOREIGN KEY (version, module) REFERENCES modules (version, id) ON DELETE CASCADE,
    FOREIGN KEY (version, parent) REFERENCES modules (version, id)
);
CREATE INDEX module_imports_parent ON module_imports (version, parent);

-- A labelled reference to a module, which keeps it.
CREATE TABLE entries (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    module TEXT NOT NULL,
    labels JSONB NOT NULL,
    idempotency_key TEXT,
    created_micros BIGINT NOT NULL,
    -- The definitions a spec leaves for the entries that replace it to change
    -- (a JSON array of names), which those inherit.
    holes TEXT,
    -- The code as its create sent it, where it imported runs by name: the
    -- module's code imports their heads at the time in their place.
    source TEXT,
    -- What its create's imports by run name resolved to (JSON).
    runs TEXT,
    PRIMARY KEY (version, id),
    UNIQUE (version, idempotency_key),
    FOREIGN KEY (version, module) REFERENCES modules (version, id)
);
CREATE INDEX entries_newest ON entries (version, created_micros DESC, id DESC);
CREATE INDEX entries_module ON entries (version, module);
CREATE INDEX entries_labels ON entries USING GIN (labels jsonb_path_ops);

-- What each entry replaces: the link its create made, having checked that it
-- kept every statement of the replaced entry, with what it allowed to change
-- (JSON of `api::Allow`, or NULL for nothing) and the comparison (JSON of
-- `api::Replacement`). `run_root` is the entry the run starts at, which every
-- link of the run names, so that a run is read in one query. A link outlives
-- the entry it replaces, as history: `replaces` has no foreign key.
CREATE TABLE links (
    version TEXT NOT NULL,
    entry TEXT NOT NULL,
    replaces TEXT NOT NULL,
    allow TEXT,
    replacement TEXT NOT NULL,
    run_root TEXT NOT NULL,
    PRIMARY KEY (version, entry),
    FOREIGN KEY (version, entry) REFERENCES entries (version, id) ON DELETE CASCADE,
    CHECK (entry <> replaces)
);
CREATE INDEX links_replaces ON links (version, replaces);
CREATE INDEX links_run ON links (version, run_root);

-- A spec's amendment of another spec: `entry` amends `amends`, for `reason`,
-- and its create found `changes` (JSON of the declaration changes) and `added`
-- (JSON of the names the new spec adds). The record goes with the new spec,
-- and outlives the amended one, as history: `amends` has no foreign key.
CREATE TABLE amendments (
    version TEXT NOT NULL,
    entry TEXT NOT NULL,
    amends TEXT NOT NULL,
    reason TEXT NOT NULL,
    changes TEXT NOT NULL,
    added TEXT NOT NULL,
    -- The amended spec's holes and the new spec's (JSON arrays of names), when
    -- they differ; null otherwise.
    holes_before TEXT,
    holes_after TEXT,
    PRIMARY KEY (version, entry),
    FOREIGN KEY (version, entry) REFERENCES entries (version, id) ON DELETE CASCADE,
    CHECK (entry <> amends)
);
CREATE INDEX amendments_amends ON amendments (version, amends);

-- A snapshot's refutation of a spec's target: `entry`'s theorem `theorem`
-- proves the negation of the statement of `spec`'s theorem `target`, as its
-- create checked. It outlives both snapshots, as the record that the target is
-- false: `entry` and `spec` have no foreign key, and the audit marks either
-- gone. `target_hash` is the hash of the target's statement, by which a spec
-- that amends the refuted one, and keeps the statement, is shown it.
CREATE TABLE refutations (
    version TEXT NOT NULL,
    entry TEXT NOT NULL,
    spec TEXT NOT NULL,
    target TEXT NOT NULL,
    theorem TEXT NOT NULL,
    target_hash TEXT NOT NULL,
    labels TEXT NOT NULL,
    PRIMARY KEY (version, entry),
    CHECK (entry <> spec)
);
CREATE INDEX refutations_spec ON refutations (version, spec);

-- A run's name, unique in its version and scope (the scope's canonical text,
-- '' for none): code may `import PisaRun.<name>`, which imports the run's
-- head. A run is named by its root, the entry its links name as `run_root`,
-- which may be deleted while the run goes on, so `root` has no foreign key. A
-- delete that leaves the run with no entries removes the name.
CREATE TABLE run_names (
    version TEXT NOT NULL,
    scope TEXT NOT NULL,
    name TEXT NOT NULL,
    root TEXT NOT NULL,
    PRIMARY KEY (version, scope, name)
);
CREATE UNIQUE INDEX run_names_root ON run_names (version, root);

-- The search service's description and vector of a module's declaration.
CREATE TABLE described (
    version TEXT NOT NULL,
    module TEXT NOT NULL,
    name TEXT NOT NULL,
    record JSONB NOT NULL,
    PRIMARY KEY (version, module, name),
    FOREIGN KEY (version, module) REFERENCES modules (version, id) ON DELETE CASCADE
);

-- One row per transaction that changes an entry or a module, for the search
-- service to follow.
CREATE TABLE changes (
    id BIGSERIAL PRIMARY KEY,
    version TEXT NOT NULL,
    logged_micros BIGINT NOT NULL,
    entry TEXT,
    module TEXT
);
CREATE INDEX changes_logged ON changes (version, logged_micros, id);

-- Each version's catalog has a random id, which the object store's marker
-- `catalogs/<version>` repeats: a worker sweeps only the files of a store that
-- names its own catalog. `sweep_refused` is why the last sweep of the
-- version's files was refused, and `sweep_refused_micros` since when its
-- sweeps have been; null once one sweeps them.
CREATE TABLE catalogs (
    version TEXT NOT NULL PRIMARY KEY,
    id TEXT NOT NULL,
    sweep_refused TEXT,
    sweep_refused_micros BIGINT
);

-- The results of POST /snapshots/accept, each the AcceptResponse it answered,
-- audit included. Kept as history: no foreign key, as a result outlives the
-- snapshots it judged.
CREATE TABLE acceptances (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    result JSONB NOT NULL,
    labels TEXT NOT NULL,
    PRIMARY KEY (version, id)
);

-- The spec snapshots each acceptance judged, to find a spec's acceptances. A
-- spec is kept as its entry's id, which may be deleted since.
CREATE TABLE acceptance_specs (
    version TEXT NOT NULL,
    spec TEXT NOT NULL,
    acceptance TEXT NOT NULL,
    created_micros BIGINT NOT NULL,
    PRIMARY KEY (version, spec, acceptance),
    FOREIGN KEY (version, acceptance) REFERENCES acceptances (version, id) ON DELETE CASCADE
);
CREATE INDEX acceptance_specs_newest ON acceptance_specs (version, spec, created_micros);

-- Named leases, which one process at a time holds until `until_micros`.
CREATE TABLE leases (
    version TEXT NOT NULL,
    name TEXT NOT NULL,
    owner TEXT NOT NULL,
    until_micros BIGINT NOT NULL,
    PRIMARY KEY (version, name)
);

-- Open documents (`/documents`), so that another worker can reopen one when
-- the worker holding it open is gone. `code` is the whole text, `revision`
-- counts its edits from 1, `updated` is when it was last used (seconds since
-- the Unix epoch), for the sweep, and `timeout` the time limit in seconds its
-- open or last edit took, which a reopen takes too.
CREATE TABLE documents (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    code TEXT NOT NULL,
    revision BIGINT NOT NULL,
    -- The worker that has it open.
    holder TEXT,
    updated BIGINT NOT NULL,
    timeout BIGINT NOT NULL,
    labels TEXT NOT NULL,
    -- What its imports by run name resolved to (JSON), which its edits resolve
    -- as.
    runs TEXT,
    PRIMARY KEY (version, id)
);
CREATE INDEX documents_updated ON documents (version, updated);

-- A tactic session, on its document.
CREATE TABLE sessions (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    meta JSONB NOT NULL,
    created BIGINT NOT NULL,
    labels TEXT NOT NULL,
    PRIMARY KEY (version, id)
);
CREATE INDEX sessions_created ON sessions (version, created);

-- A proof state: a session's sorry, or a tactic applied to another state,
-- which a worker replays on Lean Beam.
CREATE TABLE states (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    -- The session's document.
    session TEXT NOT NULL,
    -- A session's first states: the index of their sorry.
    sorry INTEGER,
    -- The others: the state the tactic was applied to, and the tactic.
    parent TEXT,
    tactic TEXT,
    created BIGINT NOT NULL,
    PRIMARY KEY (version, id)
);
CREATE INDEX states_created ON states (version, created);

-- The work queue between the servers and the workers. A server inserts a job
-- and waits for its result; a worker claims it, renews its lease while it
-- runs, and writes the result, which the server then deletes.
--
-- Jobs a client submitted asynchronously it reads back by `token` (which,
-- unlike `id`, can't be guessed), with the endpoint they ran in `kind`; both
-- are NULL for jobs a server waits for itself. An async job's outcome is kept
-- for an hour after `done_micros`. One nobody could run (given up, or lost
-- with its last worker) is closed with a JSON null `result`, not SQL NULL, so
-- it no longer counts as open; `attempts` then says which: 0 is no capacity,
-- more is lost.
CREATE TABLE jobs (
    id BIGSERIAL PRIMARY KEY,
    -- Deployments sharing a database keep their jobs apart by queue.
    queue TEXT NOT NULL,
    -- The pool version that runs it; NULL for any.
    version TEXT,
    -- 0 interactive, 1 batch.
    priority SMALLINT NOT NULL,
    header_key TEXT,
    -- The header's text, for workers to prewarm what is in demand.
    header TEXT,
    -- The worker holding the proof state the job continues.
    affinity TEXT,
    -- The document a job works on, which only workers with Beam slots claim.
    document TEXT,
    job JSONB NOT NULL,
    labels TEXT NOT NULL,
    enqueued_micros BIGINT NOT NULL,
    -- How long it may wait for a worker: from being enqueued, or from its last
    -- claim lapsing. Then it is given up.
    max_wait_micros BIGINT NOT NULL,
    max_attempts INTEGER NOT NULL,
    worker TEXT,
    claimed_micros BIGINT,
    -- The claim lapses then, unless the worker renews it.
    lease_micros BIGINT,
    -- When its client asked a worker running it to stop. It is never claimed
    -- again: its worker, or the sweep once its claim lapses, ends it as
    -- cancelled.
    cancel_micros BIGINT,
    attempts INTEGER NOT NULL DEFAULT 0,
    -- What its attempt last said it was doing (an accept's phase, kernel and
    -- peak memory), for GET /jobs/{id}. A claim clears it.
    progress JSONB,
    result JSONB,
    done_micros BIGINT,
    token TEXT,
    kind TEXT
);
CREATE INDEX jobs_open ON jobs (queue, priority, id) WHERE result IS NULL;
CREATE INDEX jobs_running ON jobs (worker) WHERE result IS NULL;
CREATE UNIQUE INDEX jobs_token ON jobs (token) WHERE token IS NOT NULL;
CREATE INDEX jobs_done ON jobs (queue, done_micros) WHERE done_micros IS NOT NULL;
-- Rows come and go all the time.
ALTER TABLE jobs SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.02);

-- What an async job was and how it ended, kept after its row is swept or
-- cancelled, so that a client who asks later is told it expired, not that it
-- never was. `labels` are the job's, so that only its scope is told. The
-- answer is never kept, only its `http_status`. A tombstone is kept for thirty
-- days after `swept_micros`.
CREATE TABLE expired_jobs (
    token TEXT PRIMARY KEY,
    queue TEXT NOT NULL,
    kind TEXT NOT NULL,
    labels TEXT NOT NULL,
    outcome TEXT NOT NULL,
    http_status INTEGER,
    finished_micros BIGINT NOT NULL,
    swept_micros BIGINT NOT NULL
);
CREATE INDEX expired_jobs_swept ON expired_jobs (swept_micros);

-- The workers taking jobs from a queue, which pick jobs by what the others
-- have warm and free.
CREATE TABLE workers (
    queue TEXT NOT NULL,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    slots INTEGER NOT NULL,
    -- How many Lean Beam processes it runs for documents, and how many
    -- documents it has open on them, so that a document no worker holds opens
    -- where fewest are open per Beam slot.
    beam_slots INTEGER NOT NULL,
    documents_open INTEGER NOT NULL,
    -- Header keys it has imported.
    warm JSONB NOT NULL,
    draining BOOLEAN NOT NULL,
    seen_micros BIGINT NOT NULL,
    -- Since when it has been live without a break: when it registered, or when
    -- it heartbeat again after going quiet for longer than the queue's live
    -- window. A lost async job waits until a worker has been live its max wait
    -- since it lapsed.
    live_since_micros BIGINT NOT NULL,
    -- It runs Lean without memory or process limits (no cgroups).
    unlimited BOOLEAN NOT NULL,
    -- The commit it was built from, `unknown` if its build didn't say.
    revision TEXT NOT NULL,
    -- Whether its last probe of the module store reached it; NULL until it has
    -- probed.
    store_reachable BOOLEAN,
    -- The limits it gives its Lean processes, summed over its slots in MiB
    -- (NULL when it runs any without a limit), the memory of the machine or
    -- container it runs in (0 when it can't tell), and the boot of that machine,
    -- which tells workers on one host from those on others (empty when it can't
    -- tell).
    memory_caps_mb BIGINT,
    host_memory_mb BIGINT NOT NULL,
    boot_id TEXT NOT NULL,
    -- The project it serves: the Lean toolchain it pins, and the Lake packages
    -- its manifest pins as JSON, which `/health` lists by pool (empty for a worker
    -- without a project).
    toolchain TEXT NOT NULL,
    packages TEXT NOT NULL,
    PRIMARY KEY (queue, name)
);

-- The Lake project behind each version a worker has served, which stays after
-- its pool is gone: what a snapshot or a check of that version ran on, and
-- what builds it again.
CREATE TABLE environments (
    version TEXT NOT NULL PRIMARY KEY,
    -- The Lean toolchain it pins, and the Lake packages its manifest pins as
    -- JSON, as on `workers`.
    toolchain TEXT NOT NULL,
    packages TEXT NOT NULL,
    -- Its files by name, as JSON: `lean-toolchain`, the lakefile, the manifest
    -- and the top-level `.lean` files.
    files TEXT NOT NULL,
    created_micros BIGINT NOT NULL
);

-- Readers' flags on search's descriptions of declarations, each on the
-- description as it was, by its hash: one written again starts with none.
CREATE TABLE description_flags (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    description_hash TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_micros BIGINT NOT NULL
);
CREATE INDEX description_flags_name ON description_flags (name, description_hash);
