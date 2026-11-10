-- The definitions a spec entry leaves for the entries that replace it to
-- change (a JSON array of names), which those inherit.
ALTER TABLE entries ADD COLUMN holes TEXT;
