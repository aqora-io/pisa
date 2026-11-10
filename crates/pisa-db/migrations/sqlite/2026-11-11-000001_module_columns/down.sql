-- Best effort: each record gets back its own module as its closure.
UPDATE modules SET meta = jsonb_set(meta,
    '$.modules', jsonb_array(jsonb_object('id', id, 'build', build, 'files', json(files))),
    '$.imports', COALESCE((SELECT jsonb_group_array(parent) FROM
                             (SELECT parent FROM module_imports i
                              WHERE i.version = modules.version AND i.module = modules.id
                              ORDER BY i.position)), jsonb_array()),
    '$.header', header, '$.status', status, '$.verified', json(CASE WHEN verified THEN 'true' ELSE 'false' END),
    '$.created', created);
ALTER TABLE modules DROP COLUMN build;
ALTER TABLE modules DROP COLUMN files;
ALTER TABLE modules DROP COLUMN header;
ALTER TABLE modules DROP COLUMN status;
ALTER TABLE modules DROP COLUMN verified;
ALTER TABLE modules DROP COLUMN created;
ALTER TABLE module_imports DROP COLUMN position;
