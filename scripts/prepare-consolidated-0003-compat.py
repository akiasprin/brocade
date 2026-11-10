#!/usr/bin/env python3
"""Generate the exact offline conversion to the consolidated prerelease 0003."""
import argparse
import hashlib
from pathlib import Path
import sys


INITIAL = "794044a4f1b7ead02b8102e21bc7f94d3c01f309216a5e15ada747c61de528c04b494a46e993f683a7d256aa37cd0947"
SECOND = "54300599c9b4cce71f9e574b3f43141fee2fbd50279f829feda08f537bcde441a725ff1af4e0fbbf96fb9d80966552f1"
OLD_THIRD = "7c2aa0289adf688a3342363065901e3615a436178e43db652a3ac228a7fb1503cc354f3de744a8fa1649b97f30cc77d9"
OLD_FOURTH = "6e837e2dec6e8296827e7053ebce42dd6abaccdc2245b7e13aef8306b60e0cb5db5c04b7a03714fd629cca5759f394f6"
OLD_FIFTH = "6c03b9240a9be4c7bb1ab334223fa72987c7bcdd72a69c44ceaeb82fa83ff30f4f8e41d85382b02160c58c3aff0f725e"
OLD_CONSOLIDATED = "629a3639adc5f87a6e6677d0b471c57872adbf3d8b735c819eb7d2467a57e1b3f4030b1dc96de68099e32df7caf7c01f"
TARGET = "6b9c70bc99b3ecd499b33fea34c0a927d8dcaa604d90e09937899bfee2415b20a21d81d2c599de4c18222ace1f2204e8"
BOUNDARY = b"\n-- Consolidated independent IPv4 / IPv6 observations.\n"
CHANNEL_BOUNDARY = b"\n-- Consolidated Agent channel notifications.\n"


def generate(migration: bytes, *, from_consolidated: bool = False) -> str:
    if migration.count(BOUNDARY) != 1:
        raise ValueError("expected one unlock consolidation boundary")
    boundary = migration.index(BOUNDARY)
    if hashlib.sha384(migration[:boundary]).hexdigest() != OLD_THIRD:
        raise ValueError("unrecognized deployed 0003 prefix")
    if hashlib.sha384(migration).hexdigest() != TARGET:
        raise ValueError("unreviewed consolidated 0003 target")
    for statement in [
        b"ADD COLUMN address_family TEXT CHECK (address_family IN ('v4','v6'))",
        b"CREATE INDEX unlock_probe_jobs_family_worker",
        b"ADD COLUMN unlock_refresh_stale BOOLEAN NOT NULL DEFAULT TRUE",
        b"UPDATE unlock_probe_discovery SET next_discovery_at",
    ]:
        if migration.split(CHANNEL_BOUNDARY)[0].count(statement) != 1:
            raise ValueError("consolidated 0003 is missing an exact required statement")
    if migration.count(CHANNEL_BOUNDARY) != 1:
        raise ValueError("expected one Agent channel consolidation boundary")
    previous, notifications = migration.split(CHANNEL_BOUNDARY)
    if hashlib.sha384(previous).hexdigest() != OLD_CONSOLIDATED:
        raise ValueError("unrecognized consolidated 0003 prefix")
    if notifications.count(b"CREATE FUNCTION notify_agent_channel_change()") != 1:
        raise ValueError("Agent channel function is missing")
    notification_sql = notifications.decode("utf-8")
    count = notifications.split(b"\n-- BEGIN USER CYCLE RESET\n")[0].count(b"CREATE TRIGGER ")
    if count != 13:
        raise ValueError("Agent channel triggers are incomplete")
    if from_consolidated:
        history = """(SELECT count(*) FROM _sqlx_migrations) <> 3
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=1 AND success AND encode(checksum,'hex')='@INITIAL@')
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=2 AND success AND encode(checksum,'hex')='@SECOND@')
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=3 AND success AND encode(checksum,'hex')='@CONSOLIDATED@')"""
    else:
        history = """(SELECT count(*) FROM _sqlx_migrations) <> 5
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=1 AND success AND encode(checksum,'hex')='@INITIAL@')
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=2 AND success AND encode(checksum,'hex')='@SECOND@')
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=3 AND success AND encode(checksum,'hex')='@THIRD@')
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=4 AND success AND encode(checksum,'hex')='@FOURTH@')
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=5 AND success AND encode(checksum,'hex')='@FIFTH@')"""
    return SQL.replace("@HISTORY_GUARD@", history).replace("@CHANNEL_NOTIFICATIONS@", notification_sql) \
        .replace("@RETIRED_RECEIPTS@", "" if from_consolidated else "DELETE FROM _sqlx_migrations WHERE version IN (4,5);") \
        .replace("@INITIAL@", INITIAL).replace("@SECOND@", SECOND) \
        .replace("@THIRD@", OLD_THIRD).replace("@FOURTH@", OLD_FOURTH) \
        .replace("@FIFTH@", OLD_FIFTH).replace("@CONSOLIDATED@", OLD_CONSOLIDATED) \
        .replace("@TRIGGER_COUNT@", str(count)).replace("@TARGET@", TARGET)


