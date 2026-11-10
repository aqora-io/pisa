-- A proof state was a pickle in the store. It is now a session's sorry, or a
-- tactic applied to another state, which a worker replays on Lean Beam: the
-- pickles' records go, and so do the sessions they belonged to, whose ids
-- change with them.
DROP TABLE states;
CREATE TABLE states (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    -- The session's document.
    session TEXT NOT NULL,
    -- A session's first states: the index of their sorry.
    sorry INTEGER,
    -- The others: the state the tactic was applied to, and the tactic.
    parent TEXT,
    tactic TEXT,
    created BIGINT NOT NULL,
    PRIMARY KEY (version, id)
);
CREATE INDEX states_created ON states (version, created);
DELETE FROM sessions;
