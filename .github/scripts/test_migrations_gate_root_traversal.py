"""Pin migration-gate root and traversal error classification.

Contract under test (tests-only pin; implementation is unchanged):

* A ``--root`` that is an existing file (not a directory) exits 2 with a
  root/non-directory diagnostic on stderr, empty stdout and no traceback.
* A traversal ``OSError`` raised while discovering SQL files, and one raised
  while discovering symlinks, each surface as exit 2 with a
  ``cannot list migrations`` diagnostic, empty stdout and no traceback.
* Clean / violation / input exits stay distinct at 0 / 1 / 2 (controls).

Seam contract (already on main; preserved here):

``migrations_gate.main(argv) -> int`` returns the CLI exit code and prints
violations to stdout and gate errors to stderr. Traversal failures surface
from ``pathlib.Path.rglob`` inside ``check_manifest`` and are classified by
the ``except OSError`` dispatch into ``GateError`` (exit 2).

Injection note: traversal failures are injected by patching
``pathlib.Path.rglob`` with a deterministic per-call failure. Permission
bits are not used because a root-run CI ignores them, and this pin does not
claim that every real ``Path.rglob`` permission error propagates on every
platform; it pins that an ``OSError`` escaping either traversal call is
classified as exit 2 with the ``cannot list migrations`` diagnostic.
"""

import contextlib
import hashlib
import io
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import migrations_gate

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
GATE_SCRIPT = ".github/scripts/migrations_gate.py"

SQL_FAILURE_SENTINEL = "SYNTHETIC-SQL-TRAVERSAL-9d4c2a"
SYMLINK_FAILURE_SENTINEL = "SYNTHETIC-SYMLINK-TRAVERSAL-5b7e1f"


class RootTraversalBase(unittest.TestCase):
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

    def write_valid_fixture(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql")])

    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, GATE_SCRIPT, "--root", str(self.root), *args],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def run_main(self, argv):
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = migrations_gate.main(argv)
        return code, stdout.getvalue(), stderr.getvalue()


def rglob_failing_on_call(fail_on_call, exc):
    """Return a Path.rglob replacement raising exc on the nth call."""
    real_rglob = pathlib.Path.rglob
    calls = {"count": 0}

    def _rglob(self, pattern="*"):
        calls["count"] += 1
        if calls["count"] == fail_on_call:
            raise exc
        return real_rglob(self, pattern)

    return _rglob


class TestRootIsNotDirectory(RootTraversalBase):
    def test_existing_file_as_root_is_exit_two(self):
        self.write_valid_fixture()
        file_root = self.root / "existing-file"
        file_root.write_text("not a directory\n", encoding="utf-8")

        result = subprocess.run(
            [sys.executable, GATE_SCRIPT, "--root", str(file_root)],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

        output = result.stdout + result.stderr
        self.assertEqual(2, result.returncode, output)
        self.assertEqual("", result.stdout, output)
        self.assertIn("repository root is not a directory", result.stderr, output)
        self.assertIn(str(file_root), result.stderr, output)
        self.assertNotIn("Traceback", output)


class TestTraversalErrorClassification(RootTraversalBase):
    def assert_gate_error(self, code, stdout, stderr, sentinel):
        output = stdout + stderr
        self.assertEqual(2, code, output)
        self.assertEqual("", stdout, output)
        self.assertIn("cannot list migrations", stderr, output)
        self.assertIn(sentinel, stderr, output)
        self.assertNotIn("Traceback", output)

    def test_sql_discovery_oserror_is_exit_two(self):
        self.write_valid_fixture()
        failing = rglob_failing_on_call(1, OSError(SQL_FAILURE_SENTINEL))

        with mock.patch.object(pathlib.Path, "rglob", failing):
            code, stdout, stderr = self.run_main(["--root", str(self.root)])

        self.assert_gate_error(code, stdout, stderr, SQL_FAILURE_SENTINEL)

    def test_symlink_discovery_oserror_is_exit_two(self):
        self.write_valid_fixture()
        failing = rglob_failing_on_call(2, OSError(SYMLINK_FAILURE_SENTINEL))

        with mock.patch.object(pathlib.Path, "rglob", failing):
            code, stdout, stderr = self.run_main(["--root", str(self.root)])

        self.assert_gate_error(code, stdout, stderr, SYMLINK_FAILURE_SENTINEL)


class TestExitControls(RootTraversalBase):
    def test_clean_fixture_exits_zero(self):
        self.write_valid_fixture()

        result = self.run_cli()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual("", result.stdout)

    def test_violation_fixture_exits_one(self):
        self.write_migration("0001_first.sql")
        self.write_migration("0002_unlisted.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql")])

        result = self.run_cli()

        output = result.stdout + result.stderr
        self.assertEqual(1, result.returncode, output)
        self.assertIn("migrations-gate: unlisted: 0002_unlisted.sql: ", result.stdout)

    def test_malformed_manifest_exits_two(self):
        (self.migrations / "manifest.toml").write_text(
            "[[migration]\nfile =\n", encoding="utf-8"
        )

        result = self.run_cli()

        output = result.stdout + result.stderr
        self.assertEqual(2, result.returncode, output)
        self.assertEqual("", result.stdout, output)
        self.assertNotIn("Traceback", output)


if __name__ == "__main__":
    unittest.main()
