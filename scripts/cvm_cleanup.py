#!/usr/bin/env python3
"""Inventory or clear explicitly owned CVM experiment records; preserve every other table.

This maintenance tool does not stop services, delete Guests, remove disks, unload a runtime,
change migrations or deploy Console. Complete the separate node-resource cleanup and pause
CVM writers before applying a reviewed inventory. Database credentials are read from a
private file and passed to psql through its environment, never through command arguments.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time
from urllib.parse import parse_qsl, unquote, urlsplit


TABLES = frozenset('''
cvm_hosts cvm_host_policies cvm_host_policy_events
cvm_network_quota_hosts cvm_user_host_network_quotas cvm_network_quota_events
cvm_quotas cvm_quota_changes cvm_instances cvm_volumes cvm_interfaces
cvm_spec_versions cvm_operations cvm_observation_state cvm_observation_samples
cvm_observation_events cvm_terminal_policies cvm_terminal_policy_events
cvm_nic_nodes cvm_nic_publications cvm_nic_publication_versions cvm_nic_requests
cvm_nic_allocations cvm_nic_bindings cvm_nic_route_versions cvm_nic_resources
cvm_nic_meter_cursors cvm_nic_monthly_usage cvm_entry_nodes cvm_entry_publications
cvm_entry_publication_versions cvm_entry_tunnels cvm_entries
'''.split())
MAX_TABLES = 512
PROTECTED_FINGERPRINT_TABLES = frozenset('''
_sqlx_migrations admin_operators admin_sessions tenants users nodes apps chains
fronts front_vias front_external_vias ingresses ingress_client_settings
external_outbounds external_outbound_bindings forwarding_rules forwarding_rule_nodes
forwarding_entitlements forwarding_node_quotas node_enrollments node_lifecycle_state
node_cert_label node_cert_trusted_labels entitlements grants control_state
user_app_quotas user_app_rate_limits subscriptions cert_labels cert_domains certificates
notification_channels data_retention_policies first_run_onboarding settings
'''.split())


class CleanupError(RuntimeError):
    pass


def timestamp():
    return datetime.now(timezone.utc).isoformat(timespec='seconds')


def literal(value):
    return "'" + value.replace("'", "''") + "'"


def identifier(value):
    return 'public."' + value.replace('"', '""') + '"'


def credentials(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
        raise CleanupError('database URL file must be an owned private regular file')
    if info.st_size > 4096:
        raise CleanupError('database URL file exceeds 4096 bytes')
    value = path.read_text().strip()
    if not value.startswith(('postgres://', 'postgresql://')) or re.search(r'\s', value):
        raise CleanupError('database URL file must contain one PostgreSQL URL')
    return value


def connection_env(url):
    env = os.environ.copy()
    # libpq does not expand a URI supplied through the PGDATABASE default.
    # Expand its fields into environment variables, keeping passwords out of argv.
    parsed = urlsplit(url)
    fields = {
        'PGDATABASE': unquote(parsed.path.lstrip('/')),
        'PGCONNECT_TIMEOUT': '10', 'PGTZ': 'UTC',
    }
    if parsed.hostname is not None:
        fields['PGHOST'] = unquote(parsed.hostname)
    if parsed.port is not None:
        fields['PGPORT'] = str(parsed.port)
    if parsed.username is not None:
        fields['PGUSER'] = unquote(parsed.username)
    if parsed.password is not None:
        fields['PGPASSWORD'] = unquote(parsed.password)
    parameters = {
        'host': 'PGHOST', 'hostaddr': 'PGHOSTADDR', 'port': 'PGPORT',
        'user': 'PGUSER', 'password': 'PGPASSWORD', 'dbname': 'PGDATABASE',
        'sslmode': 'PGSSLMODE', 'sslrootcert': 'PGSSLROOTCERT',
        'sslcert': 'PGSSLCERT', 'sslkey': 'PGSSLKEY',
        'channel_binding': 'PGCHANNELBINDING', 'application_name': 'PGAPPNAME',
        'options': 'PGOPTIONS', 'connect_timeout': 'PGCONNECT_TIMEOUT',
    }
    for key, value in parse_qsl(parsed.query):
        if key not in parameters:
            raise CleanupError('database URL contains an unsupported connection option')
        fields[parameters[key]] = value
    env.update(fields)
    return env


def query(url, sql):
    env = connection_env(url)
    try:
        result = subprocess.run(
            ['psql', '-X', '-A', '-t', '-q', '-v', 'ON_ERROR_STOP=1', '-v', 'VERBOSITY=sqlstate'],
            input="SET timezone='UTC';\nSET statement_timeout='120s';\n" + sql,
            text=True, capture_output=True, env=env, timeout=150,
        )
    except subprocess.TimeoutExpired:
        raise CleanupError('database operation exceeded its bounded deadline') from None
    if result.returncode:
        code = re.search(r'ERROR:\s+([A-Z0-9]{5})\b', result.stderr)
        raise CleanupError('database operation failed' + (f' (SQLSTATE {code[1]})' if code else ''))
    lines = [line for line in result.stdout.splitlines() if line.startswith('{')]
    if not lines:
        raise CleanupError('database operation returned no inventory')
    return json.loads(lines[-1])


def backup(url, tables, path):
    if not tables or not set(tables).issubset(TABLES):
        raise CleanupError('backup requires a nonempty explicit CVM table inventory')
    command = ['pg_dump', '--format=custom', '--compress=6', '--data-only', '--no-owner', '--no-acl']
    for table in tables:
        command += ['--table', identifier(table)]
    with path.open('xb') as output:
        os.fchmod(output.fileno(), 0o600)
        try:
            result = subprocess.run(command, stdout=output, stderr=subprocess.PIPE,
                                    env=connection_env(url), timeout=150)
        except subprocess.TimeoutExpired:
            raise CleanupError('CVM-only backup timed out; no records were cleared') from None
        output.flush()
        os.fsync(output.fileno())
    if result.returncode or path.stat().st_size == 0:
        raise CleanupError('CVM-only backup failed; no records were cleared')
    if path.stat().st_size > 128 * 1024 * 1024:
        raise CleanupError('CVM-only backup exceeds the maintenance size bound')
    with path.open('rb') as source:
        digest = hashlib.file_digest(source, 'sha256').hexdigest()
    return {'file': str(path), 'sha256': digest, 'bytes': path.stat().st_size}


def fingerprint(tables):
    if not tables:
        return "'{}'::jsonb"
    parts = []
    for table in tables:
        parts.append(
            f"SELECT {literal(table)} AS name, jsonb_build_array(count(*), "
            f"coalesce(sum(hashtextextended(to_jsonb(t)::text,0)::numeric),0)::text, "
            f"coalesce(sum(hashtextextended(to_jsonb(t)::text,1)::numeric),0)::text) AS value "
            f"FROM {identifier(table)} t"
        )
    return "(SELECT jsonb_object_agg(name,value) FROM (" + ' UNION ALL '.join(parts) + ') q)'


def inventory(url):
    tables = query(url, """
