#!/usr/bin/env python3
"""Acceptance tests: duplicate JSON member names in config parity inputs.

Contract for the Builder: a duplicate member name in any input document
(schema, staging fixture, or prod fixture), at any object depth, makes
that document unusable input: the CLI exits 2 (the same class as
malformed JSON), before any semantic parity checks run. Semantic drift
stays exit 1 and valid parity stays exit 0.

Diagnostics for a duplicate must name the input role (schema / staging /
prod) and the duplicate-member category, and must not print member
values, raw JSON, or exception content. A member name may be shown only
escaped and length-bounded.

All fixtures below are raw JSON text (never dicts via json.dumps): a
serialized dict cannot carry duplicate member names. Every duplicate
fixture is written so the LAST value wins under ordinary json.load and
validates cleanly -- so before the fix these inputs are silently
accepted (exit 0), which is exactly the defect.

Run from the repo root:

  python3 .github/scripts/test_config_duplicate_members.py
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github" / "scripts" / "config_parity_check.py"

VALID_SCHEMA = """{
  "schema_version": 1,
  "description": "synthetic duplicate-member fixture schema",
  "keys": {
    "session.ttl_secs": {
      "type": "u64",
      "default": 60,
      "required": true,
      "env_overridable": false,
      "env_var": null
    },
    "auth.rp_id": {
      "type": "string",
      "default": null,
      "required": true,
      "env_overridable": true,
      "env_var": "RP_ID"
    }
  }
}
"""

VALID_STAGING = """{
  "session.ttl_secs": 60,
  "auth.rp_id": "staging.example.invalid"
}
"""

VALID_PROD = """{
  "session.ttl_secs": 120,
  "auth.rp_id": "prod.example.invalid"
}
"""


def run_check(tmp, schema_text=VALID_SCHEMA, staging_text=VALID_STAGING,
              prod_text=VALID_PROD):
    # Role-neutral file names: the diagnostic must name the input role
    # itself — the tmp path is echoed after the message, so a role token
    # matching only the file name would let a labelless diagnostic pass.
    schema = Path(tmp) / "input-a.json"
    staging = Path(tmp) / "input-b.json"
    prod = Path(tmp) / "input-c.json"
    schema.write_text(schema_text, encoding="utf-8")
    staging.write_text(staging_text, encoding="utf-8")
    prod.write_text(prod_text, encoding="utf-8")
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), "--schema", str(schema),
         "--staging", str(staging), "--prod", str(prod)],
        capture_output=True,
        text=True,
        cwd=str(ROOT),
    )
    return proc.returncode, proc.stdout + proc.stderr


class DuplicateMemberTests(unittest.TestCase):
    def assert_duplicate_input_error(self, code, out, role_tokens,
                                     forbidden=()):
        self.assertEqual(code, 2, out)
        lowered = out.lower()
        self.assertIn("duplicate", lowered, out)
        self.assertTrue(
            any(token in lowered for token in role_tokens),
            f"diagnostic must name the input role {role_tokens}: {out}",
        )
        self.assertNotIn("Traceback", out)
        for value in forbidden:
            self.assertNotIn(value, out)

    # -- duplicates in environment fixtures ------------------------------

    def test_duplicate_staging_member_with_valid_final_value_rejected(self):
        staging = """{
  "auth.rp_id": "sentinel-first-value-7f3a",
  "session.ttl_secs": 60,
  "auth.rp_id": "staging.example.invalid"
}
"""
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(tmp, staging_text=staging)
        self.assert_duplicate_input_error(
            code, out, ("staging",),
            forbidden=("sentinel-first-value-7f3a",
                       "staging.example.invalid"),
        )

    def test_duplicate_prod_member_with_valid_final_value_rejected(self):
        prod = """{
  "session.ttl_secs": 999999,
  "auth.rp_id": "prod.example.invalid",
  "session.ttl_secs": 120
}
"""
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(tmp, prod_text=prod)
        self.assert_duplicate_input_error(
            code, out, ("prod", "production"),
            forbidden=("999999", "prod.example.invalid"),
        )

    def test_duplicate_member_rejected_before_semantic_checks(self):
        # The staging doc both duplicates a member AND omits a required
        # key. Duplicate detection happens at decode time, so this is
        # unusable input (exit 2), not semantic drift (exit 1).
        staging = """{
  "session.ttl_secs": 30,
  "session.ttl_secs": 60
}
"""
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(tmp, staging_text=staging)
        self.assert_duplicate_input_error(code, out, ("staging",))

    def test_escape_encoded_duplicate_member_name_rejected(self):
        # "session.ttl\\u005fsecs" decodes to the same member name as
        # "session.ttl_secs"; duplicate means equal after JSON decoding,
        # not byte-equal raw text.
        staging = (
            '{\n'
            '  "session.ttl_secs": 30,\n'
            '  "auth.rp_id": "staging.example.invalid",\n'
            '  "session.ttl\\u005fsecs": 60\n'
            '}\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(tmp, staging_text=staging)
        self.assert_duplicate_input_error(
            code, out, ("staging",),
            forbidden=("staging.example.invalid",),
        )

    # -- duplicates in the schema ----------------------------------------

    def test_duplicate_schema_version_with_valid_final_value_rejected(self):
        schema = VALID_SCHEMA.replace(
            '"schema_version": 1,',
            '"schema_version": 2,\n  "schema_version": 1,',
        )
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(tmp, schema_text=schema)
        self.assert_duplicate_input_error(code, out, ("schema",))

    def test_duplicate_name_under_keys_rejected(self):
        spec = (
            '{\n'
            '      "type": "string",\n'
            '      "default": null,\n'
            '      "required": true,\n'
            '      "env_overridable": true,\n'
            '      "env_var": "RP_ID"\n'
            '    }'
        )
        schema = VALID_SCHEMA.replace(
            '"auth.rp_id": {\n      "type": "string",\n'
            '      "default": null,\n      "required": true,\n'
            '      "env_overridable": true,\n'
            '      "env_var": "RP_ID"\n    }',
            f'"auth.rp_id": {spec},\n    "auth.rp_id": {spec}',
        )
        self.assertIn('"auth.rp_id": {', schema)
        self.assertEqual(schema.count('"auth.rp_id":'), 2, schema)
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(tmp, schema_text=schema)
        self.assert_duplicate_input_error(code, out, ("schema",))

    def test_repeated_nested_spec_field_rejected(self):
        cases = {
            "repeated type": ('"type": "u64",',
                              '"type": "string",\n      "type": "u64",'),
            "repeated default": ('"default": 60,',
                                 '"default": 61,\n      "default": 60,'),
        }
        for label, (old, new) in cases.items():
            with self.subTest(label):
                schema = VALID_SCHEMA.replace(old, new, 1)
                self.assertNotEqual(schema, VALID_SCHEMA)
                with tempfile.TemporaryDirectory() as tmp:
                    code, out = run_check(tmp, schema_text=schema)
                self.assert_duplicate_input_error(code, out, ("schema",))

    # -- diagnostic safety -------------------------------------------------

    def test_diagnostic_bounds_and_escapes_long_control_member_name(self):
        # A duplicated member name that is very long and, after JSON
        # decoding, contains a newline and a quote. If the diagnostic
        # shows the name at all, it must be escaped (the decoded raw
        # name, with its literal newline, must not appear) and bounded
        # (the full 300-char run must not appear). Its values must not
        # be printed either.
        long_run = "x" * 300
        raw_name = f'bad\\nname\\"{long_run}'
        decoded_name = 'bad\nname"' + long_run
        staging = (
            '{\n'
            f'  "{raw_name}": "sentinel-long-name-value-9c1d",\n'
            '  "session.ttl_secs": 60,\n'
            '  "auth.rp_id": "staging.example.invalid",\n'
            f'  "{raw_name}": "sentinel-long-name-value-9c1d"\n'
            '}\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(tmp, staging_text=staging)
        self.assert_duplicate_input_error(
            code, out, ("staging",),
            forbidden=("sentinel-long-name-value-9c1d",
                       "staging.example.invalid",
                       decoded_name,
                       long_run),
        )
        # The escaped, bounded rendering must actually be shown: absence
        # assertions alone pass even when the name is never displayed.
        # (32 mirrors MEMBER_NAME_DISPLAY_MAX in config_parity_check.py;
        # changing the bound must update this expectation deliberately.)
        self.assertIn(json.dumps(decoded_name[:32]) + "...", out)

    # -- controls: existing exit-code contract is preserved ---------------

    def test_valid_distinct_per_environment_values_accepted(self):
        # Positive control. Also proves scope: the same member names in
        # different objects ("type"/"default" in each keys spec) and in
        # different files (staging vs prod) are not duplicates.
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(tmp)
        self.assertEqual(code, 0, out)

    def test_semantic_drift_stays_exit_1(self):
        staging = """{
  "session.ttl_secs": "soon",
  "auth.rp_id": "staging.example.invalid"
}
"""
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(tmp, staging_text=staging)
        self.assertEqual(code, 1, out)

    def test_malformed_json_stays_exit_2(self):
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(tmp, staging_text="{ not json\n")
        self.assertEqual(code, 2, out)
        self.assertNotIn("Traceback", out)


if __name__ == "__main__":
    unittest.main(verbosity=2)
