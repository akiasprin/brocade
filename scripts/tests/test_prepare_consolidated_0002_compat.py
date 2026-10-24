import hashlib
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("consolidation", ROOT / "scripts/prepare-consolidated-0002-compat.py")
COMPAT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COMPAT)


class ConsolidatedMigrationCompatibility(unittest.TestCase):
    def setUp(self):
        self.migration = (ROOT / "crates/brocade-store/migrations/0002_forwarding_rule_results.sql").read_bytes()

    def test_default_converts_deployed_three_version_history_with_only_missing_ddl(self):
        sql = COMPAT.generate(self.migration)
        delta = self.migration[self.migration.index(COMPAT.RETIREMENT_MARKER):].decode()
        self.assertEqual(sql.count(delta), 1)
        self.assertNotIn("ADD COLUMN target_strategy", sql)
        self.assertIn(COMPAT.THIRD, sql)
        positions = [sql.index(marker) for marker in [
            "DO $guard$", "before-0002-consolidation", delta, "DO $verify$",
            "UPDATE _sqlx_migrations", "DELETE FROM _sqlx_migrations", "DO $receipt$", "COMMIT;",
        ]]
        self.assertEqual(positions, sorted(positions))
        self.assertNotIn("@", sql)

    def test_two_version_history_also_installs_target_strategy(self):
        sql = COMPAT.generate(self.migration, with_target_strategy=False)
        delta = self.migration[self.migration.index(COMPAT.STRATEGY_MARKER):].decode()
        self.assertEqual(sql.count(delta), 1)
        self.assertIn("(SELECT count(*) FROM _sqlx_migrations) <> 2", sql)
        self.assertNotIn(COMPAT.THIRD, sql)

    def test_deployed_fixtures_keep_the_exact_reviewed_checksums(self):
        fixtures = ROOT / "crates/brocade-store/tests/fixtures/migrations"
        for name, checksum in [
            ("deployed_0002_forwarding_rule_results.sql", COMPAT.SECOND),
            ("deployed_0003_external_outbound_target_strategy.sql", COMPAT.THIRD),
        ]:
            with self.subTest(name=name):
                self.assertEqual(hashlib.sha384((fixtures / name).read_bytes()).hexdigest(), checksum)

    def test_rejects_changed_deployed_sql_and_unreviewed_target(self):
        for before, after, error in [
            (b"-- Legacy receipts", b"-- Changed receipts", "prefix"),
            (b"ADD COLUMN target_strategy TEXT", b"ADD COLUMN target_strategy VARCHAR", "target-strategy"),
            (b"Retire user-wide forwarding caps", b"Retire shared forwarding caps", "target"),
        ]:
            with self.subTest(error=error), self.assertRaisesRegex(ValueError, error):
                COMPAT.generate(self.migration.replace(before, after))

    def test_rejects_missing_and_duplicate_boundaries(self):
        for marker in (COMPAT.STRATEGY_MARKER, COMPAT.RETIREMENT_MARKER):
            for changed in (self.migration.replace(marker, b""), self.migration + marker):
                with self.subTest(marker=marker), self.assertRaisesRegex(ValueError, "boundary"):
                    COMPAT.generate(changed)


if __name__ == "__main__":
    unittest.main()
