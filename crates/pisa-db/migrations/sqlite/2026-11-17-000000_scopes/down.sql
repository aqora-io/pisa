CREATE TABLE run_names_unscoped (
    version TEXT NOT NULL,
    name TEXT NOT NULL,
    root TEXT NOT NULL,
    PRIMARY KEY (version, name)
);
INSERT OR IGNORE INTO run_names_unscoped (version, name, root)
    SELECT version, name, root FROM run_names;
DROP TABLE run_names;
ALTER TABLE run_names_unscoped RENAME TO run_names;
CREATE UNIQUE INDEX run_names_root ON run_names (version, root);
ALTER TABLE documents DROP COLUMN runs;
ALTER TABLE entries DROP COLUMN runs;
ALTER TABLE refutations DROP COLUMN labels;
ALTER TABLE acceptances DROP COLUMN labels;
ALTER TABLE jobs DROP COLUMN labels;
ALTER TABLE sessions DROP COLUMN labels;
ALTER TABLE documents DROP COLUMN labels;
