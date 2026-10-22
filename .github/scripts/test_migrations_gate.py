"""Acceptance tests for the migrations manifest gate CLI."""

import hashlib
import pathlib
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
GATE_SCRIPT = ".github/scripts/migrations_gate.py"


class MigrationsGateTestCase(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp_dir.name)
        self.migrations = self.root / "migrations"
        self.migrations.mkdir()

    def tearDown(self):
        self.temp_dir.cleanup()

    def write_migration(self, name, content="-- migration\n"):
        path = self.migrations / name
        path.write_text(content, encoding="utf-8")
        return path

    def write_manifest(self, entries):
        lines = []
        for filename, target, checksum in entries:
            lines.extend(
                [
                    "[[migration]]",
                    f'file = "{filename}"',
                    f'target = "{target}"',
                    f'sha256 = "{checksum}"',
                    "",
                ]
            )
        (self.migrations / "manifest.toml").write_text(
            "\n".join(lines), encoding="utf-8"
        )

    def manifest_entry(self, filename, target="app"):
        content = (self.migrations / filename).read_bytes()
        return filename, target, hashlib.sha256(content).hexdigest()

    def run_gate(self, *args):
        return subprocess.run(
            ["python3", GATE_SCRIPT, "--root", str(self.root), *args],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def assert_violation(self, result, code, filename):
        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        lines = result.stdout.splitlines()
        self.assertEqual(1, len(lines), result.stdout)
        self.assertTrue(
            lines[0].startswith(f"migrations-gate: {code}: {filename}: "),
            result.stdout,
        )

    def git(self, *args):
        return subprocess.run(
            ["git", *args],
            cwd=self.root,
            check=True,
            text=True,
            capture_output=True,
        )

    def init_git_base(self):
        self.git("init", "-q")
        self.git("config", "user.email", "migration-gate-test@example.invalid")
        self.git("config", "user.name", "Migration Gate Test")
        self.git("add", "migrations")
        self.git("commit", "-qm", "base migrations")
        return self.git("rev-parse", "HEAD").stdout.strip()


class TestMigrationViolations(MigrationsGateTestCase):
    def test_unlisted_sql_file_is_reported(self):
        self.write_migration("0001_first.sql")
        self.write_migration("0002_unlisted.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql")])

        result = self.run_gate()

        self.assert_violation(result, "unlisted", "0002_unlisted.sql")
        self.assertIn(
            "migrations-gate: unlisted: 0002_unlisted.sql: "
            "SQL migration is missing from the manifest",
            result.stdout,
        )

    def test_missing_manifest_file_is_reported(self):
        self.write_manifest([("0001_missing.sql", "app", "0" * 64)])

        result = self.run_gate()

        self.assert_violation(result, "missing-file", "0001_missing.sql")
        self.assertIn(
            "migrations-gate: missing-file: 0001_missing.sql: "
            "manifest entry has no matching SQL file",
            result.stdout,
        )

    def test_duplicate_number_is_reported(self):
        self.write_migration("0001_first.sql")
        self.write_migration("0001_second.sql")
        self.write_manifest(
            [
                self.manifest_entry("0001_first.sql"),
                self.manifest_entry("0001_second.sql"),
            ]
        )

        self.assert_violation(
            self.run_gate(), "duplicate-number", "0001_second.sql"
        )

    def test_duplicate_entry_is_reported_once(self):
        self.write_migration("0001_first.sql")
        entry = self.manifest_entry("0001_first.sql")
        self.write_manifest([entry, entry])

        result = self.run_gate()

        self.assert_violation(result, "duplicate-entry", "0001_first.sql")
        self.assertIn("migration is listed more than once", result.stdout)

    def test_unshaped_filename_is_out_of_order_not_missing(self):
        self.write_migration("notes.sql")
        self.write_manifest([self.manifest_entry("notes.sql")])

        result = self.run_gate()

        self.assert_violation(result, "out-of-order", "notes.sql")
        self.assertIn(
            "filename must start with a four-digit number", result.stdout
        )

    def test_manifest_order_must_increase_by_four_digit_prefix(self):
        self.write_migration("0001_first.sql")
        self.write_migration("0002_second.sql")
        self.write_manifest(
            [
                self.manifest_entry("0002_second.sql"),
                self.manifest_entry("0001_first.sql"),
            ]
        )

        self.assert_violation(self.run_gate(), "out-of-order", "0001_first.sql")

    def test_non_app_or_mirror_target_is_reported(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql", "both")])

        self.assert_violation(self.run_gate(), "bad-target", "0001_first.sql")

    def test_sha256_mismatch_is_reported(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([("0001_first.sql", "app", "0" * 64)])

        result = self.run_gate()

        self.assert_violation(result, "checksum", "0001_first.sql")
        self.assertIn(
            "migrations-gate: checksum: 0001_first.sql: "
            "sha256 does not match the migration contents",
            result.stdout,
        )

    def test_symlinked_migration_is_reported(self):
        target = self.write_migration("0001_first.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql")])
        (self.migrations / "0002_second.sql").symlink_to(target)

        result = self.run_gate()

        self.assert_violation(result, "symlink", "0002_second.sql")
        self.assertIn(
            "migrations-gate: symlink: 0002_second.sql: "
            "migration files must not be symlinks",
            result.stdout,
        )


