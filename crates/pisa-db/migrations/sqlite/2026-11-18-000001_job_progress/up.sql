-- What a job's attempt last said it was doing, as JSON (an accept's phase,
-- kernel and peak memory), for GET /jobs/{id}. A claim clears it, so that
-- each attempt starts with none.
ALTER TABLE jobs ADD COLUMN progress BLOB;
