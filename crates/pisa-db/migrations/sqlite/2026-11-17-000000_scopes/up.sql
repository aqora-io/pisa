-- A request's scope labels what it creates, and bounds what it reads (see
-- `pisa_protocol::scope`). These resources get its labels.
ALTER TABLE documents ADD COLUMN labels TEXT NOT NULL DEFAULT '{}';
ALTER TABLE sessions ADD COLUMN labels TEXT NOT NULL DEFAULT '{}';
ALTER TABLE jobs ADD COLUMN labels TEXT NOT NULL DEFAULT '{}';
ALTER TABLE acceptances ADD COLUMN labels TEXT NOT NULL DEFAULT '{}';
ALTER TABLE refutations ADD COLUMN labels TEXT NOT NULL DEFAULT '{}';
-- What a create's imports by run name resolved to (JSON), and a document's,
-- which its edits resolve as.
ALTER TABLE entries ADD COLUMN runs TEXT;
ALTER TABLE documents ADD COLUMN runs TEXT;
-- A run name is unique within a scope, its canonical text ('' for none).
-- SQLite changes a primary key by building the table again.
CREATE TABLE run_names_scoped (
    version TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    root TEXT NOT NULL,
    PRIMARY KEY (version, scope, name)
);
INSERT INTO run_names_scoped (version, scope, name, root)
    SELECT version, '', name, root FROM run_names;
DROP TABLE run_names;
ALTER TABLE run_names_scoped RENAME TO run_names;
CREATE UNIQUE INDEX run_names_root ON run_names (version, root);
