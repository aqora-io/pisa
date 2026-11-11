DELETE FROM refutations r
    WHERE NOT EXISTS (SELECT 1 FROM entries e WHERE e.version = r.version AND e.id = r.entry);
ALTER TABLE refutations DROP COLUMN target_hash;
ALTER TABLE refutations ADD CONSTRAINT refutations_version_entry_fkey
    FOREIGN KEY (version, entry) REFERENCES entries (version, id) ON DELETE CASCADE;
