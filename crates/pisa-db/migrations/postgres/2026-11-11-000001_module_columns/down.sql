-- Best effort: each record gets back its own module as its closure.
UPDATE modules m SET meta = m.meta
    || jsonb_build_object(
        'modules', jsonb_build_array(jsonb_build_object('id', m.id, 'build', m.build, 'files', m.files)),
        'imports', COALESCE((SELECT jsonb_agg(i.parent ORDER BY i.position) FROM module_imports i
                             WHERE i.version = m.version AND i.module = m.id), '[]'::JSONB),
        'header', m.header, 'status', m.status, 'verified', m.verified, 'created', m.created);
ALTER TABLE modules DROP CONSTRAINT modules_status;
ALTER TABLE modules
    DROP COLUMN build, DROP COLUMN files, DROP COLUMN header, DROP COLUMN status,
    DROP COLUMN verified, DROP COLUMN created;
ALTER TABLE module_imports DROP COLUMN position;
