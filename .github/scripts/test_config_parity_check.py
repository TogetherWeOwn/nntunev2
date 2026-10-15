#!/usr/bin/env python3
"""Self-tests for config_parity_check.py using the shipped fixtures.

Matching fixtures stay green (including the values-differ pair, which
proves per-environment values never fail parity); each drifted fixture
fails with exit 1. Run from the repo root:

  python3 .github/scripts/test_config_parity_check.py
"""

import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github" / "scripts" / "config_parity_check.py"
SCHEMA = ROOT / "config" / "schema.json"
STAGING = ROOT / "config" / "staging.json"
FIXTURES = ROOT / "config" / "fixtures"


def run_check(staging, prod):
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), "--schema", str(SCHEMA),
         "--staging", str(staging), "--prod", str(prod)],
        capture_output=True,
        text=True,
        cwd=str(ROOT),
    )
    return proc.returncode, proc.stdout


class ParityCheckTests(unittest.TestCase):
    def test_shipped_staging_vs_prod_is_green(self):
        code, out = run_check(ROOT / "config" / "prod.json", ROOT / "config" / "prod.json")
        self.assertEqual(code, 0, out)
        code, out = run_check(STAGING, ROOT / "config" / "prod.json")
        self.assertEqual(code, 0, out)
        self.assertIn("OK", out)

    def test_matching_fixtures_are_green(self):
        code, out = run_check(STAGING, FIXTURES / "match-prod.json")
        self.assertEqual(code, 0, out)

    def test_differing_values_stay_green(self):
        # Values may differ per environment; only keys and types must match.
        code, out = run_check(STAGING, FIXTURES / "values-differ-prod.json")
        self.assertEqual(code, 0, out)

    def test_missing_required_key_fails(self):
        code, out = run_check(STAGING, FIXTURES / "drift-missing-key.json")
        self.assertEqual(code, 1, out)
        self.assertIn("missing required key", out)

    def test_type_mismatch_fails(self):
        code, out = run_check(FIXTURES / "drift-type-mismatch.json", FIXTURES / "match-prod.json")
        self.assertEqual(code, 1, out)
        self.assertIn("session.ttl_secs", out)

    def test_undeclared_key_fails(self):
        code, out = run_check(FIXTURES / "drift-extra-key.json", FIXTURES / "match-prod.json")
        self.assertEqual(code, 1, out)
        self.assertIn("undeclared key", out)

    def test_key_set_asymmetry_fails(self):
        # The extra key is undeclared, so it fails on both counts; assert
        # the parity line is present too.
        code, out = run_check(FIXTURES / "drift-extra-key.json", FIXTURES / "match-prod.json")
        self.assertEqual(code, 1, out)
        self.assertIn("parity:", out)

    def test_missing_file_is_input_error(self):
        code, _ = run_check(STAGING, FIXTURES / "does-not-exist.json")
        self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
