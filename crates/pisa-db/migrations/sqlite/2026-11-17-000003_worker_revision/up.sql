-- The commit a worker was built from, which `unknown` is for one that
-- doesn't say.
ALTER TABLE workers ADD COLUMN revision TEXT NOT NULL DEFAULT 'unknown';
