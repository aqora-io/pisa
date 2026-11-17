-- Whether a worker's last probe of the module store reached it, `NULL` until
-- it has probed, and when, in microseconds since the Unix epoch.
ALTER TABLE workers ADD COLUMN store_reachable BOOLEAN;
ALTER TABLE workers ADD COLUMN store_checked_micros BIGINT;
