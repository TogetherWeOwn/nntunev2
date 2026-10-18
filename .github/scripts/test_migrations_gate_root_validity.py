"""Pin migrations-gate root-not-directory exit 2.

Contract under test (lines refer to migrations_gate.py at 3ac62bfd):

* A file ``--root`` and a missing ``--root`` both exit 2 with
  ``repository root is not a directory`` on stderr naming the resolved
  root (main at :236-239, GateError -> exit 2 at :244-246), leaving
  stdout empty and emitting no traceback.
* Exit 2 is distinct from the exit-1 verdicts: a drift verdict
  (unlisted SQL) exits 1 with a ``migrations-gate:`` verdict line on
  stdout.
* The valid-root control exits 0 and is unaffected by the guard.

Temporary fixture dirs/files only, never the real ``migrations/`` tree.
"""

import hashlib
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
GATE_SCRIPT = ".github/scripts/migrations_gate.py"


class RootValidityTestCase(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp_dir.name)

    def tearDown(self):
        self.temp_dir.cleanup()

    def run_gate(self, *args):
        return subprocess.run(
            ["python3", GATE_SCRIPT, *args],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def write_valid_tree(self, root):
        migrations = root / "migrations"
        migrations.mkdir(parents=True, exist_ok=True)
        sql = migrations / "0001_first.sql"
        sql.write_text("-- migration\n", encoding="utf-8")
        digest = hashlib.sha256(sql.read_bytes()).hexdigest()
        (migrations / "manifest.toml").write_text(
            '[[migration]]\nfile = "0001_first.sql"\n'
            f'target = "app"\nsha256 = "{digest}"\n',
            encoding="utf-8",
        )
        return root


class TestRootValidityExitTwo(RootValidityTestCase):
    def test_file_as_root_exits_two_naming_root(self):
        victim = self.root / "not-a-dir"
        victim.write_text("i am a file, not a directory\n", encoding="utf-8")

        result = self.run_gate("--root", str(victim))

        output = result.stdout + result.stderr
        self.assertEqual(2, result.returncode, output)
        self.assertEqual("", result.stdout, output)
        self.assertIn("repository root is not a directory", result.stderr, output)
        self.assertIn(str(victim.resolve()), result.stderr, output)
        self.assertNotIn("Traceback", output)

    def test_missing_root_exits_two_naming_root(self):
        missing = self.root / "does-not-exist"

        result = self.run_gate("--root", str(missing))

        output = result.stdout + result.stderr
        self.assertEqual(2, result.returncode, output)
        self.assertEqual("", result.stdout, output)
        self.assertIn("repository root is not a directory", result.stderr, output)
        self.assertIn(str(missing.resolve()), result.stderr, output)
        self.assertNotIn("Traceback", output)

    def test_valid_root_control_exits_zero(self):
        valid = self.root / "valid"
        self.write_valid_tree(valid)

        result = self.run_gate("--root", str(valid))

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_drift_verdict_exits_one_distinct_from_root_exit_two(self):
        valid = self.root / "drift"
        self.write_valid_tree(valid)
        (valid / "migrations" / "0002_second.sql").write_text(
            "-- unlisted\n", encoding="utf-8"
        )

        result = self.run_gate("--root", str(valid))

        output = result.stdout + result.stderr
        self.assertEqual(1, result.returncode, output)
        self.assertIn(
            "migrations-gate: unlisted: 0002_second.sql: ", result.stdout, output
        )


if __name__ == "__main__":
    unittest.main()
