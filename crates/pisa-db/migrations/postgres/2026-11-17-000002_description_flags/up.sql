-- Readers' flags on search's descriptions of declarations, each on the
-- description as it was, by its hash: one written again starts with none.
CREATE TABLE description_flags (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    description_hash TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_micros BIGINT NOT NULL
);
CREATE INDEX description_flags_name ON description_flags (name, description_hash);
