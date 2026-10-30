#!/usr/bin/env python3
"""Upgrade a real v0.3.1 schema, verify a restorable dump and unchanged historical receipt.

Uses only a fresh disposable Docker PostgreSQL and explicit candidate binaries. No production access.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import uuid


def command(args, **kwargs):
    result = subprocess.run(args, text=True, capture_output=True, timeout=240, **kwargs)
    if result.returncode:
        raise RuntimeError(f'{Path(args[0]).name} failed: {result.stderr[-2000:]}')
    return result.stdout


def run(args):
    output = args.output.resolve()
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    repo = Path(__file__).resolve().parents[3]
    migrations = repo / 'crates/brocade-store/migrations'
    versions = sorted(int(path.name.split('_', 1)[0]) for path in migrations.glob('*.sql'))
    assert versions and versions[0] == 1 and len(versions) >= 2
    source = (migrations / '0001_init.sql').read_text()
    checksum = hashlib.sha384(source.encode()).hexdigest()
    assert checksum == '794044a4f1b7ead02b8102e21bc7f94d3c01f309216a5e15ada747c61de528c04b494a46e993f683a7d256aa37cd0947'
    container = 'brocade-upgrade-' + uuid.uuid4().hex[:12]
    try:
        command(['docker', 'run', '-d', '--name', container, '-p', '127.0.0.1::5432',
                 '-e', 'POSTGRES_PASSWORD=upgrade@fixture', 'postgres:17-alpine'])
        command(['docker', 'exec', container, 'sh', '-c',
                 'i=0; until pg_isready -U postgres; do i=$((i+1)); [ "$i" -lt 60 ] || exit 1; sleep 1; done'])
        def sql(statement, database='postgres'):
            return command(['docker', 'exec', '-i', container, 'psql', '-XqAt', '-v', 'ON_ERROR_STOP=1',
                            '-U', 'postgres', '-d', database], input=statement).strip()
        sql(source)
        sql("""CREATE TABLE _sqlx_migrations (
            version BIGINT PRIMARY KEY, description TEXT NOT NULL,
            installed_on TIMESTAMPTZ DEFAULT now() NOT NULL, success BOOLEAN NOT NULL,
            checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL);
            INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time)
            VALUES (1,'init',true,decode('%s','hex'),0);
            UPDATE control_state SET site_name='preserved upgrade fixture';""" % checksum)
        port = command(['docker', 'port', container, '5432/tcp']).strip().rsplit(':', 1)[1]
        env = {**os.environ, 'DATABASE_URL': f'postgresql://postgres:upgrade%40fixture@127.0.0.1:{port}/postgres'}
        console = str(args.console.resolve())
        launcher = str(args.launcher.resolve())
        before = json.loads(command([console, 'database-check'], env=env))
        assert before['compatible'] and before['pending_versions'] == versions[1:]
        previous = output / 'previous-launcher'
        previous.write_text("#!/bin/sh\nprintf 'brocade 0.3.1\\n'\n")
        previous.chmod(0o700)
        backup = output / 'backup'
        command([launcher, 'upgrade-database', '--external-db', '--previous-launcher', str(previous),
                 '--console-bin', console, '--backup-dir', str(backup),
                 '--data-dir', str(output / 'data'), '--cache-dir', str(output / 'cache')], env=env)
        assert (backup / 'database-complete').is_file() and (backup / 'migration-started').is_file()
        listing = command(['pg_restore', '--list', str(backup / 'database.dump')])
        assert any('Compression:' in line and 'gzip' in line for line in listing.splitlines())
        after = json.loads(command([console, 'database-check'], env=env))
        assert after['compatible'] and after['installed_versions'] == versions and not after['pending_versions']
        assert sql("SELECT site_name FROM control_state") == 'preserved upgrade fixture'
        assert sql("SELECT encode(checksum,'hex') FROM _sqlx_migrations WHERE version=1") == checksum
        assert sql('SELECT count(*) FROM binary_releases') == '0'
        sql('CREATE DATABASE restored')
        restore_env = {**os.environ, 'PGHOST': '127.0.0.1', 'PGPORT': port, 'PGUSER': 'postgres',
                       'PGPASSWORD': 'upgrade@fixture'}
        command(['pg_restore', '--exit-on-error', '--dbname', 'restored', str(backup / 'database.dump')], env=restore_env)
        assert sql('SELECT site_name FROM control_state', 'restored') == 'preserved upgrade fixture'
        assert sql('SELECT string_agg(version::text,\',\' ORDER BY version) FROM _sqlx_migrations', 'restored') == '1'
        # Inject a real DDL failure after the first ALTER in 0002. The whole migration must roll
        # back, while the upgrader retains its backup and uncertainty marker for the installer.
        sql('CREATE TABLE node_traffic_limit (injected BOOLEAN)', 'restored')
        failure_backup = output / 'failed-migration-backup'
        failed_env = {**env, 'DATABASE_URL': env['DATABASE_URL'].rsplit('/', 1)[0] + '/restored'}
        failed = subprocess.run([launcher, 'upgrade-database', '--external-db',
                                 '--previous-launcher', str(previous), '--console-bin', console,
                                 '--backup-dir', str(failure_backup), '--data-dir', str(output / 'data'),
                                 '--cache-dir', str(output / 'cache')], env=failed_env,
                                capture_output=True, text=True, timeout=240)
        assert failed.returncode != 0
        assert (failure_backup / 'migration-started').is_file()
        assert (failure_backup / 'database.dump').stat().st_size > 0
        assert not (failure_backup / 'database-complete').exists()
        assert sql('SELECT string_agg(version::text,\',\' ORDER BY version) FROM _sqlx_migrations', 'restored') == '1'
        assert sql("SELECT count(*) FROM information_schema.columns WHERE table_name='forwarding_node_state' AND column_name='rule_results'", 'restored') == '0'
        result = {'passed': True, 'source': 'official v0.3.1 exact schema', 'installed_before': [1],
                  'installed_after': versions, 'receipt_preserved': True, 'backup_restored': True,
                  'failed_migration_rolled_back': True, 'gzip_compression_verified': True,
                  'fleet_release_created': False}
        (output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result))
    finally:
        subprocess.run(['docker', 'rm', '-f', container], stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, timeout=30, check=False)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--console', type=Path, required=True)
    parser.add_argument('--launcher', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    run(parser.parse_args())
