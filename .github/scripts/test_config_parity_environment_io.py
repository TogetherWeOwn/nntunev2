#!/usr/bin/env python3
"""Self-tests: environment-file I/O errors in config_parity_check.py.

A directory passed as --staging or --prod cannot be JSON-loaded
(IsADirectoryError is an OSError): portable stand-in for an unreadable
environment file that works even when running as root (chmod 0 would
still be readable). Each case must exit 2 with an input error naming the
correct environment role, never a traceback or a drift verdict.

Run from the repo root:

  python3 -B .github/scripts/test_config_parity_environment_io.py
"""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github" / "scripts" / "config_parity_check.py"
SCHEMA = ROOT / "config" / "schema.json"
STAGING = ROOT / "config" / "staging.json"
PROD = ROOT / "config" / "prod.json"


def run_check(staging, prod, schema=SCHEMA):
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), "--schema", str(schema),
         "--staging", str(staging), "--prod", str(prod)],
        capture_output=True,
        text=True,
        cwd=str(ROOT),
    )
    return proc.returncode, proc.stdout, proc.stderr


class EnvironmentIoTests(unittest.TestCase):
    def assert_environment_input_error(self, code, out, err, label, path):
        self.assertEqual(code, 2, out + err)
        self.assertIn("config-parity: input error:", err)
        self.assertIn(f"{label} file unreadable", err)
        self.assertIn(str(path), err)
        self.assertNotIn("Traceback", out + err)
        self.assertNotIn("config-parity: OK", out + err)
        self.assertNotIn("DRIFT", out + err)
        self.assertNotIn("drift:", out + err)

    def test_directory_staging_is_input_error(self):
        # A directory as --staging (IsADirectoryError, an OSError) must
        # exit 2 naming staging, never a traceback or drift verdict. The
        # schema and prod inputs are the shipped valid files.
        with tempfile.TemporaryDirectory() as tmp:
            staging_dir = Path(tmp) / "staging-dir"
            staging_dir.mkdir()
            code, out, err = run_check(staging_dir, PROD)
        self.assert_environment_input_error(code, out, err, "staging", staging_dir)
        self.assertNotIn("prod file unreadable", err)

    def test_directory_prod_is_input_error(self):
        # Same unreadable-input pin for the prod side: the diagnostic must
        # carry the prod label and prod path, not the staging one. The
        # schema and staging inputs are the shipped valid files.
        with tempfile.TemporaryDirectory() as tmp:
            prod_dir = Path(tmp) / "prod-dir"
            prod_dir.mkdir()
            code, out, err = run_check(STAGING, prod_dir)
        self.assert_environment_input_error(code, out, err, "prod", prod_dir)
        self.assertNotIn("staging file unreadable", err)

    def test_valid_three_inputs_succeed(self):
        # Positive control: the shipped schema/staging/prod triple stays
        # green, proving the above failures come from the unreadable
        # environment file rather than the harness or fixtures.
        code, out, err = run_check(STAGING, PROD)
        self.assertEqual(code, 0, out + err)
        self.assertIn("config-parity: OK", out)
        self.assertNotIn("Traceback", out + err)


if __name__ == "__main__":
    unittest.main(verbosity=2)
