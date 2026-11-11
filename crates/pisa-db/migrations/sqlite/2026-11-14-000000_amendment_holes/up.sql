-- An amendment's change to the spec's holes: the amended spec's and the
-- new spec's (JSON arrays of names), when they differ. Null otherwise, and
-- for amendments made before the new spec kept the holes it didn't name.
ALTER TABLE amendments ADD COLUMN holes_before TEXT;
ALTER TABLE amendments ADD COLUMN holes_after TEXT;
