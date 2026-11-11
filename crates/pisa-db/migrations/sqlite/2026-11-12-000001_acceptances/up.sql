-- The results of POST /snapshots/accept, each the AcceptResponse it
-- answered, audit included. Kept as history: no foreign key, as a result
-- outlives the snapshots it judged.
CREATE TABLE acceptances (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    created_micros BIGINT NOT NULL,
    result BLOB NOT NULL,
    PRIMARY KEY (version, id)
);