SQL = r"""-- Generated from the reviewed consolidated prerelease 0003.
-- Execute only while every Console writer is stopped and after a verified backup restore rehearsal.
-- psql -X -v ON_ERROR_STOP=1 -v app_role=<owner> -d <database> -f <file>
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL lock_timeout='5s';
SET LOCAL statement_timeout='120s';
SET LOCAL ROLE :"app_role";
SET LOCAL search_path=public,pg_catalog;
SELECT pg_advisory_xact_lock(72625104422003);
LOCK TABLE _sqlx_migrations IN SHARE ROW EXCLUSIVE MODE;
LOCK TABLE control_state,unlock_probe_jobs,unlock_probe_discovery,user_online_sources IN ACCESS EXCLUSIVE MODE;
DO $guard$
BEGIN
    IF @HISTORY_GUARD@ THEN
        RAISE EXCEPTION 'unrecognized migration history; no changes applied';
    END IF;
    IF (SELECT relowner FROM pg_class WHERE oid='_sqlx_migrations'::regclass) <> current_user::regrole
       OR (SELECT relowner FROM pg_class WHERE oid='unlock_probe_jobs'::regclass) <> current_user::regrole
       OR (SELECT relowner FROM pg_class WHERE oid='unlock_probe_discovery'::regclass) <> current_user::regrole
       OR (SELECT relowner FROM pg_class WHERE oid='control_state'::regclass) <> current_user::regrole
       OR (SELECT relowner FROM pg_class WHERE oid='user_online_sources'::regclass) <> current_user::regrole THEN
        RAISE EXCEPTION 'app_role must own migration history and unlock tables';
    END IF;
    IF EXISTS (SELECT FROM pg_attribute WHERE attrelid='user_online_sources'::regclass
               AND attname='session_id' AND NOT attisdropped)
       OR NOT EXISTS (SELECT FROM pg_constraint WHERE conrelid='user_online_sources'::regclass
               AND contype='p' AND convalidated AND pg_get_constraintdef(oid)=
                   'PRIMARY KEY (node_id, tenant_id, user_id, ingress_id, source_ip)')
       OR to_regclass('public.user_online_sources_active_epoch') IS NOT NULL
       OR to_regclass('public.user_online_sources_user_history') IS NOT NULL THEN
        RAISE EXCEPTION 'unexpected online source schema; no changes applied';
    END IF;
    IF NOT EXISTS (SELECT FROM pg_attribute WHERE attrelid='unlock_probe_jobs'::regclass
                   AND attname='address_family' AND atttypid='text'::regtype AND NOT attnotnull AND NOT attisdropped)
       OR to_regclass('public.unlock_probe_jobs_family_worker') IS NULL
       OR NOT EXISTS (SELECT FROM pg_attribute WHERE attrelid='control_state'::regclass
                      AND attname='unlock_refresh_stale' AND atttypid='boolean'::regtype AND attnotnull AND NOT attisdropped)
       OR pg_get_expr((SELECT adbin FROM pg_attrdef WHERE adrelid='control_state'::regclass
                      AND adnum=(SELECT attnum FROM pg_attribute WHERE attrelid='control_state'::regclass
                                 AND attname='unlock_refresh_stale' AND NOT attisdropped)), 'control_state'::regclass) <> 'true'
       OR NOT EXISTS (SELECT FROM pg_constraint WHERE conrelid='unlock_probe_jobs'::regclass
                      AND contype='c' AND convalidated AND pg_get_constraintdef(oid) LIKE '%address_family%v4%v6%') THEN
        RAISE EXCEPTION 'unexpected unlock schema; no changes applied';
    END IF;
END
$guard$;
CREATE TEMP TABLE consolidation_receipts_before ON COMMIT DROP AS
    SELECT version,to_jsonb(m) AS receipt FROM _sqlx_migrations m;
CREATE TEMP TABLE consolidation_jobs_before ON COMMIT DROP AS
    SELECT scope_key,to_jsonb(j) AS record FROM unlock_probe_jobs j;
CREATE TEMP TABLE consolidation_discovery_before ON COMMIT DROP AS
    SELECT node_id,to_jsonb(d) AS record FROM unlock_probe_discovery d;
CREATE TEMP TABLE consolidation_control_before ON COMMIT DROP AS
    SELECT to_jsonb(c) AS record FROM control_state c;
CREATE TEMP TABLE consolidation_presence_before ON COMMIT DROP AS
    SELECT to_jsonb(s) AS record FROM user_online_sources s;
SELECT jsonb_build_object('event','before-0003-consolidation','receipts',
    (SELECT jsonb_agg(receipt ORDER BY version) FROM consolidation_receipts_before));
@CHANNEL_NOTIFICATIONS@
UPDATE _sqlx_migrations SET checksum=decode('@TARGET@','hex') WHERE version=3;
@RETIRED_RECEIPTS@
DO $verify$
BEGIN
    IF (SELECT count(*) FROM _sqlx_migrations) <> 3
       OR NOT EXISTS (SELECT FROM pg_constraint WHERE conrelid='user_online_sources'::regclass
                      AND contype='p' AND pg_get_constraintdef(oid)='PRIMARY KEY (session_id)')
       OR NOT EXISTS (SELECT FROM pg_index WHERE indexrelid=to_regclass('public.user_online_sources_active_epoch')
                      AND indisunique AND indisvalid AND pg_get_expr(indpred,indrelid)='active')
       OR EXISTS ((SELECT record FROM consolidation_presence_before
                   EXCEPT ALL SELECT to_jsonb(s)-'session_id' FROM user_online_sources s)
                  UNION ALL
                  (SELECT to_jsonb(s)-'session_id' FROM user_online_sources s
                   EXCEPT ALL SELECT record FROM consolidation_presence_before))
       OR (SELECT count(*) FROM pg_trigger
           WHERE tgfoid='public.notify_agent_channel_change()'::regprocedure
             AND NOT tgisinternal AND tgenabled='O') <> @TRIGGER_COUNT@
       OR EXISTS (SELECT FROM consolidation_jobs_before b FULL JOIN unlock_probe_jobs j USING(scope_key)
                  WHERE b.record-'next_run_at' IS DISTINCT FROM to_jsonb(j)-'readiness_failures'-'dns_fingerprint'-'next_run_at'
                     OR (b.record->>'path_error' IS DISTINCT FROM 'family_unavailable'
                         AND b.record->'next_run_at' IS DISTINCT FROM to_jsonb(j)->'next_run_at'))
       OR EXISTS (SELECT FROM consolidation_discovery_before b FULL JOIN unlock_probe_discovery d USING(node_id)
                  WHERE b.record-'next_discovery_at' IS DISTINCT FROM to_jsonb(d)-'next_discovery_at'
                     OR (NOT EXISTS (SELECT FROM unlock_probe_jobs j WHERE j.node_id=d.node_id AND j.state='complete' AND j.path_error='family_unavailable')
                         AND b.record->'next_discovery_at' IS DISTINCT FROM to_jsonb(d)->'next_discovery_at'))
       OR EXISTS (SELECT FROM consolidation_control_before b FULL JOIN control_state c
                  ON TRUE WHERE b.record IS DISTINCT FROM to_jsonb(c)-'unlock_detection_mode'-'unlock_detection_interval_hours'-'unlock_detection_requested_at'-'unlock_detection_next_run_at')
       OR EXISTS (SELECT FROM consolidation_receipts_before b FULL JOIN _sqlx_migrations m USING(version)
                  WHERE COALESCE(b.version,m.version) NOT IN (3,4,5)
                    AND b.receipt IS DISTINCT FROM to_jsonb(m))
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=3 AND success
                      AND encode(checksum,'hex')='@TARGET@') THEN
        RAISE EXCEPTION 'consolidated 0003 verification failed';
    END IF;
END
$verify$;
SELECT jsonb_build_object('event','verified-0003-consolidation','migration_versions',ARRAY[1,2,3],
    'target_checksum','@TARGET@','original_receipts',
    (SELECT jsonb_agg(receipt ORDER BY version) FROM consolidation_receipts_before));
COMMIT;
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--migration", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="new file, or - for stdout")
    parser.add_argument("--from-consolidated", action="store_true",
                        help="accept only the exact previous consolidated [1,2,3] history")
    args = parser.parse_args()
    sql = generate(args.migration.read_bytes(), from_consolidated=args.from_consolidated)
    if str(args.output) == "-":
        sys.stdout.write(sql)
    else:
        with args.output.open("x", encoding="utf-8") as stream:
            stream.write(sql)


if __name__ == "__main__":
    main()
