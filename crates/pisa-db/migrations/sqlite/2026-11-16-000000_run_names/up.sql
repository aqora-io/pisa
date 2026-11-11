-- A run's name, unique in its version: code may `import PisaRun.<name>`,
-- which imports the run's head. A run is named by its root, the entry its
-- links name as `run_root`, which may be deleted while the run goes on, so
-- `root` has no foreign key. A delete that leaves the run with no entries
-- removes the name.
CREATE TABLE run_names (
    version TEXT NOT NULL,
    name TEXT NOT NULL,
    root TEXT NOT NULL,
    PRIMARY KEY (version, name)
);
CREATE UNIQUE INDEX run_names_root ON run_names (version, root);
-- The code as a create sent it, where it imported runs by name: the
-- module's code imports their heads at the time in their place.
ALTER TABLE entries ADD COLUMN source TEXT;