class TestMigrationDrift(MigrationsGateTestCase):
    def test_edit_to_base_migration_is_reported(self):
        self.write_migration("0001_first.sql", "SELECT 1;\n")
        self.write_manifest([self.manifest_entry("0001_first.sql")])
        base_ref = self.init_git_base()

        self.write_migration("0001_first.sql", "SELECT 2;\n")
        self.write_manifest([self.manifest_entry("0001_first.sql")])

        self.assert_violation(
            self.run_gate("--base-ref", base_ref), "applied-edited", "0001_first.sql"
        )

    def test_deleted_base_migration_is_reported(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql")])
        base_ref = self.init_git_base()

        (self.migrations / "0001_first.sql").unlink()
        self.write_manifest([])

        self.assert_violation(
            self.run_gate("--base-ref", base_ref), "applied-removed", "0001_first.sql"
        )

    def test_renamed_base_migration_is_reported_as_removed(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql")])
        base_ref = self.init_git_base()

        (self.migrations / "0001_first.sql").rename(self.migrations / "0002_renamed.sql")
        self.write_manifest([self.manifest_entry("0002_renamed.sql")])

        self.assert_violation(
            self.run_gate("--base-ref", base_ref), "applied-removed", "0001_first.sql"
        )

    def test_correctly_numbered_new_migration_passes(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql")])
        base_ref = self.init_git_base()

        self.write_migration("0002_second.sql")
        self.write_manifest(
            [
                self.manifest_entry("0001_first.sql"),
                self.manifest_entry("0002_second.sql"),
            ]
        )

        result = self.run_gate("--base-ref", base_ref)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)


class TestMigrationGateContract(MigrationsGateTestCase):
    def assert_cli_error(self, result):
        output = result.stdout + result.stderr
        self.assertEqual(2, result.returncode, output)
        self.assertNotIn("can't open file", output)

    def test_clean_fixture_exits_zero(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql", "mirror")])

        result = self.run_gate()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_unknown_argument_is_a_usage_error(self):
        result = self.run_gate("--not-a-real-option")

        self.assert_cli_error(result)

    def test_malformed_manifest_is_a_parse_error(self):
        (self.migrations / "manifest.toml").write_text(
            "[[migration]\nfile =\n", encoding="utf-8"
        )

        result = self.run_gate()

        self.assert_cli_error(result)

    def test_nested_manifest_path_is_a_usage_error(self):
        self.write_manifest([("sub/0001_x.sql", "app", "0" * 64)])

        result = self.run_gate()

        output = result.stdout + result.stderr
        self.assertEqual(2, result.returncode, output)
        self.assertEqual("", result.stdout, output)
        self.assertIn("migrations/manifest.toml", result.stderr, output)
        self.assertIn("file must be a migration filename", result.stderr, output)
        self.assertNotIn("Traceback", output)

    def test_directory_manifest_is_a_read_error(self):
        self.write_migration("0001_first.sql")
        (self.migrations / "manifest.toml").mkdir()

        result = self.run_gate()

        output = result.stdout + result.stderr
        self.assertEqual(2, result.returncode, output)
        self.assertEqual("", result.stdout, output)
        self.assertIn("cannot read", result.stderr, output)
        self.assertNotIn("Traceback", output)

    def test_repository_manifest_and_migrations_pass(self):
        manifest_path = REPO_ROOT / "migrations" / "manifest.toml"
        self.assertTrue(manifest_path.is_file(), "migrations/manifest.toml is required")
        self.assertTrue((REPO_ROOT / GATE_SCRIPT).is_file(), "migration gate CLI is required")

        import tomllib

        manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
        targets = {entry["file"]: entry["target"] for entry in manifest["migration"]}
        expected_targets = {
            "0001_mb_mirror.sql": "mirror",
            "0002_auth_vault.sql": "app",
            "0003_library.sql": "app",
            "0004_auth_challenge_purpose.sql": "app",
            "0005_atomic_rate_limits_and_leases.sql": "app",
        }
        self.assertTrue(
            expected_targets.items() <= targets.items(),
            (
                f"expected original migration targets {expected_targets!r} "
                f"to be present in {targets!r}"
            ),
        )

        result = subprocess.run(
            ["python3", GATE_SCRIPT, "--root", str(REPO_ROOT)],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual("", result.stdout)


if __name__ == "__main__":
    unittest.main()
