ALTER TABLE entries ADD COLUMN replaces TEXT;
ALTER TABLE entries ADD COLUMN replaces_allow TEXT;
ALTER TABLE entries ADD COLUMN replacement TEXT;
UPDATE entries SET
    replaces = (SELECT l.replaces FROM links l WHERE l.version = entries.version AND l.entry = entries.id),
    replaces_allow = (SELECT l.allow FROM links l WHERE l.version = entries.version AND l.entry = entries.id),
    replacement = (SELECT l.replacement FROM links l WHERE l.version = entries.version AND l.entry = entries.id);
CREATE INDEX entries_replaces ON entries (version, replaces);
DROP TABLE links;
