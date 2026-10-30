import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/prepare-consolidated-0003-compat.py"
SPEC = importlib.util.spec_from_file_location("consolidate_unlock", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ConsolidatedUnlockMigrationTests(unittest.TestCase):
    def test_accepts_only_the_reviewed_target_and_exact_history(self):
        migration = (ROOT / "crates/brocade-store/migrations/0003_unlock_probe_schedule.sql").read_bytes()
        sql = MODULE.generate(migration)
        self.assertIn(MODULE.OLD_THIRD, sql)
        self.assertIn(MODULE.OLD_FOURTH, sql)
        self.assertIn(MODULE.OLD_FIFTH, sql)
        self.assertIn(MODULE.TARGET, sql)
        self.assertIn("DELETE FROM _sqlx_migrations WHERE version IN (4,5)", sql)
        self.assertIn("unexpected unlock schema", sql)
        with self.assertRaisesRegex(ValueError, "unreviewed"):
            MODULE.generate(migration + b"\nSELECT 1;\n")
        with self.assertRaisesRegex(ValueError, "prefix"):
            MODULE.generate(migration.replace(b"CREATE TABLE unlock_probe_jobs", b"CREATE TABLE changed_jobs", 1))

    def test_executes_notification_ddl_before_changing_the_receipt(self):
        migration = (ROOT / "crates/brocade-store/migrations/0003_unlock_probe_schedule.sql").read_bytes()
        notifications = migration.split(MODULE.CHANNEL_BOUNDARY)[1].decode()
        for consolidated in [False, True]:
            sql = MODULE.generate(migration, from_consolidated=consolidated)
            self.assertIn(notifications, sql)
            self.assertLess(sql.index("CREATE FUNCTION notify_agent_channel_change()"),
                            sql.index("UPDATE _sqlx_migrations SET checksum="))
            self.assertIn("tgenabled='O'", sql)
            self.assertLess(sql.index("CREATE TABLE user_cycle_resets"),
                            sql.index("UPDATE _sqlx_migrations SET checksum="))

    def test_previous_consolidated_mode_is_explicit_and_precisely_pinned(self):
        migration = (ROOT / "crates/brocade-store/migrations/0003_unlock_probe_schedule.sql").read_bytes()
        previous = (ROOT / "crates/brocade-store/tests/fixtures/migrations/deployed_0003_consolidated_unlock.sql").read_bytes()
        self.assertEqual(MODULE.hashlib.sha384(previous).hexdigest(), MODULE.OLD_CONSOLIDATED)
        sql = MODULE.generate(migration, from_consolidated=True)
        self.assertIn(MODULE.OLD_CONSOLIDATED, sql)
        self.assertNotIn(MODULE.OLD_FOURTH, sql)
        self.assertNotIn("DELETE FROM _sqlx_migrations WHERE version IN (4,5)", sql)


if __name__ == "__main__":
    unittest.main()
