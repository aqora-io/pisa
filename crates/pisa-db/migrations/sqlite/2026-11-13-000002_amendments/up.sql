-- A spec's amendment of an earlier spec: `entry` amends `amends`, for
-- `reason`, and its create found `changes` (JSON of the declaration changes)
-- and `added` (JSON of the names the new spec adds). The record goes with
-- the new spec, and outlives the amended one, as history: `amends` has no
-- foreign key.
CREATE TABLE amendments (
    version TEXT NOT NULL,
    entry TEXT NOT NULL,
    amends TEXT NOT NULL,
    reason TEXT NOT NULL,
    changes TEXT NOT NULL,
    added TEXT NOT NULL,
    PRIMARY KEY (version, entry),
    FOREIGN KEY (version, entry) REFERENCES entries (version, id) ON DELETE CASCADE,
    CHECK (entry <> amends)
);
CREATE INDEX amendments_amends ON amendments (version, amends);
