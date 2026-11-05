-- Jobs a client submitted asynchronously, and reads back by `token` (which,
-- unlike `id`, can't be guessed), with the endpoint they ran in `kind`. Both
-- are NULL for jobs a server waits for itself. An async job's outcome is
-- kept for an hour after `done_micros`. One nobody could run (given up, or
-- lost with its last worker) is closed with a JSON null `result`, not SQL
-- NULL, so it no longer counts as open; `attempts` then says which: 0 is no
-- capacity, more is lost.
ALTER TABLE jobs ADD COLUMN token TEXT;
ALTER TABLE jobs ADD COLUMN kind TEXT;
CREATE UNIQUE INDEX jobs_token ON jobs (token) WHERE token IS NOT NULL;
-- Done rows now stay an hour: the sweep finds them by when they were done.
CREATE INDEX jobs_done ON jobs (queue, done_micros) WHERE done_micros IS NOT NULL;
