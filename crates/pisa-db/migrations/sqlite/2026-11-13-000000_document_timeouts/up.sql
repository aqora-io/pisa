-- The time limit in seconds that a document's open or last edit took, which
-- a reopen on another worker, or after an eviction or a restart, takes too.
ALTER TABLE documents ADD COLUMN timeout BIGINT NOT NULL DEFAULT 60;
