import importlib.util
import os
from pathlib import Path
import subprocess
import time
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("compat", ROOT / "scripts/prepare-user-service-compat.py")
COMPAT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COMPAT)


class UserServiceCompatibility(unittest.TestCase):
    def setUp(self):
        self.migration = (ROOT / "crates/brocade-store/migrations/0002_forwarding_rule_results.sql").read_bytes()

    def test_exact_reviewed_delta_precedes_receipt_alignment(self):
        sql = COMPAT.generate(self.migration)
        delta = self.migration[self.migration.index(COMPAT.MARKER):].decode()
        self.assertEqual(sql.count(delta), 1)
        positions = [sql.index(marker) for marker in ["DO $guard$", delta, "DO $verify$", "UPDATE _sqlx_migrations", "DO $receipt$", "COMMIT;"]]
        self.assertEqual(positions, sorted(positions))
        self.assertIn(COMPAT.BASELINE, sql)
        self.assertIn(COMPAT.TARGET, sql)
        self.assertNotIn("@", sql)

    def test_rejects_changed_published_prefix(self):
        with self.assertRaisesRegex(ValueError, "prefix"):
            COMPAT.generate(b"-- changed\n" + self.migration)

    def test_rejects_unreviewed_target(self):
        with self.assertRaisesRegex(ValueError, "target"):
            COMPAT.generate(self.migration + b"\nSELECT 1;\n")

    def test_rejects_missing_or_ambiguous_boundary(self):
        for data in [b"", self.migration.replace(COMPAT.MARKER, b""), self.migration + COMPAT.MARKER]:
            with self.subTest(data_length=len(data)), self.assertRaisesRegex(ValueError, "boundary"):
                COMPAT.generate(data)


@unittest.skipUnless(os.environ.get("BROCADE_RUN_COMPAT_TESTS") == "1", "requires disposable Docker PostgreSQL")
class PostgreSQLCompatibility(unittest.TestCase):
    @classmethod
    def command(cls, *args, data=None):
        return subprocess.run(args, input=data, text=True, capture_output=True, timeout=60, check=True).stdout

    @classmethod
    def sql(cls, database, text, role="brocade"):
        return cls.command("docker", "exec", "-i", cls.container, "psql", "-XqAt", "-U", role,
                           "-d", database, "-v", "ON_ERROR_STOP=1", "-v", "app_role=brocade", data=text)

    @classmethod
    def setUpClass(cls):
        cls.container = "brocade-service-compat-" + uuid.uuid4().hex[:12]
        cls.command("docker", "run", "-d", "--name", cls.container, "--network", "none",
                    "--memory", "512m", "--pids-limit", "128", "-e", "POSTGRES_HOST_AUTH_METHOD=trust", "postgres:16-alpine")
        cls.addClassCleanup(cls.command, "docker", "rm", "-f", cls.container)
        for _ in range(100):
            try:
                cls.sql("postgres", "SELECT 1", role="postgres")
                break
            except subprocess.CalledProcessError:
                time.sleep(0.1)
        else:
            raise RuntimeError("disposable PostgreSQL did not start")
        # initdb briefly serves a temporary server before restarting the final one.
        for _ in range(100):
            logs = cls.command("docker", "logs", cls.container)
            if "PostgreSQL init process complete; ready for start up." in logs:
                break
            time.sleep(0.1)
        else:
            raise RuntimeError("disposable PostgreSQL initialization did not finish")
        for _ in range(100):
            try:
                cls.sql("postgres", "SELECT 1", role="postgres")
                break
            except subprocess.CalledProcessError:
                time.sleep(0.1)
        else:
            raise RuntimeError("final disposable PostgreSQL did not start")
        cls.sql("postgres", "CREATE ROLE brocade LOGIN", role="postgres")
        cls.migration = (ROOT / "crates/brocade-store/migrations/0002_forwarding_rule_results.sql").read_bytes()
        cls.compat = COMPAT.generate(cls.migration)

    def fixture(self, name, complete=False):
        self.sql("postgres", f"CREATE DATABASE {name} OWNER brocade", role="postgres")
        initial = (ROOT / "crates/brocade-store/migrations/0001_init.sql").read_text()
        second = self.migration.decode() if complete else self.migration[:self.migration.index(COMPAT.MARKER)].decode()
        checksum = COMPAT.TARGET if complete else COMPAT.BASELINE
        self.sql(name, "BEGIN;\n" + initial + "\n" + second + "\nCOMMIT;")
        self.sql(name, f"""CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL,
            installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL, checksum BYTEA NOT NULL,
            execution_time BIGINT NOT NULL);
            INSERT INTO _sqlx_migrations VALUES (1,'init','2026-01-01T00:00:00Z',true,decode('{COMPAT.INITIAL}','hex'),17),
                (2,'forwarding rule results','2026-02-01T00:00:00Z',true,decode('{checksum}','hex'),29);
            INSERT INTO tenants(id,name) VALUES ('test','preserved tenant');
            INSERT INTO users(tenant_id,id,uuid) VALUES ('test','kept','00000000-0000-0000-0000-000000000001');""")

    def schema(self, name):
        dump = self.command("docker", "exec", self.container, "pg_dump", "-U", "brocade", "-d", name,
                            "--schema-only", "--no-owner", "--no-privileges", "--schema=public")
        return "\n".join(line for line in dump.splitlines() if not line.startswith(("\\restrict", "\\unrestrict")))

    def test_manual_conversion_matches_fresh_schema_and_preserves_receipts(self):
        self.fixture("upgraded")
        self.fixture("fresh", complete=True)
        before = self.sql("upgraded", "SELECT to_jsonb(m) - 'checksum' FROM _sqlx_migrations m ORDER BY version")
        self.sql("upgraded", self.compat)
        self.assertEqual(self.schema("upgraded"), self.schema("fresh"))
        self.assertEqual(before, self.sql("upgraded", "SELECT to_jsonb(m) - 'checksum' FROM _sqlx_migrations m ORDER BY version"))
        self.assertEqual(self.sql("upgraded", "SELECT traffic_reset_day,service_starts_at,service_expires_at,service_suspended FROM users").strip(), "1|||f")
        with self.assertRaises(subprocess.CalledProcessError):
            self.sql("upgraded", self.compat)
        self.assertEqual(self.schema("upgraded"), self.schema("fresh"))

    def test_failure_after_ddl_rolls_back_everything(self):
        self.fixture("rollback")
        before = self.schema("rollback")
        injected = self.compat.replace("UPDATE _sqlx_migrations", "DO $$ BEGIN RAISE EXCEPTION 'injected verification failure'; END $$;\nUPDATE _sqlx_migrations")
        with self.assertRaises(subprocess.CalledProcessError):
            self.sql("rollback", injected)
        self.assertEqual(self.schema("rollback"), before)
        self.assertEqual(self.sql("rollback", "SELECT encode(checksum,'hex') FROM _sqlx_migrations WHERE version=2").strip(), COMPAT.BASELINE)

    def test_unknown_receipt_is_rejected_without_schema_changes(self):
        self.fixture("unknown")
        self.sql("unknown", "UPDATE _sqlx_migrations SET checksum=decode('00','hex') WHERE version=2")
        before = self.schema("unknown")
        with self.assertRaises(subprocess.CalledProcessError):
            self.sql("unknown", self.compat)
        self.assertEqual(self.schema("unknown"), before)


if __name__ == "__main__":
    unittest.main()
