-- Additive compatibility only: this script does not aggregate or delete any Load samples.
-- Stop ALL old Console writers, back up and rehearse first. Once the new Console starts sealing
-- history, reverting to a writer that ignores the fence is unsafe, and 30-second data is lost.
-- load_schema_file must be the BEGIN/END LOAD ROLLUP SCHEMA block of the target 0001.
-- psql -X -v app_role=brocade -v load_schema_file=/verified/load-schema.sql -f this-file
-- Do not use this script to bypass SQLx migration-checksum verification.
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL ROLE :"app_role";
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '15min';
LOCK TABLE node_load_samples IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF to_regclass('node_load_compaction_state') IS NOT NULL
        OR EXISTS (SELECT 1 FROM pg_attribute
            WHERE attrelid = 'node_load_samples'::regclass AND attname = 'is_rollup' AND NOT attisdropped)
    THEN
        RAISE EXCEPTION 'Load minute compatibility already exists or is incomplete; inspect before retry';
    END IF;
END $$;
ALTER TABLE node_load_samples ADD COLUMN is_rollup BOOLEAN NOT NULL DEFAULT FALSE;
\i :load_schema_file
COMMIT;
