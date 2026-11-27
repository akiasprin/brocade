"""Known-old 0009 rehearsal. BROCADE_PG_BIN opts in using installed PG tools."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / 'prepare-cvm-stack-0009-compat.py'
SPEC = importlib.util.spec_from_file_location('prepare_cube_stack_compat', SCRIPT)
compat = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(compat)


class GeneratorTests(unittest.TestCase):
    def test_only_exact_baseline_and_candidate_generate_sql(self):
        migrations = compat.migration_inputs()
        sql = compat.generate(migrations, 'a' * 64)
        self.assertIn(compat.OLD, sql)
        self.assertIn(compat.TARGET, sql)
        self.assertNotRegex(sql, r'@[A-Z_0-9]+@')
        self.assertNotIn('TRUNCATE TABLE\n    cvm_host_policy_events', sql)
        for index, side in ((0, 2), (8, 1), (8, 2)):
            with self.subTest(version=index + 1, side=side):
                changed = migrations.copy()
                entry = list(changed[index])
                entry[side] += b'\n'
                changed[index] = tuple(entry)
                with self.assertRaisesRegex(ValueError, 'unrecognized baseline or target'):
                    compat.generate(changed, 'a' * 64)

    def test_tool_requires_restore_attestation_and_backup_file(self):
        with tempfile.TemporaryDirectory() as work:
            output = Path(work) / 'convert.sql'
            command = [sys.executable, str(SCRIPT), '--backup', str(output), '--output', str(output)]
            for flags, failure in (([], b'restore and verify'), (['--backup-verified'], b'nonempty')):
                with self.subTest(flags=flags):
                    result = subprocess.run(command + flags, capture_output=True, timeout=30)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(failure, result.stderr)
                    self.assertFalse(output.exists())


RECEIPTS = """
CREATE TABLE _sqlx_migrations (
    version bigint PRIMARY KEY, description text NOT NULL, installed_on timestamptz NOT NULL DEFAULT now(),
    success boolean NOT NULL, checksum bytea NOT NULL, execution_time bigint NOT NULL
);
"""


def payload(*, install: bool) -> str:
    data = {
        'contract': 'brocade-cvm-stack-fresh-install-v1' if install else 'brocade-cvm-stack-cold-pair-v1',
        'architecture': 'x86_64',
        'storage_backend': 'plain' if install else 'xfs',
        'target_sha256': 'a' * 64,
        'target_bytes': 24576,
        'target_cubelet_sha256': 'b' * 64,
        'target_shim_sha256': 'c' * 64,
        'expires_at': 1700000000,
    }
    if not install:
        data.update(source_sha256='d' * 64, source_cubelet_sha256='e' * 64, source_shim_sha256='f' * 64)
    # The SHA256 must hash the exact stored bytes, including noncanonical whitespace.
    return json.dumps(data, indent=2) + '\n'


@unittest.skipUnless(os.environ.get('BROCADE_PG_BIN'), 'set BROCADE_PG_BIN to an installed PostgreSQL bin directory')
class IsolatedPostgresTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        work = tempfile.TemporaryDirectory(prefix='cvm-0009-')
        cls.addClassCleanup(work.cleanup)
        cls.work = Path(work.name)
        cls.bin = Path(os.environ['BROCADE_PG_BIN']).resolve(strict=True)
        for name in ('initdb', 'pg_ctl', 'psql', 'pg_dump', 'pg_restore'):
            if not (cls.bin / name).is_file():
                raise RuntimeError(f'missing PostgreSQL tool {name}')
        cls.env = {key: value for key, value in os.environ.items() if not key.startswith('PG')}
        cls.env.update(PGHOST=str(cls.work), PGPORT='5432', PGUSER='rehearsal', PGCONNECT_TIMEOUT='5', PGTZ='UTC')
        cls.data = str(cls.work / 'cluster')
        cls.tool('initdb', '-D', cls.data, '-U', 'rehearsal', '--auth=trust', '--no-locale', '--encoding=UTF8')
        cls.addClassCleanup(cls.stop_cluster)
        cls.tool(
            'pg_ctl',
            '-D',
            cls.data,
            '-l',
            str(cls.work / 'postgres.log'),
            '-o',
            f"-h '' -k {shlex.quote(str(cls.work))}",
            '-w',
            'start',
        )
        migrations = compat.migration_inputs()
        for db, index in (('old', 1), ('fresh', 2)):
            cls.psql('postgres', f'CREATE DATABASE {db};')
            cls.psql(db, RECEIPTS)
            for version, entry in enumerate(migrations, start=1):
                data = entry[index]
                checksum = hashlib.sha384(data).hexdigest()
                description = entry[0].stem.split('_', 1)[1].replace('_', ' ')
                # SQLx applies each migration and records its receipt in the same transaction.
                cls.psql(
                    db,
                    f"""BEGIN;
{data.decode()}
INSERT INTO _sqlx_migrations VALUES ({version},'{description}','2023-01-01T00:00:00Z',true,decode('{checksum}','hex'),23);
COMMIT;
""",
                )

        cls.psql(
            'old',
            """
