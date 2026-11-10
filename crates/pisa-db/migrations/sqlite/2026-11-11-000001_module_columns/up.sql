-- As for PostgreSQL. SQLite adds one column a statement, and a column it
-- adds can't be NOT NULL without a default; every row has them all the same.
ALTER TABLE modules ADD COLUMN build TEXT;
ALTER TABLE modules ADD COLUMN files BLOB;
ALTER TABLE modules ADD COLUMN header TEXT;
ALTER TABLE modules ADD COLUMN status TEXT CHECK (status IN ('proved', 'conditional', 'sorry', 'rejected'));
ALTER TABLE modules ADD COLUMN verified BOOLEAN;
ALTER TABLE modules ADD COLUMN created BIGINT;
ALTER TABLE module_imports ADD COLUMN position INTEGER NOT NULL DEFAULT 0;

UPDATE module_imports SET position = (
    SELECT CAST(x.key AS INTEGER) FROM modules m, json_each(m.meta, '$.imports') x
    WHERE m.version = module_imports.version AND m.id = module_imports.module
      AND x.value = module_imports.parent);

UPDATE modules SET
    build = json_extract(meta, '$.modules[#-1].build'),
    files = jsonb_extract(meta, '$.modules[#-1].files'),
    header = json_extract(meta, '$.header'),
    status = json_extract(meta, '$.status'),
    verified = COALESCE(json_extract(meta, '$.verified'), 0),
    created = json_extract(meta, '$.created'),
    meta = jsonb_remove(meta, '$.modules', '$.imports', '$.header', '$.status', '$.verified', '$.created');
