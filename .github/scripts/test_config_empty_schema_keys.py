#!/usr/bin/env python3
"""Pin empty-schema-keys vacuous-pass guard in config_parity_check.py.

``validate_schema`` rejects a schema whose ``keys`` member is missing,
a non-dict, or an empty dict with ``keys must be a non-empty object``
(exit 1). That is the only stop against an emptied schema validating
zero keys and passing everything vacuously (exit 0).

Each shape below (``keys: {}``, missing ``keys``, ``keys: []``, and a
truthy non-dict ``keys: ["x"]``) must
exit 1 and name the guard; the shipped schema stays green as the
control. Temporary fixture schemas only; the shipped config/ fixtures
stay read-only.

Run from the repo root:

  python3 .github/scripts/test_config_empty_schema_keys.py
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
PROD = ROOT / "config" / "prod.json"

GUARD_MESSAGE = "keys must be a non-empty object"


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


class EmptySchemaKeysTests(unittest.TestCase):
    def assert_empty_keys_rejected(self, schema):
        with tempfile.TemporaryDirectory() as tmp:
            schema_path = Path(tmp) / "schema.json"
            write_json(schema_path, schema)
            code, out, err = run_check(STAGING, PROD, schema=schema_path)
        combined = out + err
        self.assertEqual(code, 1, combined)
        self.assertNotEqual(code, 0, combined)
        self.assertNotEqual(code, 2, combined)
        self.assertIn(GUARD_MESSAGE, combined)
        self.assertNotIn("Traceback", combined)

    def test_empty_keys_object_fails(self):
        schema = load_schema()
        schema["keys"] = {}
        self.assert_empty_keys_rejected(schema)

    def test_missing_keys_fails(self):
        schema = load_schema()
        del schema["keys"]
        self.assert_empty_keys_rejected(schema)

    def test_keys_list_fails(self):
        schema = load_schema()
        schema["keys"] = []
        self.assert_empty_keys_rejected(schema)

    def test_keys_nonempty_non_object_fails(self):
        schema = load_schema()
        schema["keys"] = ["x"]
        self.assert_empty_keys_rejected(schema)

    def test_shipped_schema_green_control(self):
        code, out, err = run_check(STAGING, PROD)
        self.assertEqual(code, 0, out + err)
        self.assertIn("OK", out)


if __name__ == "__main__":
    unittest.main(verbosity=2)
