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
ALTER TABLE run_names ADD COLUMN scope TEXT NOT NULL DEFAULT '';
ALTER TABLE run_names DROP CONSTRAINT run_names_pkey;
ALTER TABLE run_names ADD PRIMARY KEY (version, scope, name);
