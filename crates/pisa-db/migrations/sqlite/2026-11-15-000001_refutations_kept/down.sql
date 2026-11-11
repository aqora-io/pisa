CREATE TABLE refutations_cascading (
    version TEXT NOT NULL,
    entry TEXT NOT NULL,
    spec TEXT NOT NULL,
    target TEXT NOT NULL,
    theorem TEXT NOT NULL,
    PRIMARY KEY (version, entry),
    FOREIGN KEY (version, entry) REFERENCES entries (version, id) ON DELETE CASCADE,
    CHECK (entry <> spec)
);
INSERT INTO refutations_cascading (version, entry, spec, target, theorem)
    SELECT r.version, r.entry, r.spec, r.target, r.theorem FROM refutations r
    WHERE EXISTS (SELECT 1 FROM entries e WHERE e.version = r.version AND e.id = r.entry);
DROP TABLE refutations;
ALTER TABLE refutations_cascading RENAME TO refutations;
CREATE INDEX refutations_spec ON refutations (version, spec);
