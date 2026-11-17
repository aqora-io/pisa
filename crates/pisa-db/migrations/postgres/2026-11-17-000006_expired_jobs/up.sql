-- What an async job was and how it ended, kept after its row is swept or
-- cancelled so that a client who asks later is told it expired, not that it
-- never was. `labels` are the job's, so that only its scope is told. The
-- answer is never kept, only `http_status` of it. A tombstone is kept for
-- thirty days after `swept_micros`.
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
