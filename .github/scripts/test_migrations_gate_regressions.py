"""Regression tests for base-aware migration numbering and discovery."""

import hashlib
import pathlib
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
GATE_SCRIPT = ".github/scripts/migrations_gate.py"


class MigrationGateRegressionTests(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp_dir.name)
        self.migrations = self.root / "migrations"
        self.migrations.mkdir()

    def tearDown(self):
        self.temp_dir.cleanup()

    def write_migration(self, filename, content="SELECT 1;\n"):
        path = self.migrations / filename
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return path

    def write_manifest(self, entries):
        lines = []
        for filename, target in entries:
            digest = hashlib.sha256((self.migrations / filename).read_bytes()).hexdigest()
            lines.extend(("[[migration]]", f'file = "{filename}"', f'target = "{target}"', f'sha256 = "{digest}"', ""))
        (self.migrations / "manifest.toml").write_text("\n".join(lines), encoding="utf-8")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, check=True, text=True, capture_output=True)

    def run_gate(self, *args):
        return subprocess.run(
            ["python3", GATE_SCRIPT, "--root", str(self.root), *args],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def make_base(self, filenames):
        self.git("init", "-q")
        self.git("config", "user.email", "migration-gate-test@example.invalid")
        self.git("config", "user.name", "Migration Gate Test")
        for filename in filenames:
            self.write_migration(filename)
        self.write_manifest([(filename, "app") for filename in filenames])
        self.git("add", "migrations")
        self.git("commit", "-qm", "base migrations")
        return self.git("rev-parse", "HEAD").stdout.strip()

    def test_new_number_must_exceed_base_maximum(self):
        for added in ("0000_first.sql", "0002_inserted.sql"):
            with self.subTest(added=added):
                with tempfile.TemporaryDirectory() as temp:
                    self.root = pathlib.Path(temp)
                    self.migrations = self.root / "migrations"
                    self.migrations.mkdir()
                    base_files = ["0001_first.sql", "0003_third.sql"]
                    base_ref = self.make_base(base_files)
                    self.write_migration(added)
                    current_files = sorted(base_files + [added])
                    self.write_manifest([(filename, "app") for filename in current_files])

                    result = self.run_gate("--base-ref", base_ref)

                    self.assertEqual(1, result.returncode, result.stdout + result.stderr)
                    self.assertIn(f"migrations-gate: out-of-order: {added}: ", result.stdout)

    def test_new_number_must_be_next_after_base_maximum(self):
        base_ref = self.make_base(["0001_first.sql", "0003_third.sql"])
        self.write_migration("0005_gap.sql")
        self.write_manifest(
            [
                ("0001_first.sql", "app"),
                ("0003_third.sql", "app"),
                ("0005_gap.sql", "app"),
            ]
        )

        result = self.run_gate("--base-ref", base_ref)

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("migrations-gate: out-of-order: 0005_gap.sql: new migration number must be 0004", result.stdout)

    def test_applied_manifest_target_change_is_reported(self):
        base_ref = self.make_base(["0001_first.sql"])
        self.write_manifest([("0001_first.sql", "mirror")])

        result = self.run_gate("--base-ref", base_ref)

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("migrations-gate: applied-edited: 0001_first.sql: ", result.stdout)

    def test_malformed_base_manifest_is_a_usage_error(self):
        self.git("init", "-q")
        self.git("config", "user.email", "migration-gate-test@example.invalid")
        self.git("config", "user.name", "Migration Gate Test")
        self.write_migration("0001_first.sql")
        (self.migrations / "manifest.toml").write_text("[[migration]\\nfile =", encoding="utf-8")
        self.git("add", "migrations")
        self.git("commit", "-qm", "base with malformed manifest")
        base_ref = self.git("rev-parse", "HEAD").stdout.strip()
        self.write_manifest([("0001_first.sql", "app")])

        result = self.run_gate("--base-ref", base_ref)

        self.assertEqual(2, result.returncode, result.stdout + result.stderr)
        self.assertIn("cannot parse base migrations/manifest.toml", result.stderr)

    def test_unknown_base_ref_is_a_git_error(self):
        self.make_base(["0001_first.sql"])

        result = self.run_gate("--base-ref", "does-not-exist-xyz")

        output = result.stdout + result.stderr
        self.assertEqual(2, result.returncode, output)
        self.assertEqual("", result.stdout, output)
        self.assertIn(
            "git rev-parse --verify does-not-exist-xyz^{commit} failed",
            result.stderr,
            output,
        )
        self.assertNotIn("Traceback", output)

    def test_base_ref_outside_git_is_a_gate_error(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([("0001_first.sql", "app")])

        result = self.run_gate("--base-ref", "HEAD")

        output = result.stdout + result.stderr
        self.assertEqual(2, result.returncode, output)
        self.assertEqual("", result.stdout, output)
        self.assertIn("git rev-parse", result.stderr, output)
        self.assertNotIn("Traceback", output)

    def test_non_ascii_base_migration_drift_is_detected(self):
        filename = "0001_café.sql"
        base_ref = self.make_base([filename])
        self.write_migration(filename, "DROP TABLE users;\n")
        self.write_manifest([(filename, "app")])

        result = self.run_gate("--base-ref", base_ref)

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("migrations-gate: applied-edited: 0001_café.sql: ", result.stdout)

    def test_missing_base_manifest_is_tolerated(self):
        self.git("init", "-q")
        self.git("config", "user.email", "migration-gate-test@example.invalid")
        self.git("config", "user.name", "Migration Gate Test")
        self.write_migration("0001_first.sql")
        self.git("add", "migrations/0001_first.sql")
        self.git("commit", "-qm", "base migration without manifest")
        base_ref = self.git("rev-parse", "HEAD").stdout.strip()
        self.write_manifest([("0001_first.sql", "mirror")])

        result = self.run_gate("--base-ref", base_ref)

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_nested_base_sql_is_unlisted_without_manifest_drift_noise(self):
        self.git("init", "-q")
        self.git("config", "user.email", "migration-gate-test@example.invalid")
        self.git("config", "user.name", "Migration Gate Test")
        self.write_migration("0001_first.sql")
        self.write_migration("sub/0009_nested.sql")
        self.write_manifest([("0001_first.sql", "app")])
        self.git("add", "migrations")
        self.git("commit", "-qm", "base migrations")
        base_ref = self.git("rev-parse", "HEAD").stdout.strip()

        result = self.run_gate("--base-ref", base_ref)

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("migrations-gate: unlisted: sub/0009_nested.sql: ", result.stdout)
        self.assertNotIn("manifest target or checksum differs", result.stdout)

    def test_migration_directory_symlink_is_reported(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([("0001_first.sql", "app")])
        external = self.root / "external"
        external.mkdir()
        (external / "0009_hidden.sql").write_text("SELECT 9;\\n", encoding="utf-8")
        (self.migrations / "linked").symlink_to(external, target_is_directory=True)
        (self.migrations / "dangling").symlink_to(self.root / "missing", target_is_directory=True)

        result = self.run_gate()

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("migrations-gate: symlink: linked: ", result.stdout)
        self.assertIn("migrations-gate: symlink: dangling: ", result.stdout)

    def test_nested_base_number_does_not_raise_flat_numbering_floor(self):
        self.git("init", "-q")
        self.git("config", "user.email", "migration-gate-test@example.invalid")
        self.git("config", "user.name", "Migration Gate Test")
        self.write_migration("0001_first.sql")
        self.write_migration("sub/0099_nested.sql")
        self.write_manifest([("0001_first.sql", "app")])
        self.git("add", "migrations")
        self.git("commit", "-qm", "base migrations")
        base_ref = self.git("rev-parse", "HEAD").stdout.strip()
        self.write_migration("0002_next.sql")
        self.write_manifest([("0001_first.sql", "app"), ("0002_next.sql", "app")])

        result = self.run_gate("--base-ref", base_ref)

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertNotIn("out-of-order: 0002_next.sql", result.stdout)

    def test_unlisted_base_file_in_empty_manifest_has_no_manifest_drift_noise(self):
        self.git("init", "-q")
        self.git("config", "user.email", "migration-gate-test@example.invalid")
        self.git("config", "user.name", "Migration Gate Test")
        self.write_migration("0001_first.sql")
        (self.migrations / "manifest.toml").write_text("", encoding="utf-8")
        self.git("add", "migrations")
        self.git("commit", "-qm", "base with unlisted migration")
        base_ref = self.git("rev-parse", "HEAD").stdout.strip()

        result = self.run_gate("--base-ref", base_ref)

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("migrations-gate: unlisted: 0001_first.sql: ", result.stdout)
        self.assertNotIn("manifest target or checksum differs", result.stdout)

    def test_nested_and_case_variant_sql_are_not_invisible(self):
        self.write_migration("0001_first.sql")
        self.write_migration("sub/0009_nested.sql")
        self.write_migration("0002_UP.SQL")
        self.write_manifest([("0001_first.sql", "app")])

        result = self.run_gate()

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("migrations-gate: unlisted: 0002_UP.SQL: ", result.stdout)
        self.assertIn("migrations-gate: unlisted: sub/0009_nested.sql: ", result.stdout)


if __name__ == "__main__":
    unittest.main()
