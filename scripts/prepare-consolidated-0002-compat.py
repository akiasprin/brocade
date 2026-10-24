#!/usr/bin/env python3
"""Generate the reviewed, offline conversion to the consolidated prerelease 0002.

Only the exact deployed 0002, optionally followed by the exact deployed 0003,
is accepted. Normal Console startup still rejects unknown or changed receipts.
Back up and rehearse on a restored database before using this SQL on a stopped
installation. Keep its output, including the original receipts, as release evidence.
"""

import argparse
import hashlib
from pathlib import Path
import sys

INITIAL = "794044a4f1b7ead02b8102e21bc7f94d3c01f309216a5e15ada747c61de528c04b494a46e993f683a7d256aa37cd0947"
SECOND = "375a1d3c5e05e7de1b1d1c26125787bdb227a8b74cb2fa1e3f0c2d959525a968652d9b1dcdcb7c570d9fb221d07ab71a"
THIRD = "1ef92edf6db7d2b20a9965cf8e770287211448212a454c714d9a378e66a25683d3790acf6d55c7666a1b265a31eb4c13"
TARGET = "54300599c9b4cce71f9e574b3f43141fee2fbd50279f829feda08f537bcde441a725ff1af4e0fbbf96fb9d80966552f1"
STRATEGY_MARKER = b"\n-- Consolidated tunnel target strategy.\n"
RETIREMENT_MARKER = b"\n-- Consolidated forwarding total-quota retirement.\n"


def generate(migration, with_target_strategy=True):
    for marker in (STRATEGY_MARKER, RETIREMENT_MARKER):
        if migration.count(marker) != 1:
            raise ValueError("expected exactly one consolidation boundary for each change")
    strategy = migration.index(STRATEGY_MARKER)
    retirement = migration.index(RETIREMENT_MARKER)
    if hashlib.sha384(migration[:strategy]).hexdigest() != SECOND:
        raise ValueError("unrecognized deployed 0002 prefix")
    if hashlib.sha384(migration[strategy + len(STRATEGY_MARKER):retirement]).hexdigest() != THIRD:
        raise ValueError("unrecognized deployed target-strategy SQL")
    if hashlib.sha384(migration).hexdigest() != TARGET:
        raise ValueError("unreviewed consolidated 0002 target")
    history_guard = (
        "(SELECT count(*) FROM _sqlx_migrations) <> 3 OR NOT EXISTS "
        "(SELECT FROM _sqlx_migrations WHERE version=3 AND success "
        f"AND encode(checksum,'hex')='{THIRD}')"
        if with_target_strategy else
        "(SELECT count(*) FROM _sqlx_migrations) <> 2"
    )
    schema_guard = (
        "NOT EXISTS (SELECT FROM pg_attribute WHERE attrelid='external_outbounds'::regclass "
        "AND attname='target_strategy' AND atttypid='text'::regtype AND NOT attisdropped)"
        if with_target_strategy else
        "EXISTS (SELECT FROM pg_attribute WHERE attrelid='external_outbounds'::regclass "
        "AND attname='target_strategy' AND NOT attisdropped)"
    )
    header = HEADER.replace("@HISTORY_GUARD@", history_guard).replace("@SCHEMA_GUARD@", schema_guard)
    header = header.replace("@INITIAL@", INITIAL).replace("@SECOND@", SECOND)
    delta = migration[retirement if with_target_strategy else strategy:].decode("utf-8")
    return header + delta + FOOTER.replace("@TARGET@", TARGET)


