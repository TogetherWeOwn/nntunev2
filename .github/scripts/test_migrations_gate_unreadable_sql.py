"""Pin migrations-gate unreadable-SQL exit 2.

Contract under test (lines refer to migrations_gate.py at df962ccd):

* A manifest-valid tree with one unreadable migration file exits 2 with
  ``cannot read`` on stderr (check_manifest hashing at :127-128), leaving
  stdout empty and emitting no traceback.
* Exit 2 is distinct from the exit-1 verdicts: a checksum mismatch (:151)
  and a missing manifest file both exit 1 with a ``migrations-gate:``
  verdict line on stdout.
* The valid-tree and mismatch controls stay in the existing suites; this
  file carries only its own minimal contrast pins and temporary fixture
  trees (tempfile only, never the real ``migrations/`` tree).

Seam note: the read failure is injected with ``mock.patch`` on
``Path.read_bytes`` because permission bits are not portable (a root-run
CI ignores ``chmod 0``). Only the victim SQL file raises; the manifest
read (``read_text``) and every other file use the real implementation.
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


class UnreadableSqlTestCase(unittest.TestCase):
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

    def run_gate_cli(self, *args):
        return subprocess.run(
            ["python3", GATE_SCRIPT, "--root", str(self.root), *args],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def run_gate_inprocess(self, *args, read_failure=None):
        """Run migrations_gate.main with stdout/stderr captured.

        read_failure maps a Path to an OSError to raise from read_bytes.
        """
        failures = read_failure or {}
        real_read_bytes = pathlib.Path.read_bytes

        def failing_read_bytes(path_self, *args, **kwargs):
            for victim, exc in failures.items():
                if pathlib.Path(os.fspath(path_self)) == pathlib.Path(
                    os.fspath(victim)
                ):
                    raise exc
            return real_read_bytes(path_self, *args, **kwargs)

        stdout_buf = io.StringIO()
        stderr_buf = io.StringIO()
        with mock.patch.object(
            pathlib.Path, "read_bytes", failing_read_bytes
        ), contextlib.redirect_stdout(stdout_buf), contextlib.redirect_stderr(
            stderr_buf
        ):
            code = migrations_gate.main(["--root", str(self.root), *args])
        return code, stdout_buf.getvalue(), stderr_buf.getvalue()


class TestUnreadableSqlExitTwo(UnreadableSqlTestCase):
    def test_valid_tree_exits_zero(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql")])

        result = self.run_gate_cli()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_unreadable_sql_exits_two_with_cannot_read(self):
        victim = self.write_migration("0001_first.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql")])

        code, stdout, stderr = self.run_gate_inprocess(
            read_failure={victim: OSError("synthetic unreadable migration")}
        )

        output = stdout + stderr
        self.assertEqual(2, code, output)
        self.assertEqual("", stdout, output)
        self.assertIn("cannot read", stderr, output)
        self.assertIn("0001_first.sql", stderr, output)
        self.assertNotIn("Traceback", output)

    def test_unreadable_sql_exit_is_distinct_from_checksum_exit_one(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([("0001_first.sql", "app", "0" * 64)])

        result = self.run_gate_cli()

        output = result.stdout + result.stderr
        self.assertEqual(1, result.returncode, output)
        self.assertIn("migrations-gate: checksum: 0001_first.sql: ", result.stdout)

    def test_unreadable_sql_exit_is_distinct_from_missing_file_exit_one(self):
        self.write_manifest([("0001_missing.sql", "app", "0" * 64)])

        result = self.run_gate_cli()

        output = result.stdout + result.stderr
        self.assertEqual(1, result.returncode, output)
        self.assertIn(
            "migrations-gate: missing-file: 0001_missing.sql: ", result.stdout
        )


if __name__ == "__main__":
    unittest.main()
