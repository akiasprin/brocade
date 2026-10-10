-- Add the durable certificate scan queue to an existing Console database.
-- Run only while every Console writer is stopped. Extract certificate_scan_schema_file from the
-- target 0001 BEGIN/END CERTIFICATE SCAN SCHEMA block and verify it before use.
--
-- psql -X -v ON_ERROR_STOP=1 -d brocade -v app_role=brocade \
--   -v certificate_scan_schema_file=/verified/certificate-scan-schema.sql \
--   -f scripts/compat-certificate-scan.sql
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL ROLE :"app_role";
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';
SELECT pg_advisory_xact_lock(1481785689);

DO $$ BEGIN
    IF to_regclass('certificate_scan_runs') IS NOT NULL THEN
        RAISE EXCEPTION 'certificate_scan_runs already exists; review schema before retrying';
    END IF;
END $$;

-- This changes only the default for new rows. Existing domains retain their explicit value.
ALTER TABLE cert_domains ALTER COLUMN renew_before_days SET DEFAULT 60;
\i :certificate_scan_schema_file
COMMIT;
