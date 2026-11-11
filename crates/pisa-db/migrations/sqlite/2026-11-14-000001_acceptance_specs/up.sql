-- The spec snapshots each acceptance judged, to find a spec's acceptances.
-- A spec is kept as its entry's id, which may be deleted since.
CREATE TABLE acceptance_specs (
    version TEXT NOT NULL,
    spec TEXT NOT NULL,
    acceptance TEXT NOT NULL,
    created_micros BIGINT NOT NULL,
    PRIMARY KEY (version, spec, acceptance),
    FOREIGN KEY (version, acceptance) REFERENCES acceptances (version, id) ON DELETE CASCADE
);
CREATE INDEX acceptance_specs_newest ON acceptance_specs (version, spec, created_micros);
