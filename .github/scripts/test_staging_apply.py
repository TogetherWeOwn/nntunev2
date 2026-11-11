"""Executed tests for the staging-apply driver (finding 5 coverage)."""
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import staging_apply

APP_SHA = "a" * 64
MIRROR_SHA = "b" * 64
DB = "nntunev2_staging"
URL_REQUIRE = ("postgresql://migrator:secretpw@ep.example.com"
               f"/{DB}?sslmode=require")
URL_FULL = ("postgresql://migrator:secretpw@ep.example.com"
            f"/{DB}?sslmode=verify-full")


def manifest_text():
    return f"""[[migration]]
file = "0054_first.sql"
sha256 = "{APP_SHA}"
target = "app"

[[migration]]
file = "0055_second.sql"
sha256 = "{APP_SHA}"
target = "app"

[[migration]]
file = "0001_mb_mirror.sql"
sha256 = "{MIRROR_SHA}"
target = "mirror"
"""


def renamed_manifest_text():
    return f"""[[migration]]
file = "0054_renamed.sql"
sha256 = "{APP_SHA}"
target = "app"

[[migration.renamed_from]]
file = "0054_original.sql"
sha256 = "{APP_SHA}"
"""


class StagingApplyTests(unittest.TestCase):
    def _roots(self, manifest):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = pathlib.Path(tmp.name)
        data = root / "data"
        trusted = root / "trusted"
        (data / "migrations").mkdir(parents=True)
        (trusted / ".github/scripts").mkdir(parents=True)
        (data / "migrations/manifest.toml").write_text(manifest)
        (trusted / ".github/scripts/apply_migration.py").write_text(
            "import sys\nsys.exit(0)\n")
        return data, trusted, root

    # Finding 2: single mode refuses a mirror file before connecting.
    def test_single_mode_refuses_mirror_before_connecting(self):
        def fail_runner(*args, **kwargs):
            raise AssertionError("subprocess must not run for a mirror file")
        data, trusted, _ = self._roots(manifest_text())
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "PATH": os.environ["PATH"], "HOME": "/tmp"}
        original = staging_apply.subprocess.run
        staging_apply.subprocess.run = fail_runner
        try:
            with self.assertRaises(staging_apply.StagingApplyError):
                staging_apply.apply_files(str(trusted), str(data),
                                          "0001_mb_mirror.sql", "tester",
                                          "single", DB, env)
        finally:
            staging_apply.subprocess.run = original

    def test_plan_files_single_unknown_file_refuses(self):
        entries = staging_apply.load_entries(
            str(self._roots(manifest_text())[0]))
        with self.assertRaises(staging_apply.StagingApplyError):
            staging_apply.plan_files(entries, "0099_unknown.sql", "single")

    def test_plan_files_all_pending_walks_app_order_only(self):
        data, _, _ = self._roots(manifest_text())
        entries = staging_apply.load_entries(str(data))
        self.assertEqual(
            staging_apply.plan_files(entries, "0055_second.sql", "all-pending"),
            ["0054_first.sql", "0055_second.sql"])

    # Finding 3: one shared normalization forcing verify-full.
    def test_normalize_forces_verify_full_over_require(self):
        normalized = staging_apply.normalize_url(URL_REQUIRE, DB)
        self.assertIn("sslmode=verify-full", normalized)
        self.assertNotIn("require", normalized)

    def test_evidence_child_env_forces_verify_full_over_require(self):
        env = staging_apply.evidence_child_env(URL_REQUIRE, DB)
        self.assertEqual(env["PGSSLMODE"], "verify-full")
        apply_url = staging_apply.normalize_url(URL_REQUIRE, DB)
        self.assertIn("verify-full", apply_url)
        # Both phases accept the same secret: no sslmode rejection.
        full_env = staging_apply.evidence_child_env(URL_FULL, DB)
        self.assertEqual(full_env["PGSSLMODE"], "verify-full")

    # Finding 4: evidence finds a renamed (alias-recorded) row.
    def test_evidence_accepts_renamed_alias_row(self):
        data, _, root = self._roots(renamed_manifest_text())
        summary = root / "summary.md"
        summary.write_text("")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_STEP_SUMMARY": str(summary),
               "PATH": os.environ["PATH"], "HOME": "/tmp"}

        def fake_psql(argv, **kwargs):
            # Row exists only under the pre-rename name.
            return SimpleNamespace(
                returncode=0,
                stdout="0054_original.sql\t" + APP_SHA + "\t2026-11-11\tci")

        original = staging_apply.subprocess.run
        staging_apply.subprocess.run = fake_psql
        try:
            staging_apply.run_evidence(str(data), "0054_renamed.sql",
                                       "0054_renamed.sql", "success", DB, env)
        finally:
            staging_apply.subprocess.run = original
        self.assertIn("0054_original.sql", summary.read_text())

    def test_evidence_query_includes_alias_names(self):
        data, _, _ = self._roots(renamed_manifest_text())
        entries = staging_apply.load_entries(str(data))
        entry = staging_apply.find_entry(entries, "0054_renamed.sql")
        self.assertEqual(sorted(staging_apply.entry_names(entry)),
                         ["0054_original.sql", "0054_renamed.sql"])

    # First-failure stop with applied-prefix evidence.
    def test_first_failure_stops_and_reports_applied_prefix(self):
        data, trusted, root = self._roots(manifest_text())
        output = root / "outputs.txt"
        output.write_text("")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_OUTPUT": str(output),
               "PATH": os.environ["PATH"], "HOME": "/tmp"}
        calls = []

        def fake_runner(argv, **kwargs):
            calls.append(argv)
            # First file succeeds, second fails.
            code = 0 if len(calls) == 1 else 1
            return SimpleNamespace(returncode=code, stdout="", stderr="boom")

        original = staging_apply.subprocess.run
        staging_apply.subprocess.run = fake_runner
        try:
            with self.assertRaises(staging_apply.StagingApplyError):
                staging_apply.apply_files(str(trusted), str(data),
                                          "0055_second.sql", "tester",
                                          "all-pending", DB, env)
        finally:
            staging_apply.subprocess.run = original
        self.assertEqual(len(calls), 2)
        self.assertEqual(output.read_text().strip(),
                         "applied_files=0054_first.sql")

    # In-flight file is named in the evidence summary on failure.
    def test_in_flight_file_reported_on_failure(self):
        data, _, root = self._roots(manifest_text())
        summary = root / "summary.md"
        summary.write_text("")
        progress = root / "progress"
        progress.write_text("0055_second.sql")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_STEP_SUMMARY": str(summary),
               "APPLY_PROGRESS": str(progress),
               "PATH": os.environ["PATH"], "HOME": "/tmp"}

        def fake_psql(argv, **kwargs):
            return SimpleNamespace(returncode=0, stdout="")

        original = staging_apply.subprocess.run
        staging_apply.subprocess.run = fake_psql
        try:
            staging_apply.run_evidence(str(data), "0055_second.sql",
                                       "0054_first.sql", "failure", DB, env)
        finally:
            staging_apply.subprocess.run = original
        text = summary.read_text()
        self.assertIn("In-flight at failure", text)
        self.assertIn("0055_second.sql", text)
        self.assertIn("reconcile", text)

    # Finding 1: only trusted code executes; data code never runs, and
    # the child environment is allowlisted.
    def test_only_trusted_migrator_executes_with_allowlisted_env(self):
        data, trusted, root = self._roots(manifest_text())
        # Malicious scripts in the data checkout: must never execute.
        (data / ".github/scripts").mkdir(parents=True, exist_ok=True)
        (data / ".github/scripts/apply_migration.py").write_text(
            "import pathlib; pathlib.Path('pwned.txt').write_text('x')\n")
        # Trusted stub: records that it ran and dumps its env keys.
        stub = trusted / ".github/scripts/apply_migration.py"
        stub.write_text(
            "import os, pathlib\n"
            "pathlib.Path('ran.txt').write_text('ok')\n"
            "pathlib.Path('env.txt').write_text(','.join(sorted(os.environ)))\n")
        output = root / "outputs.txt"
        output.write_text("")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_OUTPUT": str(output),
               "PATH": os.environ["PATH"], "HOME": "/tmp",
               "FAKE_SECRET": "spike-value", "GITHUB_TOKEN": "spike"}
        staging_apply.apply_files(str(trusted), str(data),
                                  "0054_first.sql", "tester",
                                  "single", DB, env)
        self.assertTrue((trusted / "ran.txt").is_file())
        self.assertFalse((trusted / "pwned.txt").exists())
        self.assertFalse((data / "pwned.txt").exists())
        env_keys = (trusted / "env.txt").read_text()
        self.assertIn("DATABASE_URL", env_keys)
        self.assertNotIn("FAKE_SECRET", env_keys)
        self.assertNotIn("GITHUB_TOKEN", env_keys)
        self.assertNotIn("PLANETSCALE_MIGRATOR_URL_STAGING", env_keys)

    def test_apply_argv_points_at_trusted_script_and_data_files(self):
        data, trusted, root = self._roots(manifest_text())
        output = root / "outputs.txt"
        output.write_text("")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_OUTPUT": str(output),
               "PATH": os.environ["PATH"], "HOME": "/tmp"}
        seen = []

        def fake_runner(argv, **kwargs):
            seen.append(argv)
            return SimpleNamespace(returncode=0, stdout="", stderr="")

        original = staging_apply.subprocess.run
        staging_apply.subprocess.run = fake_runner
        try:
            staging_apply.apply_files(str(trusted), str(data),
                                      "0054_first.sql", "tester",
                                      "single", DB, env)
        finally:
            staging_apply.subprocess.run = original
        argv = seen[0]
        self.assertIn(str(trusted / ".github/scripts/apply_migration.py"), argv)
        self.assertIn(str(data / "migrations/0054_first.sql"), argv)
        self.assertIn(str(data / "migrations/manifest.toml"), argv)
        # -E -s (not isolated -I): the flag set ships with the driver and
        # keeps the trusted script directory on sys.path for the sibling
        # import (finding 1 in the return-#2 fix).
        self.assertIn("-E", argv)
        self.assertIn("-s", argv)
        self.assertNotIn("-I", argv)

    # Return-#2 finding 4: executed regression in the workflow's exact
    # invocation shape (subprocess, relative roots, sibling import),
    # which the in-process tests above cannot see.
    def test_driver_subprocess_reaches_child_with_relative_roots(self):
        scripts_dir = pathlib.Path(__file__).resolve().parent
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        workspace = pathlib.Path(tmp.name)
        trusted_scripts = workspace / "trusted/.github/scripts"
        data_migrations = workspace / "data/migrations"
        trusted_scripts.mkdir(parents=True)
        data_migrations.mkdir(parents=True)
        # Real driver + real sibling module so the import chain executes.
        (trusted_scripts / "staging_apply.py").write_bytes(
            (scripts_dir / "staging_apply.py").read_bytes())
        (trusted_scripts / "migration_history.py").write_bytes(
            (scripts_dir / "migration_history.py").read_bytes())
        # Trusted migrator stub: imports the sibling module and refuses
        # unless --file/--manifest resolve to existing files.
        (trusted_scripts / "apply_migration.py").write_text(
            "import migration_history, pathlib, sys\n"
            "argv = sys.argv\n"
            "file_arg = argv[argv.index('--file') + 1]\n"
            "manifest_arg = argv[argv.index('--manifest') + 1]\n"
            "assert pathlib.Path(file_arg).is_file(), file_arg\n"
            "assert pathlib.Path(manifest_arg).is_file(), manifest_arg\n"
            "pathlib.Path('child_ran.txt').write_text('ok')\n")
        (data_migrations / "manifest.toml").write_text(manifest_text())
        (data_migrations / "0054_first.sql").write_text("SELECT 1;\n")
        output = workspace / "outputs.txt"
        output.write_text("")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_OUTPUT": str(output),
               "APPLY_PROGRESS": str(workspace / "progress"),
               "PATH": os.environ["PATH"], "HOME": "/tmp"}
        result = subprocess.run(
            [sys.executable, "-E", "-s",
             "trusted/.github/scripts/staging_apply.py", "apply",
             "--trusted-root", "trusted", "--data-root", "data",
             "--file", "0054_first.sql", "--applied-by", "tester",
             "--mode", "single", "--expect-database", DB],
            cwd=str(workspace), env=env, capture_output=True, text=True,
            timeout=120)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(
            (workspace / "trusted/child_ran.txt").is_file()
            or (workspace / "child_ran.txt").is_file())
        self.assertIn("applied_files=0054_first.sql", output.read_text())


if __name__ == "__main__":
    unittest.main()
