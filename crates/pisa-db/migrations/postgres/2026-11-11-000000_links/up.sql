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

-- Each run from its root, the entry its first link names, down its links.
WITH RECURSIVE run (version, entry, root) AS (
    SELECT e.version, e.id, e.replaces FROM entries e
    WHERE e.replaces IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM entries p
        WHERE p.version = e.version AND p.id = e.replaces AND p.replaces IS NOT NULL)
    UNION ALL
    SELECT c.version, c.id, run.root FROM entries c
    JOIN run ON c.version = run.version AND c.replaces = run.entry
)
INSERT INTO links (version, entry, replaces, allow, replacement, run_root)
SELECT e.version, e.id, e.replaces, e.replaces_allow, COALESCE(e.replacement, '{}'), run.root
FROM entries e JOIN run ON run.version = e.version AND run.entry = e.id;

DROP INDEX entries_replaces;
ALTER TABLE entries DROP COLUMN replaces;
ALTER TABLE entries DROP COLUMN replaces_allow;
ALTER TABLE entries DROP COLUMN replacement;
