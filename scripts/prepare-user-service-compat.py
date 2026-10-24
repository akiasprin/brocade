#!/usr/bin/env python3
"""Generate the explicitly approved one-time prerelease 0002 compatibility SQL.

DDL stays authoritative in 0002. This accepts only the reviewed old prefix and
complete target; it is not a general SQLx checksum repair tool. See the Console
deployment runbook for backup, restore rehearsal and rollback requirements.
"""

import argparse
import hashlib
from pathlib import Path

BASELINE = "201b4ff0a84b24548874f83d34198eabba509b08ed1acee83cc157d65faeeacb791c57b91014d709c1fbc1fb9acd0643"
TARGET = "54300599c9b4cce71f9e574b3f43141fee2fbd50279f829feda08f537bcde441a725ff1af4e0fbbf96fb9d80966552f1"
INITIAL = "794044a4f1b7ead02b8102e21bc7f94d3c01f309216a5e15ada747c61de528c04b494a46e993f683a7d256aa37cd0947"
MARKER = b"\n-- User billing/service policy is operational state"


def generate(migration):
    if migration.count(MARKER) != 1:
        raise ValueError("expected exactly one user-service boundary")
    boundary = migration.index(MARKER)
    if hashlib.sha384(migration[:boundary]).hexdigest() != BASELINE:
        raise ValueError("unrecognized published 0002 prefix")
    if hashlib.sha384(migration).hexdigest() != TARGET:
        raise ValueError("unreviewed target 0002; review a new compatibility path")
    delta = migration[boundary:].decode("utf-8")
    return HEADER.replace("@INITIAL@", INITIAL).replace("@BASELINE@", BASELINE) + delta + FOOTER.replace("@TARGET@", TARGET)


HEADER = r"""-- Generated from the complete reviewed 0002; retain this file with release evidence.
-- psql -X -v ON_ERROR_STOP=1 -v app_role=<owner> -d <restored-db> -f <file>
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';
SET LOCAL ROLE :"app_role";
SET LOCAL search_path = public, pg_catalog;
SELECT pg_advisory_xact_lock(72625104422002);
LOCK TABLE _sqlx_migrations IN SHARE ROW EXCLUSIVE MODE;
LOCK TABLE users IN ACCESS EXCLUSIVE MODE;
DO $guard$
BEGIN
    IF (SELECT count(*) FROM _sqlx_migrations) <> 2
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version = 1 AND success
                      AND encode(checksum, 'hex') = '@INITIAL@')
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version = 2 AND success
                      AND encode(checksum, 'hex') = '@BASELINE@') THEN
        RAISE EXCEPTION 'unrecognized migration history; no changes applied';
    END IF;
    IF (SELECT relowner FROM pg_class WHERE oid = 'users'::regclass) <> current_user::regrole
       OR (SELECT relowner FROM pg_class WHERE oid = '_sqlx_migrations'::regclass) <> current_user::regrole THEN
        RAISE EXCEPTION 'app_role must own users and migration history';
    END IF;
    IF EXISTS (SELECT FROM pg_attribute WHERE attrelid = 'users'::regclass AND NOT attisdropped
               AND attname IN ('traffic_reset_day', 'service_starts_at', 'service_expires_at', 'service_suspended'))
       OR to_regclass('public.user_service_policy_events') IS NOT NULL
       OR EXISTS (SELECT FROM pg_proc WHERE pronamespace = 'public'::regnamespace
                  AND proname IN ('user_service_state', 'user_traffic_cycle')) THEN
        RAISE EXCEPTION 'partial or repeated compatibility execution; inspect before retry';
    END IF;
END
$guard$;
CREATE TEMP TABLE compat_users_before ON COMMIT DROP AS
    SELECT tenant_id, id, md5(to_jsonb(u)::text) AS fingerprint FROM users u;
CREATE TEMP TABLE compat_receipts_before ON COMMIT DROP AS
    SELECT version, to_jsonb(m) AS receipt FROM _sqlx_migrations m;
"""

