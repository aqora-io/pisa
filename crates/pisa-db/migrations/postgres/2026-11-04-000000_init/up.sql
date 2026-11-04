-- Every key starts with the pool version, so versions share one database.

-- A snapshot module: `meta` is its `ModuleMeta`, `decls` its declarations
-- for the search service (`SnapshotDecls`).
CREATE TABLE modules (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    meta JSONB NOT NULL,
    decls JSONB,
    PRIMARY KEY (version, id)
);

-- `module` imports `parent`, which can't be removed while it does.
CREATE TABLE module_imports (
    version TEXT NOT NULL,
    module TEXT NOT NULL,
    parent TEXT NOT NULL,
    PRIMARY KEY (version, module, parent),
    FOREIGN KEY (version, module) REFERENCES modules (version, id) ON DELETE CASCADE,
    FOREIGN KEY (version, parent) REFERENCES modules (version, id)
);
CREATE INDEX module_imports_parent ON module_imports (version, parent);

-- A labelled reference to a module, which keeps it.
CREATE TABLE entries (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    module TEXT NOT NULL,
    labels JSONB NOT NULL,
    idempotency_key TEXT,
    created_micros BIGINT NOT NULL,
    PRIMARY KEY (version, id),
    UNIQUE (version, idempotency_key),
    FOREIGN KEY (version, module) REFERENCES modules (version, id)
);
CREATE INDEX entries_newest ON entries (version, created_micros DESC, id DESC);
CREATE INDEX entries_module ON entries (version, module);
CREATE INDEX entries_labels ON entries USING GIN (labels jsonb_path_ops);

-- The search service's description and vector of a module's declaration.
CREATE TABLE described (
    version TEXT NOT NULL,
    module TEXT NOT NULL,
    name TEXT NOT NULL,
    record JSONB NOT NULL,
    PRIMARY KEY (version, module, name),
    FOREIGN KEY (version, module) REFERENCES modules (version, id) ON DELETE CASCADE
);

-- One row per transaction that changes an entry or a module, for the search
-- service to follow.
CREATE TABLE changes (
    id BIGSERIAL PRIMARY KEY,
    version TEXT NOT NULL,
    logged_micros BIGINT NOT NULL,
    entry TEXT,
    module TEXT
);
CREATE INDEX changes_logged ON changes (version, logged_micros, id);

CREATE TABLE leases (
    version TEXT NOT NULL,
    name TEXT NOT NULL,
    owner TEXT NOT NULL,
    until_micros BIGINT NOT NULL,
    PRIMARY KEY (version, name)
);

-- A proof state's `StateMeta`; its pickle is in object storage.
CREATE TABLE states (
    version TEXT NOT NULL,
    hash TEXT NOT NULL,
    meta JSONB NOT NULL,
    created BIGINT NOT NULL,
    PRIMARY KEY (version, hash)
);
CREATE INDEX states_created ON states (version, created);

CREATE TABLE sessions (
    version TEXT NOT NULL,
    id TEXT NOT NULL,
    meta JSONB NOT NULL,
    created BIGINT NOT NULL,
    PRIMARY KEY (version, id)
);
CREATE INDEX sessions_created ON sessions (version, created);
