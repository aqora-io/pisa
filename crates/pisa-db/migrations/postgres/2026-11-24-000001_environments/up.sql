-- The Lake project behind each version a worker has served, which stays
-- after its pool is gone: what a snapshot or a check of that version ran on,
-- and what builds it again.
CREATE TABLE environments (
    version TEXT NOT NULL PRIMARY KEY,
    -- The Lean toolchain it pins, and the Lake packages its manifest pins as
    -- JSON, as on `workers`.
    toolchain TEXT NOT NULL,
    packages TEXT NOT NULL,
    -- Its files by name, as JSON: `lean-toolchain`, the lakefile, the
    -- manifest and the top-level `.lean` files.
    files TEXT NOT NULL,
    created_micros BIGINT NOT NULL
);
