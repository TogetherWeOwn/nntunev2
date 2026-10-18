"""Pin migrations-gate manifest-shape exit-2 mapping.

Contract under test (lines refer to migrations_gate.py at ba9820c8):

* A manifest with zero ``[[migration]]`` tables exits 2 with
  ``must contain [[migration]] tables`` on stderr (load_manifest at :53).
* An entry missing ``sha256`` exits 2 with
  ``requires file, target, and sha256`` on stderr (load_manifest at :55-56).
* An entry with a non-string field exits 2 with
  ``fields must be strings`` on stderr (load_manifest at :57-58).
* An unlistable migrations dir exits 2 with ``cannot list migrations``
  on stderr (check_manifest at :75).
* Each shape case routes through ``main()`` (:244-246) returning exactly 2,
  leaving stdout empty and emitting no traceback: a malformed manifest must
  never render as "0 violations, exit 0" nor as confusable drift (exit 1).
* Exit 2 is distinct from the exit-1 verdicts: a checksum mismatch exits 1
  with a ``migrations-gate:`` verdict line on stdout.

Seam notes: temporary fixture trees only (tempfile, never the real
``migrations/`` tree). The unlistable-dir failure is injected with
``mock.patch`` on ``Path.rglob`` because permission bits are not portable
(a root-run CI ignores ``chmod 0``). Only the listing raises; manifest and
SQL reads use the real implementation.
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


class ManifestShapeTestCase(unittest.TestCase):
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

    def write_manifest_text(self, text):
        (self.migrations / "manifest.toml").write_text(text, encoding="utf-8")

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
        self.write_manifest_text("\n".join(lines))

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

    def run_gate_inprocess(self, *args, rglob_failure=None):
        """Run migrations_gate.main with stdout/stderr captured."""
        stdout_buf = io.StringIO()
        stderr_buf = io.StringIO()
        patcher = (
            mock.patch.object(
                pathlib.Path,
                "rglob",
                lambda self, *a, **k: (_ for _ in ()).throw(rglob_failure),
            )
            if rglob_failure is not None
            else contextlib.nullcontext()
        )
        with patcher, contextlib.redirect_stdout(
            stdout_buf
        ), contextlib.redirect_stderr(stderr_buf):
            code = migrations_gate.main(["--root", str(self.root), *args])
        return code, stdout_buf.getvalue(), stderr_buf.getvalue()

    def assert_shape_exit_two(self, code, stdout, stderr, needle):
        output = stdout + stderr
        self.assertEqual(2, code, output)
        self.assertEqual("", stdout, output)
        self.assertIn(needle, stderr, output)
        self.assertNotIn("Traceback", output)
        self.assertNotIn("0 violations", output)


class TestManifestShapeExitTwo(ManifestShapeTestCase):
    def test_zero_migration_tables_exits_two(self):
        # Singular [migration] table: zero [[migration]] array entries.
        self.write_migration("0001_first.sql")
        self.write_manifest_text('[migration]\nfile = "0001_first.sql"\n')

        code, stdout, stderr = self.run_gate_inprocess()

        self.assert_shape_exit_two(
            code, stdout, stderr, "must contain [[migration]] tables"
        )

    def test_entry_missing_sha256_exits_two(self):
        self.write_migration("0001_first.sql")
        self.write_manifest_text(
            '[[migration]]\nfile = "0001_first.sql"\ntarget = "app"\n'
        )

        code, stdout, stderr = self.run_gate_inprocess()

        self.assert_shape_exit_two(
            code, stdout, stderr, "requires file, target, and sha256"
        )

    def test_non_string_field_exits_two(self):
        self.write_migration("0001_first.sql")
        self.write_manifest_text(
            '[[migration]]\nfile = "0001_first.sql"\ntarget = "app"\nsha256 = 123\n'
        )

        code, stdout, stderr = self.run_gate_inprocess()

        self.assert_shape_exit_two(code, stdout, stderr, "fields must be strings")

    def test_unlistable_migrations_dir_exits_two(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([self.manifest_entry("0001_first.sql")])

        code, stdout, stderr = self.run_gate_inprocess(
            rglob_failure=OSError("synthetic unlistable migrations dir")
        )

        self.assert_shape_exit_two(code, stdout, stderr, "cannot list migrations")

    def test_shape_exit_is_distinct_from_drift_exit_one(self):
        self.write_migration("0001_first.sql")
        self.write_manifest([("0001_first.sql", "app", "0" * 64)])

        result = self.run_gate_cli()

        output = result.stdout + result.stderr
        self.assertEqual(1, result.returncode, output)
        self.assertIn("migrations-gate: checksum: 0001_first.sql: ", result.stdout)


if __name__ == "__main__":
    unittest.main()
