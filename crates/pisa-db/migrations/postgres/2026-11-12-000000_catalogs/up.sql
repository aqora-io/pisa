-- Each version's catalog has a random id, which the object store's marker
-- `catalogs/<version>` repeats: a worker sweeps only the files of a store
-- that names its own catalog.
CREATE TABLE catalogs (
    version TEXT NOT NULL PRIMARY KEY,
    id TEXT NOT NULL,
    created_micros BIGINT NOT NULL
);
