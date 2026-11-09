-- Entries are found by the entry they replaced, to follow a link forward.
-- `replacement` is the comparison of the create that made the link (JSON of
-- `api::Replacement`), set exactly when `replaces` is.
CREATE INDEX entries_replaces ON entries (version, replaces);
ALTER TABLE entries ADD COLUMN replacement TEXT;
