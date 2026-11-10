-- Open documents (`/documents`), by version and id, so that another worker
-- can reopen one when the worker holding it open is gone. `code` is the
-- whole text, `revision` counts its edits from 1, and `updated` is when it
-- was last used (seconds since the Unix epoch), for the sweep.
CREATE TABLE documents (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    code TEXT NOT NULL,
    revision BIGINT NOT NULL,
    -- The worker that has it open.
    holder TEXT,
    updated BIGINT NOT NULL,
    PRIMARY KEY (version, id)
);
CREATE INDEX documents_updated ON documents (version, updated);

-- The document a job works on, which only workers with Beam slots claim.
ALTER TABLE jobs ADD COLUMN document TEXT;

-- How many Lean Beam processes a worker runs for documents.
ALTER TABLE workers ADD COLUMN beam_slots INTEGER NOT NULL DEFAULT 0;
