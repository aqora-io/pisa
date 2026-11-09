-- The entry that an entry's create replaced, having checked that it kept
-- every statement of the replaced one, and what it allowed to change (JSON
-- of `api::Allow`, or NULL for nothing). Only that create sets them. The link
-- outlives the replaced entry, as history.
ALTER TABLE entries ADD COLUMN replaces TEXT;
ALTER TABLE entries ADD COLUMN replaces_allow TEXT;