FOOTER = r"""
DO $verify$
BEGIN
    IF EXISTS (
        SELECT FROM compat_users_before b FULL JOIN users u USING (tenant_id, id)
        WHERE b.fingerprint IS DISTINCT FROM md5((to_jsonb(u)
            - ARRAY['traffic_reset_day', 'service_starts_at', 'service_expires_at', 'service_suspended'])::text)
    ) OR EXISTS (SELECT FROM users WHERE traffic_reset_day <> 1 OR service_starts_at IS NOT NULL
                 OR service_expires_at IS NOT NULL OR service_suspended) THEN
        RAISE EXCEPTION 'existing users or default service policy changed';
    END IF;
    IF (SELECT count(*) FROM pg_attribute WHERE attrelid = 'users'::regclass AND NOT attisdropped
        AND ((attname = 'traffic_reset_day' AND atttypid = 'integer'::regtype AND attnotnull)
          OR (attname IN ('service_starts_at', 'service_expires_at') AND atttypid = 'timestamptz'::regtype AND NOT attnotnull)
          OR (attname = 'service_suspended' AND atttypid = 'boolean'::regtype AND attnotnull))) <> 4
       OR (SELECT count(*) FROM user_service_policy_events) <> 0
       OR (SELECT relowner FROM pg_class WHERE oid = 'user_service_policy_events'::regclass) <> current_user::regrole
       OR NOT EXISTS (SELECT FROM pg_constraint WHERE conrelid = 'users'::regclass
                      AND conname = 'users_service_period' AND convalidated)
       OR NOT EXISTS (SELECT FROM pg_index WHERE indexrelid = 'user_service_policy_events_owner'::regclass AND indisvalid)
       OR (SELECT count(*) FROM pg_proc WHERE pronamespace = 'public'::regnamespace
           AND proname IN ('user_service_state', 'user_traffic_cycle') AND proowner = current_user::regrole) <> 2 THEN
        RAISE EXCEPTION 'new service schema or ownership verification failed';
    END IF;
    IF user_service_state(NULL, NULL, now()) <> 'active'
       OR user_service_state('2026-03-01T00:00:00Z', NULL, '2026-02-01T00:00:00Z') <> 'pending'
       OR user_service_state(NULL, '2026-01-01T00:00:00Z', '2026-02-01T00:00:00Z') <> 'expired'
       OR NOT EXISTS (SELECT FROM user_traffic_cycle(31, '2026-03-01T00:00:00+08')
                      WHERE starts_at = '2026-02-28T00:00:00+08' AND ends_at = '2026-03-31T00:00:00+08') THEN
        RAISE EXCEPTION 'service state or clamped traffic cycle verification failed';
    END IF;
END
$verify$;
-- Only a fully verified additive conversion may align the reviewed receipt.
UPDATE _sqlx_migrations SET checksum = decode('@TARGET@', 'hex') WHERE version = 2;
DO $receipt$
BEGIN
    IF EXISTS (
        SELECT FROM compat_receipts_before b FULL JOIN _sqlx_migrations m USING (version)
        WHERE (b.receipt - 'checksum') IS DISTINCT FROM (to_jsonb(m) - 'checksum')
           OR (m.version = 1 AND b.receipt IS DISTINCT FROM to_jsonb(m))
    ) OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version = 2 AND success
                     AND encode(checksum, 'hex') = '@TARGET@') THEN
        RAISE EXCEPTION 'migration receipt metadata changed unexpectedly';
    END IF;
END
$receipt$;
SELECT jsonb_build_object('compatibility', 'user-service-0002', 'verified', true,
                         'users_preserved', (SELECT count(*) FROM compat_users_before),
                         'migration_versions', ARRAY[1, 2], 'target_checksum', '@TARGET@');
COMMIT;
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--migration", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    sql = generate(args.migration.read_bytes())
    with args.output.open("x", encoding="utf-8") as stream:
        stream.write(sql)


if __name__ == "__main__":
    main()
