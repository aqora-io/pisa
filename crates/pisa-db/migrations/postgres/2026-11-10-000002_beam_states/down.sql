DROP TABLE states;
CREATE TABLE states (
    version TEXT NOT NULL,
    hash TEXT NOT NULL,
    meta JSONB NOT NULL,
    created BIGINT NOT NULL,
    holder TEXT,
    PRIMARY KEY (version, hash)
);
CREATE INDEX states_created ON states (version, created);
