-- Why the last sweep of a version's files in the store was refused, and
-- since when its sweeps have been; null once one sweeps them.
ALTER TABLE catalogs ADD COLUMN sweep_refused TEXT;
ALTER TABLE catalogs ADD COLUMN sweep_refused_micros BIGINT;
-- A worker that runs Lean without memory or process limits (no cgroups).
ALTER TABLE workers ADD COLUMN unlimited BOOLEAN NOT NULL DEFAULT FALSE;
