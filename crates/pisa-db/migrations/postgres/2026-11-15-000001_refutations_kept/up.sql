-- A refutation outlives the snapshot that made it, as the record that the
-- target is false: the audit marks it gone. `target_hash` is the hash of the
-- target's statement, by which a spec that amends the refuted one, and keeps
-- the statement, is shown the refutation; null in records made before.
ALTER TABLE refutations DROP CONSTRAINT refutations_version_entry_fkey;
ALTER TABLE refutations ADD COLUMN target_hash TEXT;