SELECT jsonb_build_object('database',current_database(),'tables',coalesce(jsonb_agg(c.relname ORDER BY c.relname),'[]'::jsonb))
FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname='public' AND c.relkind IN ('r','p');
"""
    )
    names = tables['tables']
    if len(names) > MAX_TABLES:
        raise CleanupError('schema exceeds the bounded maintenance inventory')
    unknown = [name for name in names if name.startswith('cvm_') and name not in TABLES]
    if unknown:
        raise CleanupError('unreviewed CVM tables: ' + ', '.join(unknown))
    owned = sorted(TABLES.intersection(names))
    preserved = sorted(set(names) - TABLES)
    # High-frequency usage/probe tables can contain millions of rows. They are
    # structurally protected by the explicit TRUNCATE list and trigger/FK checks;
    # fingerprint stable identity/configuration tables without rescanning traffic.
    protected = sorted(PROTECTED_FINGERPRINT_TABLES.intersection(preserved))
    owned_sql = 'ARRAY[' + ','.join(literal(name) for name in owned) + ']::text[]'
    edges = query(url, f"""
SELECT jsonb_build_object(
 'external_references', (SELECT coalesce(jsonb_agg(conrelid::regclass::text),'[]'::jsonb)
   FROM pg_constraint WHERE contype='f' AND confrelid IN
     (SELECT oid FROM pg_class WHERE relnamespace='public'::regnamespace AND relname=ANY({owned_sql}))
   AND conrelid NOT IN
     (SELECT oid FROM pg_class WHERE relnamespace='public'::regnamespace AND relname=ANY({owned_sql}))),
 'truncate_triggers', (SELECT coalesce(jsonb_agg(t.tgname),'[]'::jsonb)
   FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid
   WHERE c.relnamespace='public'::regnamespace AND c.relname=ANY({owned_sql})
     AND NOT t.tgisinternal AND (t.tgtype::integer & 32)<>0));