HEADER = r"""-- Generated from the reviewed consolidated 0002; execute only while Console is stopped.
-- psql -X -v ON_ERROR_STOP=1 -v app_role=<owner> -d <restored-db> -f <file>
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL lock_timeout='5s';
SET LOCAL statement_timeout='120s';
SET LOCAL ROLE :"app_role";
SET LOCAL search_path=public,pg_catalog;
SELECT pg_advisory_xact_lock(72625104422002);
LOCK TABLE _sqlx_migrations IN SHARE ROW EXCLUSIVE MODE;
LOCK TABLE users IN ACCESS EXCLUSIVE MODE;
DO $guard$
BEGIN
    IF @HISTORY_GUARD@
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=1 AND success
                      AND encode(checksum,'hex')='@INITIAL@')
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=2 AND success
                      AND encode(checksum,'hex')='@SECOND@') THEN
        RAISE EXCEPTION 'unrecognized migration history; no changes applied';
    END IF;
    IF (SELECT relowner FROM pg_class WHERE oid='users'::regclass) <> current_user::regrole
       OR (SELECT relowner FROM pg_class WHERE oid='_sqlx_migrations'::regclass) <> current_user::regrole THEN
        RAISE EXCEPTION 'app_role must own users and migration history';
    END IF;
    IF @SCHEMA_GUARD@
       OR to_regclass('public.user_traffic_policies') IS NULL
       OR to_regclass('public.retired_forwarding_traffic_policies') IS NOT NULL
       OR to_regprocedure('user_global_suspended(text,text,text)') IS NULL
       OR to_regprocedure('user_global_configured(text,text,text)') IS NULL THEN
        RAISE EXCEPTION 'unexpected schema or partial/repeated conversion; inspect before retry';
    END IF;
END
$guard$;
CREATE TEMP TABLE consolidation_receipts_before ON COMMIT DROP AS
    SELECT version,to_jsonb(m) AS receipt FROM _sqlx_migrations m;
CREATE TEMP TABLE consolidation_access_before ON COMMIT DROP AS
    SELECT tenant_id,user_id,app_id,to_jsonb(q) AS record FROM user_app_quotas q;
CREATE TEMP TABLE consolidation_budgets_before ON COMMIT DROP AS
    SELECT tenant_id,user_id,scope,to_jsonb(b) AS record FROM user_traffic_budgets b WHERE kind='access';
SELECT jsonb_build_object('event','before-0002-consolidation','receipts',
    (SELECT jsonb_agg(receipt ORDER BY version) FROM consolidation_receipts_before));
"""

FOOTER = r"""
DO $verify$
BEGIN
    IF EXISTS (SELECT FROM consolidation_access_before b FULL JOIN user_app_quotas q
               USING(tenant_id,user_id,app_id) WHERE b.record IS DISTINCT FROM to_jsonb(q))
       OR EXISTS (SELECT FROM consolidation_budgets_before b FULL JOIN
                  (SELECT * FROM user_traffic_budgets WHERE kind='access') q
                  USING(tenant_id,user_id,scope) WHERE b.record IS DISTINCT FROM to_jsonb(q)) THEN
        RAISE EXCEPTION 'access quota or budget data changed';
    END IF;
    IF to_regclass('public.user_traffic_policies') IS NOT NULL
       OR to_regclass('public.retired_forwarding_traffic_policies') IS NULL
       OR to_regprocedure('user_global_suspended(text,text,text)') IS NOT NULL
       OR to_regprocedure('user_global_configured(text,text,text)') IS NOT NULL
       OR EXISTS (SELECT FROM user_traffic_scopes WHERE kind='forwarding')
       OR EXISTS (SELECT FROM user_traffic_budgets WHERE kind='forwarding')
       OR NOT EXISTS (SELECT FROM pg_constraint WHERE conrelid='external_outbounds'::regclass
                      AND conname='external_outbounds_target_strategy_check' AND convalidated) THEN
        RAISE EXCEPTION 'consolidated schema verification failed';
    END IF;
END
$verify$;
-- Align only the verified, explicitly requested prerelease consolidation.
UPDATE _sqlx_migrations SET checksum=decode('@TARGET@','hex') WHERE version=2;
DELETE FROM _sqlx_migrations WHERE version=3;
DO $receipt$
BEGIN
    IF (SELECT count(*) FROM _sqlx_migrations) <> 2
       OR EXISTS (SELECT FROM consolidation_receipts_before b
                  FULL JOIN _sqlx_migrations m USING(version)
                  WHERE COALESCE(b.version,m.version)<>3
                    AND ((b.receipt-'checksum') IS DISTINCT FROM (to_jsonb(m)-'checksum')
                      OR (m.version=1 AND b.receipt IS DISTINCT FROM to_jsonb(m))))
       OR NOT EXISTS (SELECT FROM _sqlx_migrations WHERE version=2 AND success
                      AND encode(checksum,'hex')='@TARGET@') THEN
        RAISE EXCEPTION 'unexpected migration receipt changes';
    END IF;
END
$receipt$;
SELECT jsonb_build_object('event','verified-0002-consolidation','migration_versions',ARRAY[1,2],
    'target_checksum','@TARGET@','original_receipts',
    (SELECT jsonb_agg(receipt ORDER BY version) FROM consolidation_receipts_before));
COMMIT;
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--migration", type=Path, required=True)
    parser.add_argument("--without-target-strategy", action="store_true",
                        help="convert the exact [1,2] history instead of the default [1,2,3]")
    parser.add_argument("--output", type=Path, required=True, help="new file, or - for stdout")
    args = parser.parse_args()
    sql = generate(args.migration.read_bytes(), not args.without_target_strategy)
    if str(args.output) == "-":
        sys.stdout.write(sql)
    else:
        with args.output.open("x", encoding="utf-8") as stream:
            stream.write(sql)


if __name__ == "__main__":
    main()
