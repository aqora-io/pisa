-- A module's record as columns, so that listings read what they show
-- without its code and check: its own build and files (the closure is no
-- longer stored, but read through `module_imports`), its header, status,
-- whether it is verified, and when it was built. `meta` keeps the code, the
-- check result and `macros`. `position` is an import's place in the header.
ALTER TABLE modules
    ADD COLUMN build TEXT,
    ADD COLUMN files JSONB,
    ADD COLUMN header TEXT,
    ADD COLUMN status TEXT,
    ADD COLUMN verified BOOLEAN,
    ADD COLUMN created BIGINT;
ALTER TABLE module_imports ADD COLUMN position INTEGER NOT NULL DEFAULT 0;

UPDATE module_imports i SET position = x.ord - 1
FROM modules m, jsonb_array_elements_text(m.meta -> 'imports') WITH ORDINALITY AS x (parent, ord)
WHERE m.version = i.version AND m.id = i.module AND x.parent = i.parent;

UPDATE modules SET
    build = meta -> 'modules' -> -1 ->> 'build',
    files = meta -> 'modules' -> -1 -> 'files',
    header = meta ->> 'header',
    status = meta ->> 'status',
    verified = COALESCE((meta ->> 'verified')::BOOLEAN, FALSE),
    created = (meta ->> 'created')::BIGINT,
    meta = meta - 'modules' - 'imports' - 'header' - 'status' - 'verified' - 'created';

ALTER TABLE modules
    ALTER COLUMN build SET NOT NULL,
    ALTER COLUMN files SET NOT NULL,
    ALTER COLUMN header SET NOT NULL,
    ALTER COLUMN status SET NOT NULL,
    ALTER COLUMN verified SET NOT NULL,
    ALTER COLUMN created SET NOT NULL,
    ADD CONSTRAINT modules_status CHECK (status IN ('proved', 'conditional', 'sorry', 'rejected'));
