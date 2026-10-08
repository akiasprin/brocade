-- Manual, lossless compatibility for the single 0001 schema. Stop ALL old Console writers first.
-- Back up and rehearse on a restored isolated database. Provision space for old + new relations,
-- indexes, temporary sort and WAL; do not assume the final size is the migration peak.
-- psql -X -v app_role=brocade -v ping_schema_file=/verified/ping-schema.sql -f this-file
-- ping_schema_file is the BEGIN/END PING SERIES SCHEMA block extracted from target 0001.
-- This script does NOT change SQLx checksums or authorize restarting the old binary.
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL ROLE :"app_role";
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '15min';
SELECT pg_advisory_xact_lock(1481785690);
LOCK TABLE node_ping_probe_samples IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF to_regclass('node_ping_probe_series') IS NOT NULL
       OR to_regclass('node_ping_probe_samples_legacy') IS NOT NULL THEN
        RAISE EXCEPTION 'PING series conversion already exists or is incomplete; inspect before retry';
    END IF;
END $$;
ALTER TABLE node_ping_probe_samples RENAME TO node_ping_probe_samples_legacy;
ALTER TABLE node_ping_probe_samples_legacy
    RENAME CONSTRAINT node_ping_probe_samples_pkey TO node_ping_probe_samples_legacy_pkey;
ALTER INDEX node_ping_probe_samples_probed_at_idx RENAME TO node_ping_probe_samples_legacy_time_idx;
-- PostgreSQL 18 stores named NOT NULL constraints. Free their original names as well, so the
-- replacement has exactly the same schema as a fresh install (PG16 has no such catalog rows).
DO $$ DECLARE constraint_name text; BEGIN
    FOR constraint_name IN SELECT conname FROM pg_constraint
        WHERE conrelid='node_ping_probe_samples_legacy'::regclass AND contype='n'
    LOOP
        EXECUTE format('ALTER TABLE node_ping_probe_samples_legacy RENAME CONSTRAINT %I TO %I',
            constraint_name, constraint_name || '_legacy');
    END LOOP;
END $$;

\i :ping_schema_file

INSERT INTO node_ping_probe_series (node_id, target, family)
SELECT DISTINCT node_id, target, family FROM node_ping_probe_samples_legacy
ORDER BY node_id, target, family;

INSERT INTO node_ping_probe_samples (series_id, probed_at, attempted, latency_us, skip_reason)
SELECT series.id, old.probed_at, old.attempted, old.latency_us, old.skip_reason
FROM node_ping_probe_samples_legacy old
JOIN node_ping_probe_series series USING (node_id, target, family)
ORDER BY series.id, old.probed_at;

ANALYZE node_ping_probe_series;
ANALYZE node_ping_probe_samples;
-- Exact row identity and values, including NULLs and sub-second timestamps. Count equality plus
-- the unique identities and this left join prove no missing, changed, duplicated or extra point.
DO $$ BEGIN
    IF (SELECT count(*) FROM node_ping_probe_samples) <>
       (SELECT count(*) FROM node_ping_probe_samples_legacy)
       OR EXISTS (
           SELECT 1 FROM node_ping_probe_samples_legacy old
           LEFT JOIN node_ping_probe_series series USING (node_id, target, family)
           LEFT JOIN node_ping_probe_samples new
             ON new.series_id = series.id AND new.probed_at = old.probed_at
           WHERE new.series_id IS NULL
              OR ROW(new.attempted, new.latency_us, new.skip_reason)
                 IS DISTINCT FROM ROW(old.attempted, old.latency_us, old.skip_reason)
       ) THEN
        RAISE EXCEPTION 'lossless PING conversion verification failed';
    END IF;
END $$;
-- No CASCADE: unexpected dependents abort the transaction, rather than losing unrelated objects.
DROP TABLE node_ping_probe_samples_legacy;
COMMIT;
