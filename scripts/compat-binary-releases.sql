-- Manual compatibility for the single 0001 schema; never run beside an old Console.
-- psql -v app_role=brocade -v binary_schema_file=/verified/schema-block.sql -f ...
-- The schema file is the BEGIN/END BINARY RELEASE SCHEMA block from the target 0001.
-- Back up first. Verify owners, counts, identities and evidence before changing SQLx checksum.
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL ROLE :"app_role";
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';
SELECT pg_advisory_xact_lock(1481785689);
LOCK TABLE xray_releases, xray_release_targets, xray_release_events IN ACCESS EXCLUSIVE MODE;
DO $$ BEGIN
    IF to_regclass('binary_releases') IS NOT NULL THEN
        RAISE EXCEPTION 'binary release ledger already exists; refusing to duplicate history';
    END IF;
    IF EXISTS (SELECT 1 FROM xray_releases WHERE active) THEN
        RAISE EXCEPTION 'finish or cancel active Xray releases before compatibility conversion';
    END IF;
END $$;

\i :binary_schema_file

INSERT INTO binary_releases
    (id, component, idempotency_key, build_id, version, artifacts, status,
     note, created_at, created_by, halted_at, finished_at)
SELECT id, 'xray', idempotency_key, build_id, version, artifacts, status,
       note, created_at, created_by, halted_at, finished_at
FROM xray_releases;

INSERT INTO binary_release_targets
    (release_id, node_id, status, attempt, before_sha256, desired_sha256, arch, error,
     reported_performed_update, reported_service_enabled, reported_installed_sha256,
     reported_running_sha256, verification, dispatched_at, finished_at)
SELECT release_id, node_id, status, attempt, before_sha256, desired_sha256, arch, error,
       reported_performed_update, reported_xray_enabled, reported_installed_sha256,
       reported_running_sha256, 'legacy', dispatched_at, finished_at
FROM xray_release_targets;

INSERT INTO binary_release_events (id, release_id, kind, node_id, actor, detail, created_at)
SELECT id, release_id, kind, node_id, actor,
       detail || CASE WHEN wave IS NULL THEN '{}'::jsonb ELSE jsonb_build_object('legacy_wave', wave) END,
       created_at
FROM xray_release_events;

-- Import actual report evidence, not an invented replay of earlier executions.
INSERT INTO binary_release_attempts
    (release_id, node_id, attempt, status, verification, error, evidence, finished_at)
SELECT DISTINCT ON (release_id, node_id, (detail->'report'->>'attempt')::integer)
       release_id, node_id, (detail->'report'->>'attempt')::integer,
       COALESCE(detail->>'classified_status', detail->'report'->>'outcome'),
       'legacy', COALESCE(detail->>'classification_error', detail->'report'->>'error'),
       jsonb_build_object('component', 'xray', 'report', detail->'report'), created_at
FROM xray_release_events
WHERE node_id IS NOT NULL AND detail->'report'->>'attempt' IS NOT NULL
ORDER BY release_id, node_id, (detail->'report'->>'attempt')::integer, id DESC;

-- Attempts without a recorded report remain explicitly legacy/unknown, not receipt-verified.
INSERT INTO binary_release_attempts
    (release_id, node_id, attempt, status, verification, error, evidence, started_at, finished_at)
SELECT release_id, node_id, attempt, status, 'legacy', error,
       jsonb_build_object('legacy_target', to_jsonb(t)), dispatched_at, finished_at
FROM xray_release_targets t
ON CONFLICT (release_id, node_id, attempt)
DO UPDATE SET started_at = EXCLUDED.started_at;

SELECT setval(pg_get_serial_sequence('binary_releases', 'id'), COALESCE(max(id), 1), count(*) > 0)
FROM binary_releases;
SELECT setval(pg_get_serial_sequence('binary_release_events', 'id'), COALESCE(max(id), 1), count(*) > 0)
FROM binary_release_events;

-- Preserve removed layout metadata in one migration event per release.
INSERT INTO binary_release_events (release_id, kind, detail)
SELECT r.id, 'legacy-layout-imported',
       jsonb_build_object('confirmed_wave', r.confirmed_wave, 'batch_size', r.batch_size,
           'target_waves', (SELECT jsonb_object_agg(t.node_id, t.wave) FROM xray_release_targets t WHERE t.release_id = r.id))
FROM xray_releases r;

DO $$ BEGIN
    IF (SELECT count(*) FROM binary_releases) <> (SELECT count(*) FROM xray_releases)
       OR (SELECT count(*) FROM binary_release_targets) <> (SELECT count(*) FROM xray_release_targets)
       OR EXISTS (
          SELECT 1 FROM xray_release_events old LEFT JOIN binary_release_events new USING (id)
          WHERE new.id IS NULL OR NOT (new.detail @> old.detail) OR new.release_id <> old.release_id
       ) THEN
        RAISE EXCEPTION 'release compatibility verification failed';
    END IF;
END $$;
-- Old ledgers are replaced only after lossless identity/evidence checks in this transaction.
DROP TABLE xray_release_events;
DROP TABLE xray_release_targets;
DROP TABLE xray_releases;
COMMIT;
