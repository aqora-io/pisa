-- The work queue between the servers and the workers. A server inserts a job
-- and waits for its result; a worker claims it, renews its lease while it
-- runs, and writes the result, which the server then deletes.

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
    job JSONB NOT NULL,
    enqueued_micros BIGINT NOT NULL,
    -- How long it may wait for a worker: from being enqueued, or from its
    -- last claim lapsing. Then it is given up.
    max_wait_micros BIGINT NOT NULL,
    max_attempts INTEGER NOT NULL,
    worker TEXT,
    claimed_micros BIGINT,
    -- The claim lapses then, unless the worker renews it.
    lease_micros BIGINT,
    attempts INTEGER NOT NULL DEFAULT 0,
    result JSONB,
    done_micros BIGINT
);
CREATE INDEX jobs_open ON jobs (queue, priority, id) WHERE result IS NULL;
CREATE INDEX jobs_running ON jobs (worker) WHERE result IS NULL;
-- Rows come and go all the time.
ALTER TABLE jobs SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.02);

-- The workers taking jobs from a queue, which pick jobs by what the others
-- have warm and free.
CREATE TABLE workers (
    queue TEXT NOT NULL,
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    slots INTEGER NOT NULL,
    -- Header keys it has imported.
    warm JSONB NOT NULL,
    draining BOOLEAN NOT NULL,
    seen_micros BIGINT NOT NULL,
    PRIMARY KEY (queue, name)
);

-- The worker that produced the state, which may still hold it.
ALTER TABLE states ADD COLUMN holder TEXT;