INSERT INTO tenants (id,name) VALUES ('test','test');
INSERT INTO nodes (id,tenant_id,name,overlay_addr,wg_private_key,wg_public_key,wg_listen_port,dns_kind)
VALUES ('host','test','host','192.0.2.1','fixture-private','fixture-public',1025,'system');
INSERT INTO cvm_stack_observations (node_id,installable) VALUES ('host','plain');
""",
        )
        cls.cases = (('succeeded', True), ('recovered', False), ('failed', False), ('cancelled', True))
        for i, (status, install) in enumerate(cls.cases, start=1):
            # This fixture models admitted envelopes; neither the archive nor this tool revalidates signatures.
            cls.psql(
                'old',
                f"""
INSERT INTO cvm_stack_releases (node_id,tenant_id,artifact,status,expires_at,roots_sha256,error)
VALUES ('host','test',jsonb_build_object('payload',$payload${payload(install=install)}$payload$,'signature',repeat('e',128)),
    '{status}',1690000000+{i},repeat('1',64),'historical');
INSERT INTO cvm_stack_release_events (release_id,actor_kind,actor_id,status,created_at)
VALUES ({i},'admin','operator','preparing','2023-01-0{i}T04:05:06Z'),
       ({i},'admin','operator','approved','2023-02-0{i}T04:05:06Z'),
       ({i},'agent','host','{status}','2023-03-0{i}T04:05:06Z');
""",
            )
        cls.backup = cls.work / 'old.dump'
        cls.tool('pg_dump', '-Fc', '-f', str(cls.backup), '-d', 'old')
        cls.psql('postgres', 'CREATE DATABASE restored;')
        cls.tool('pg_restore', '--exit-on-error', '-d', 'restored', str(cls.backup))
        cls.assert_snapshots_match(cls.snapshot('old'), cls.snapshot('restored'))
        cls.sql = cls.work / 'convert.sql'
        subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                '--backup',
                str(cls.backup),
                '--backup-verified',
                '--pg-restore',
                str(cls.bin / 'pg_restore'),
                '--output',
                str(cls.sql),
            ],
            check=True,
            env=cls.env,
            timeout=120,
        )

    @classmethod
    def stop_cluster(cls):
        if (Path(cls.data) / 'postmaster.pid').is_file():
            cls.tool('pg_ctl', '-D', cls.data, '-m', 'immediate', '-w', 'stop')

    @classmethod
    def tool(cls, name, *args, text=None, check=True):
        return subprocess.run(
            [str(cls.bin / name), *args],
            input=text,
            text=True,
            capture_output=True,
            env=cls.env,
            check=check,
            timeout=120,
        )

    @classmethod
    def psql(cls, db, text, *, check=True):
        result = cls.tool('psql', '-XqAt', '-v', 'ON_ERROR_STOP=1', '-d', db, '-f', '-', text=text, check=False)
        if check and result.returncode != 0:
            raise AssertionError(result.stderr)
        return result

    @classmethod
    def snapshot(cls, db):
        sql = 'SELECT row_to_json(t) FROM {} t ORDER BY {};'
        return {
            table: cls.psql(db, sql.format(table, order)).stdout
            for table, order in (
                ('cvm_stack_releases', 'id'),
                ('cvm_stack_release_events', 'id'),
                ('cvm_stack_observations', 'node_id'),
                ('_sqlx_migrations', 'version'),
                ('nodes', 'id'),
            )
        }

    @staticmethod
    def assert_snapshots_match(before, after):
        if before != after:
            raise AssertionError('fixture archive did not preserve all test rows')

    def test_conversion_matches_fresh_schema_preserves_history_and_clears_observations(self):
        self.psql('postgres', 'CREATE DATABASE converted TEMPLATE restored;')
        before = self.snapshot('converted')
        sql = self.sql.read_text()
        self.assertIn('4', self.psql('converted', sql).stdout.splitlines())
        after = self.snapshot('converted')
        self.assertEqual(after['nodes'], before['nodes'])
        self.assertEqual(after['cvm_stack_release_events'], before['cvm_stack_release_events'])
        self.assertIn('"status":"approved"', after['cvm_stack_release_events'])
        self.assertNotEqual(before['cvm_stack_observations'], '')
        self.assertEqual(after['cvm_stack_observations'], '')
        for i, (legacy, converted) in enumerate(
            zip(before['cvm_stack_releases'].splitlines(), after['cvm_stack_releases'].splitlines(), strict=True),
            start=1,
        ):
            legacy = json.loads(legacy)
            converted = json.loads(converted)
            _, install = self.cases[i - 1]
            identity = lambda bundle, cubelet, shim: {
                'bundle_sha256': bundle * 64,
                'cubelet_sha256': cubelet * 64,
                'shim_sha256': shim * 64,
                'storage_backend': 'plain' if install else 'xfs',
            }
            self.assertEqual(
                converted.pop('artifact'),
                {
                    'pair_id': hashlib.sha256(payload(install=install).encode()).hexdigest(),
                    'source': None if install else identity('d', 'e', 'f'),
                    'target': identity('a', 'b', 'c'),
                },
            )
            self.assertEqual(converted.pop('created_at'), f'2023-01-0{i}T04:05:06+00:00')
            old_deadline = legacy.pop('expires_at')
            legacy.pop('artifact')
            self.assertEqual(converted.pop('start_deadline_at'), old_deadline)
            self.assertEqual(converted, legacy)
        old_receipts, new_receipts = (
            list(map(json.loads, item['_sqlx_migrations'].splitlines())) for item in (before, after)
        )
        old_receipts[8]['checksum'] = '\\x' + compat.TARGET
        self.assertEqual(new_receipts, old_receipts)

        def schema(db):
            dump = self.tool('pg_dump', '--schema-only', '--no-owner', '--no-privileges', '-d', db).stdout
            return re.sub(r'^\\(?:un)?restrict .*\n', '', dump, flags=re.MULTILINE)

        # Restore also reparses DDL outside Cube Stack. Round-trip both schemas
        # so that pg_dump's equivalent parenthesization and aliases compare alike.
        fresh_archive = self.work / 'fresh-schema.dump'
        self.tool('pg_dump', '-Fc', '--schema-only', '-f', str(fresh_archive), '-d', 'fresh')
        self.psql('postgres', 'CREATE DATABASE fresh_restored;')
        self.tool('pg_restore', '--exit-on-error', '-d', 'fresh_restored', str(fresh_archive))
        self.assertEqual(schema('converted'), schema('fresh_restored'))
        self.assertNotEqual(self.psql('converted', sql, check=False).returncode, 0)
        self.assertEqual(self.snapshot('converted'), after)
        new_id = self.psql(
            'converted',
            """
