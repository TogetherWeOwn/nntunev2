#!/usr/bin/env python3
"""Self-tests for config_parity_check.py using the shipped fixtures.

Matching fixtures stay green (including the values-differ pair, which
proves per-environment values never fail parity); each drifted fixture
fails with exit 1. Run from the repo root:

  python3 .github/scripts/test_config_parity_check.py
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github" / "scripts" / "config_parity_check.py"
SCHEMA = ROOT / "config" / "schema.json"
STAGING = ROOT / "config" / "staging.json"
FIXTURES = ROOT / "config" / "fixtures"


def run_check(staging, prod, schema=SCHEMA):
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), "--schema", str(schema),
         "--staging", str(staging), "--prod", str(prod)],
        capture_output=True,
        text=True,
        cwd=str(ROOT),
    )
    return proc.returncode, proc.stdout, proc.stderr


def load_schema():
    return json.loads(SCHEMA.read_text(encoding="utf-8"))


def write_json(path, obj):
    Path(path).write_text(json.dumps(obj), encoding="utf-8")


class ParityCheckTests(unittest.TestCase):
    def test_shipped_staging_vs_prod_is_green(self):
        code, out, _ = run_check(ROOT / "config" / "prod.json", ROOT / "config" / "prod.json")
        self.assertEqual(code, 0, out)
        code, out, _ = run_check(STAGING, ROOT / "config" / "prod.json")
        self.assertEqual(code, 0, out)
        self.assertIn("OK", out)

    def test_matching_fixtures_are_green(self):
        code, out, _ = run_check(STAGING, FIXTURES / "match-prod.json")
        self.assertEqual(code, 0, out)

    def test_differing_values_stay_green(self):
        # Values may differ per environment; only keys and types must match.
        code, out, _ = run_check(STAGING, FIXTURES / "values-differ-prod.json")
        self.assertEqual(code, 0, out)

    def test_missing_required_key_fails(self):
        code, out, _ = run_check(STAGING, FIXTURES / "drift-missing-key.json")
        self.assertEqual(code, 1, out)
        self.assertIn("missing required key", out)

    def test_type_mismatch_fails(self):
        code, out, _ = run_check(FIXTURES / "drift-type-mismatch.json", FIXTURES / "match-prod.json")
        self.assertEqual(code, 1, out)
        self.assertIn("session.ttl_secs", out)

    def test_matching_type_errors_on_both_sides_fail(self):
        # Both sides carry the SAME wrong type (string where u64 belongs),
        # so the staging-vs-prod type-diff fallback cannot see it: only the
        # per-side schema validation catches this. Pins check_value.
        code, out, _ = run_check(
            FIXTURES / "drift-type-mismatch.json",
            FIXTURES / "drift-type-mismatch.json",
        )
        self.assertEqual(code, 1, out)
        self.assertIn("expected u64", out)

    def test_undeclared_key_fails(self):
        code, out, _ = run_check(FIXTURES / "drift-extra-key.json", FIXTURES / "match-prod.json")
        self.assertEqual(code, 1, out)
        self.assertIn("undeclared key", out)

    def test_key_set_asymmetry_fails(self):
        # Validation errors return before the parity block, so an
        # undeclared extra key can never exercise it. Use an OPTIONAL
        # schema key present on one side only: both sides validate, and
        # only the parity block can fail. Assert the exact "parity: key"
        # line (a bare "parity:" prefix matches every "config-parity:"
        # status line and proves nothing).
        schema = load_schema()
        schema["keys"]["mb.search_limit"]["required"] = False
        staging = json.loads((ROOT / "config" / "staging.json").read_text(encoding="utf-8"))
        prod = json.loads((ROOT / "config" / "prod.json").read_text(encoding="utf-8"))
        del prod["mb.search_limit"]
        with tempfile.TemporaryDirectory() as tmp:
            schema_path = Path(tmp) / "schema.json"
            staging_path = Path(tmp) / "staging.json"
            prod_path = Path(tmp) / "prod.json"
            write_json(schema_path, schema)
            write_json(staging_path, staging)
            write_json(prod_path, prod)
            code, out, _ = run_check(staging_path, prod_path, schema=schema_path)
        self.assertEqual(code, 1, out)
        self.assertIn("parity: key mb.search_limit", out)

    def test_bad_schema_fails(self):
        # Pins validate_schema: neutering it must not turn these green.
        cases = {
            "bad version": ("schema_version", 2),
        }
        for label, (field, value) in cases.items():
            with self.subTest(label):
                schema = load_schema()
                schema[field] = value
                with tempfile.TemporaryDirectory() as tmp:
                    schema_path = Path(tmp) / "schema.json"
                    write_json(schema_path, schema)
                    code, out, _ = run_check(STAGING, ROOT / "config" / "prod.json",
                                          schema=schema_path)
                self.assertEqual(code, 1, out)
                self.assertIn("schema_version must be 1", out)
        with self.subTest("bad default type"):
            schema = load_schema()
            schema["keys"]["session.ttl_secs"]["default"] = "soon"
            with tempfile.TemporaryDirectory() as tmp:
                schema_path = Path(tmp) / "schema.json"
                write_json(schema_path, schema)
                code, out, _ = run_check(STAGING, ROOT / "config" / "prod.json",
                                      schema=schema_path)
            self.assertEqual(code, 1, out)
            self.assertIn("default", out)
        with self.subTest("overridable without env_var"):
            schema = load_schema()
            schema["keys"]["session.ttl_secs"]["env_var"] = None
            with tempfile.TemporaryDirectory() as tmp:
                schema_path = Path(tmp) / "schema.json"
                write_json(schema_path, schema)
                code, out, _ = run_check(STAGING, ROOT / "config" / "prod.json",
                                      schema=schema_path)
            self.assertEqual(code, 1, out)
            self.assertIn("env_var", out)

    def test_bool_schema_type_accepts_boolean_and_rejects_integer(self):
        # Exercise bool validation without adding a bool key to the shipped schema.
        schema = load_schema()
        schema["keys"]["test.bool_flag"] = {
            "type": "bool",
            "default": False,
            "required": False,
            "env_overridable": False,
            "env_var": None,
        }
        staging = json.loads(STAGING.read_text(encoding="utf-8"))
        prod = json.loads((ROOT / "config" / "prod.json").read_text(encoding="utf-8"))
        staging["test.bool_flag"] = True
        prod["test.bool_flag"] = False

        with tempfile.TemporaryDirectory() as tmp:
            schema_path = Path(tmp) / "schema.json"
            staging_path = Path(tmp) / "staging.json"
            prod_path = Path(tmp) / "prod.json"
            write_json(schema_path, schema)
            write_json(staging_path, staging)
            write_json(prod_path, prod)
            code, out, _ = run_check(staging_path, prod_path, schema=schema_path)
            self.assertEqual(code, 0, out)

            staging["test.bool_flag"] = 1
            write_json(staging_path, staging)
            code, out, _ = run_check(staging_path, prod_path, schema=schema_path)
        self.assertEqual(code, 1, out)
        self.assertIn("expected bool, got int", out)

    def test_missing_file_is_input_error(self):
        code, _, _ = run_check(STAGING, FIXTURES / "does-not-exist.json")
        self.assertEqual(code, 2)

    def test_malformed_staging_json_is_input_error(self):
        # Truncated staging JSON must exit 2 with an input error naming the
        # file, never a traceback or a pass. Tmp-dir fixture only; the
        # shipped config/ fixtures stay read-only.
        with tempfile.TemporaryDirectory() as tmp:
            bad_staging = Path(tmp) / "staging.json"
            bad_staging.write_text('{"schema_version": 1, "keys": {', encoding="utf-8")
            code, out, err = run_check(bad_staging, ROOT / "config" / "prod.json")
        self.assertEqual(code, 2, out + err)
        self.assertIn("config-parity: input error:", err)
        self.assertIn("staging file is not valid JSON", err)
        self.assertIn(str(bad_staging), err)
        self.assertNotIn("Traceback", out + err)

    def test_malformed_prod_json_names_prod_file(self):
        # Same malformed-input pin for the prod side: the diagnostic must
        # carry the prod label and prod path, not the staging one.
        with tempfile.TemporaryDirectory() as tmp:
            bad_prod = Path(tmp) / "prod.json"
            bad_prod.write_text('{"mb.search_limit": 10,', encoding="utf-8")
            code, out, err = run_check(STAGING, bad_prod)
        self.assertEqual(code, 2, out + err)
        self.assertIn("config-parity: input error:", err)
        self.assertIn("prod file is not valid JSON", err)
        self.assertIn(str(bad_prod), err)
        self.assertNotIn("staging file is not valid JSON", err)
        self.assertNotIn("Traceback", out + err)

    def test_directory_schema_path_is_input_error(self):
        # A directory cannot be JSON-loaded (IsADirectoryError is an
        # OSError): portable stand-in for an unreadable schema file that
        # works even when running as root (chmod 0 would still be
        # readable). Must exit 2 with an input error, never a traceback.
        with tempfile.TemporaryDirectory() as tmp:
            schema_dir = Path(tmp) / "schema-dir"
            schema_dir.mkdir()
            code, out, err = run_check(
                STAGING, ROOT / "config" / "prod.json", schema=schema_dir
            )
        self.assertEqual(code, 2, out + err)
        self.assertIn("config-parity: input error:", err)
        self.assertIn("schema file unreadable", err)
        self.assertIn(str(schema_dir), err)
        self.assertNotIn("Traceback", out + err)


if __name__ == "__main__":
    unittest.main(verbosity=2)
