-- Since when a worker has been live without a break, in microseconds since
-- the Unix epoch: when it registered, or when it heartbeat again after going
-- quiet for longer than the queue's live window. A lost async job waits
-- until a worker has been live its max wait since it lapsed.
ALTER TABLE workers ADD COLUMN live_since_micros BIGINT NOT NULL DEFAULT 0;
UPDATE workers SET live_since_micros = seen_micros;
