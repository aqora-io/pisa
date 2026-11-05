DROP INDEX jobs_done;
DROP INDEX jobs_token;
ALTER TABLE jobs DROP COLUMN kind;
ALTER TABLE jobs DROP COLUMN token;
