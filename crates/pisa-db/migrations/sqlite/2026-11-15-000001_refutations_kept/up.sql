-- A refutation outlives the snapshot that made it, as the record that the
-- target is false: the audit marks it gone. `target_hash` is the hash of the
-- target's statement, by which a spec that amends the refuted one, and keeps
-- the statement, is shown the refutation; null in records made before.
-- SQLite drops a foreign key by building the table again.
CREATE TABLE refutations_kept (
    version TEXT NOT NULL,
    entry TEXT NOT NULL,
    spec TEXT NOT NULL,
    target TEXT NOT NULL,
    theorem TEXT NOT NULL,
    target_hash TEXT,
    PRIMARY KEY (version, entry),
    CHECK (entry <> spec)
);
INSERT INTO refutations_kept (version, entry, spec, target, theorem)
    SELECT version, entry, spec, target, theorem FROM refutations;
DROP TABLE refutations;
ALTER TABLE refutations_kept RENAME TO refutations;
CREATE INDEX refutations_spec ON refutations (version, spec);