"""
    )
    contents = query(url, 'BEGIN READ ONLY;\nSELECT jsonb_build_object(\'cvm\',' + fingerprint(owned)
                     + ',\'preserved\',' + fingerprint(protected) + ');\nCOMMIT;')
    return {'database': tables['database'], 'cvm_tables': owned, 'preserved_tables': preserved,
            'protected_fingerprint_tables': protected,
            'fingerprints': contents, 'blockers': edges}


def apply(url, expected):
    current = inventory(url)
    if any(current['blockers'].values()):
        raise CleanupError('external references or truncate triggers exceed the CVM-only boundary')
    if current['database'] != expected['database'] or current['cvm_tables'] != expected['cvm_tables']:
        raise CleanupError('reviewed database or table inventory changed')
    if current['preserved_tables'] != expected['preserved_tables']:
        raise CleanupError('preserved table inventory changed')
    owned = current['cvm_tables']
    if not owned:
        raise CleanupError('no CVM tables matched; refusing an empty cleanup')
    expected_cvm = literal(json.dumps(expected['fingerprints']['cvm'], sort_keys=True))
    protected = fingerprint(current['protected_fingerprint_tables'])
    sql = f"""
BEGIN ISOLATION LEVEL REPEATABLE READ;
SET LOCAL lock_timeout='5s';
SELECT pg_advisory_xact_lock(1112687438,65);
LOCK TABLE {', '.join(identifier(name) for name in owned)} IN ACCESS EXCLUSIVE MODE;
DO $cleanup$
DECLARE preserved_before jsonb; preserved_after jsonb;
BEGIN
 IF {fingerprint(owned)} IS DISTINCT FROM {expected_cvm}::jsonb THEN
   RAISE EXCEPTION 'CVM inventory changed since review';
 END IF;
 preserved_before := {protected};
 TRUNCATE TABLE {', '.join(identifier(name) for name in owned)};
 preserved_after := {protected};
 IF preserved_after IS DISTINCT FROM preserved_before THEN
   RAISE EXCEPTION 'non-CVM data changed; rolling back cleanup';
 END IF;
END $cleanup$;
COMMIT;
SELECT jsonb_build_object('database',current_database(),'cvm',{fingerprint(owned)});
"""
    result = query(url, sql)
    if any(value[0] for value in result['cvm'].values()):
        raise CleanupError('CVM writers recreated records; cleanup is not complete')
    return result


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--database-url-file', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--apply-reviewed', type=Path)
    parser.add_argument('--backup-file', type=Path)
    parser.add_argument('--cvm-writers-paused', action='store_true')
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x') as output:
        json.dump({'started_at': timestamp(), 'status': 'running'}, output)
        output.flush()
        os.fsync(output.fileno())
    started = time.monotonic()
    record = {'started_at': timestamp(), 'scope': 'CVM experiment records only'}
    try:
        url = credentials(args.database_url_file)
        if args.apply_reviewed:
            if not args.cvm_writers_paused:
                raise CleanupError('pause CVM writers and finish the separate node-resource cleanup before apply')
            if args.backup_file is None:
                raise CleanupError('apply requires a private CVM-only backup file')
            expected = json.loads(args.apply_reviewed.read_text())['inventory']
            record['backup'] = backup(url, expected['cvm_tables'], args.backup_file)
            record['result'] = apply(url, expected)
        else:
            record['inventory'] = inventory(url)
        record['status'] = 'complete'
    except (CleanupError, OSError, ValueError, KeyError) as error:
        record.update(status='failed', error=str(error))
    record.update(ended_at=timestamp(), elapsed_seconds=time.monotonic()-started)
    with args.output.open('w') as output:
        json.dump(record, output, ensure_ascii=False, indent=2)
        output.write('\n')
    print(json.dumps({'status': record['status'], 'report': str(args.output),
                      'elapsed_seconds': record['elapsed_seconds']}))
    return 0 if record['status'] == 'complete' else 1


if __name__ == '__main__':
    sys.exit(main())
