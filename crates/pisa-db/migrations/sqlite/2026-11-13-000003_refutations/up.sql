-- A snapshot's refutation of a spec's target: `entry`'s theorem `theorem`
-- proves the negation of the statement of `spec`'s theorem `target`, as its
-- create checked. The record goes with the refuting snapshot, and outlives
-- the spec, as history: `spec` has no foreign key.
CREATE TABLE refutations (
    version TEXT NOT NULL,
    entry TEXT NOT NULL,
    spec TEXT NOT NULL,
    target TEXT NOT NULL,
    theorem TEXT NOT NULL,
    PRIMARY KEY (version, entry),
    FOREIGN KEY (version, entry) REFERENCES entries (version, id) ON DELETE CASCADE,
    CHECK (entry <> spec)
);
CREATE INDEX refutations_spec ON refutations (version, spec);
