"""CVM maintenance boundary, exercised against an exclusively owned PostgreSQL container."""
import importlib.util
import os
from pathlib import Path
import re
import subprocess
import time
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('cvm_cleanup', ROOT / 'scripts/cvm_cleanup.py')
cleanup = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cleanup)


class WhitelistTests(unittest.TestCase):
    def test_whitelist_covers_the_current_cvm_schema_and_nothing_else(self):
        source = (ROOT / 'crates/brocade-store/migrations/0005_node_xray_enabled.sql').read_text()
        names = set(re.findall(r'CREATE TABLE (cvm_\w+)', source))
        self.assertEqual(len(names), 33)
        self.assertEqual(names, cleanup.TABLES)
        self.assertNotIn('users', cleanup.TABLES)
        self.assertNotIn('_sqlx_migrations', cleanup.TABLES)


@unittest.skipUnless(os.environ.get('BROCADE_RUN_PG_TESTS') == '1', 'requires an isolated Docker PostgreSQL')
class DatabaseTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.container = subprocess.check_output(
            ['docker', 'run', '--rm', '-d', '--label', 'brocade.test=cvm-cleanup',
             '-e', 'POSTGRES_PASSWORD=fixture-password', '-p', '127.0.0.1::5432',
             'postgres:16-alpine'], text=True,
        ).strip()
        try:
            port = subprocess.check_output(['docker', 'port', cls.container, '5432/tcp'], text=True).strip().rsplit(':', 1)[1]
            cls.url = f'postgresql://postgres:fixture-password@127.0.0.1:{port}/postgres'
            deadline = time.monotonic() + 30
            while True:
                try:
                    cleanup.query(cls.url, "SELECT '{}'::jsonb;")
                    break
                except cleanup.CleanupError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.2)
        except BaseException:
            subprocess.run(['docker', 'rm', '-f', cls.container], capture_output=True, timeout=20)
            raise

    @classmethod
    def tearDownClass(cls):
        subprocess.run(['docker', 'rm', '-f', cls.container], check=True, capture_output=True, timeout=20)

    def sql(self, source):
        return cleanup.query(self.url, source + "\nSELECT '{}'::jsonb;")

    def setUp(self):
        # The database above is created exclusively for this test class.
        self.sql("""
DROP SCHEMA public CASCADE;
CREATE SCHEMA public;
CREATE TABLE users(id text PRIMARY KEY, quota bigint NOT NULL);
CREATE TABLE settings(id text PRIMARY KEY, value text NOT NULL);
CREATE TABLE ordinary_usage(id text PRIMARY KEY, bytes bigint NOT NULL);
CREATE TABLE cvm_instances(id text PRIMARY KEY, user_id text REFERENCES users(id));
CREATE TABLE cvm_volumes(id text PRIMARY KEY, instance_id text REFERENCES cvm_instances(id));
INSERT INTO users VALUES ('owner',12345);
INSERT INTO settings VALUES ('secret-fixture','preserve-this-value');
INSERT INTO ordinary_usage VALUES ('usage',987654);
INSERT INTO cvm_instances VALUES ('experiment','owner');
INSERT INTO cvm_volumes VALUES ('disk','experiment');
""")

    def test_cleanup_preserves_all_non_cvm_rows_and_immutable_delete_rules(self):
        self.sql("""
CREATE FUNCTION immutable_cvm() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'immutable'; END $$;
CREATE TRIGGER immutable_cvm BEFORE DELETE ON cvm_instances
FOR EACH ROW EXECUTE FUNCTION immutable_cvm();
""")
        before = cleanup.inventory(self.url)
        all_before = cleanup.query(self.url, 'SELECT jsonb_build_object(\'protected\',' + cleanup.fingerprint(before['preserved_tables']) + ');')
        result = cleanup.apply(self.url, before)
        self.assertTrue(all(row[0] == 0 for row in result['cvm'].values()))
        after = cleanup.inventory(self.url)
        self.assertEqual(before['fingerprints']['preserved'], after['fingerprints']['preserved'])
        self.assertEqual(before['preserved_tables'], ['ordinary_usage', 'settings', 'users'])
        all_after = cleanup.query(self.url, 'SELECT jsonb_build_object(\'protected\',' + cleanup.fingerprint(after['preserved_tables']) + ');')
        self.assertEqual(all_before, all_after)

    def test_changed_inventory_is_refused_without_deleting_anything(self):
        before = cleanup.inventory(self.url)
        self.sql("INSERT INTO cvm_instances VALUES ('new-work','owner');")
        with self.assertRaises(cleanup.CleanupError):
            cleanup.apply(self.url, before)
        after = cleanup.inventory(self.url)
        self.assertEqual(after['fingerprints']['cvm']['cvm_instances'][0], 2)
        self.assertEqual(after['fingerprints']['cvm']['cvm_volumes'][0], 1)
        self.assertEqual(before['fingerprints']['preserved'], after['fingerprints']['preserved'])

    def test_backup_contains_only_the_reviewed_cvm_tables(self):
        before = cleanup.inventory(self.url)
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'cvm.dump'
            receipt = cleanup.backup(self.url, before['cvm_tables'], path)
            self.assertGreater(receipt['bytes'], 0)
            self.assertEqual(len(receipt['sha256']), 64)
            listing = subprocess.check_output(['pg_restore', '--list', str(path)], text=True)
            self.assertIn('TABLE DATA public cvm_instances', listing)
            self.assertIn('TABLE DATA public cvm_volumes', listing)
            self.assertNotIn('TABLE DATA public users', listing)
            self.assertNotIn('TABLE DATA public ordinary_usage', listing)
        self.assertEqual(before, cleanup.inventory(self.url))

    def test_non_cvm_foreign_reference_never_expands_cleanup_scope(self):
        self.sql("CREATE TABLE retained_links(id text REFERENCES cvm_instances(id)); INSERT INTO retained_links VALUES ('experiment');")
        before = cleanup.inventory(self.url)
        self.assertTrue(before['blockers']['external_references'])
        with self.assertRaises(cleanup.CleanupError):
            cleanup.apply(self.url, before)
        self.assertEqual(before, cleanup.inventory(self.url))

    def test_unreviewed_cvm_table_is_not_cleaned_by_prefix(self):
        self.sql("CREATE TABLE cvm_unreviewed(id int); INSERT INTO cvm_unreviewed VALUES (1);")
        with self.assertRaisesRegex(cleanup.CleanupError, 'unreviewed CVM tables'):
            cleanup.inventory(self.url)

    def test_truncate_trigger_cannot_mutate_shared_tables(self):
        self.sql("""
CREATE FUNCTION mutate_shared() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN DELETE FROM settings; RETURN NULL; END $$;
CREATE TRIGGER mutate_shared AFTER TRUNCATE ON cvm_instances
EXECUTE FUNCTION mutate_shared();
""")
        before = cleanup.inventory(self.url)
        self.assertTrue(before['blockers']['truncate_triggers'])
        with self.assertRaises(cleanup.CleanupError):
            cleanup.apply(self.url, before)
        self.assertEqual(before, cleanup.inventory(self.url))


if __name__ == '__main__':
    unittest.main()
