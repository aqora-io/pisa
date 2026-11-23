-- The project a worker serves: the Lean toolchain it pins, and the Lake
-- packages its manifest pins as JSON, which `/health` lists by pool (empty
-- for a worker from before workers said).
ALTER TABLE workers ADD COLUMN toolchain TEXT NOT NULL DEFAULT '';
ALTER TABLE workers ADD COLUMN packages TEXT NOT NULL DEFAULT '[]';