INSERT INTO cvm_stack_releases (node_id,tenant_id,artifact,status,created_at,start_deadline_at)
SELECT 'host','test','{"payload":"new manifest","signature":"retained bytes"}', 'preparing', t,
       floor(extract(epoch FROM t))::bigint + 18000 FROM (SELECT clock_timestamp() AS t) db_clock
RETURNING id, start_deadline_at - floor(extract(epoch FROM created_at))::bigint;
""",
        ).stdout.strip()
        self.assertEqual(new_id, '5|18000')
        for status in ('preparing', 'ready', 'applying', 'blocked', 'approved'):
            with self.subTest(status=status):
                self.assertNotEqual(
                    self.psql(
                        'converted',
                        f"""
INSERT INTO cvm_stack_releases (node_id,tenant_id,artifact,status,start_deadline_at,roots_sha256)
VALUES ('host','test','{{}}','{status}',1700000000,repeat('1',64));
""",
                        check=False,
                    ).returncode,
                    0,
                )

    def test_unknown_dirty_or_live_input_and_verification_failure_roll_back(self):
        cases = [
            (
                'checksum',
                "UPDATE _sqlx_migrations SET checksum='bad' WHERE version=9;",
                'unrecognized prerelease Cube Stack migration history',
            ),
            (
                'schema',
                'ALTER INDEX cvm_stack_releases_open_host RENAME TO drifted;',
                'unexpected prerelease Cube Stack schema',
            ),
            (
                'artifact',
                "UPDATE cvm_stack_releases SET artifact = '{}' WHERE id=1;",
                'invalid legacy signed Cube Stack artifact',
            ),
            (
                'bad_json',
                "UPDATE cvm_stack_releases SET artifact = jsonb_build_object('payload','broken-secret-marker','signature',repeat('e',128)) WHERE id=1;",
                'invalid legacy signed Cube Stack payload',
            ),
            (
                'audit',
                "DELETE FROM cvm_stack_release_events WHERE release_id=1 AND status='preparing';",
                'lacks its initial preparation audit event',
            ),
            ('postverify', '', 'audit or observation verification failed'),
        ] + [
            (
                status,
                f"UPDATE cvm_stack_releases SET status='{status}' WHERE id=1;",
                'live legacy Cube Stack release remains',
            )
            for status in ('preparing', 'ready', 'approved', 'applying', 'blocked')
        ]
        for index, (kind, mutation, failure) in enumerate(cases):
            with self.subTest(kind=kind):
                db = f'negative_{index}'
                self.psql('postgres', f'CREATE DATABASE {db} TEMPLATE restored;')
                self.psql(db, mutation)
                before = self.snapshot(db)
                sql = self.sql.read_text()
                if kind == 'postverify':
                    sql = sql.replace('DO $verify$', 'DELETE FROM public.cvm_stack_release_events;\nDO $verify$', 1)
                result = self.psql(db, sql, check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(failure, result.stderr)
                self.assertNotIn('broken-secret-marker', result.stderr)
                self.assertEqual(self.snapshot(db), before)
                self.assertEqual(
                    self.psql(db, 'SELECT count(*) FROM cvm_stack_releases WHERE expires_at > 0;').stdout.strip(), '4'
                )
