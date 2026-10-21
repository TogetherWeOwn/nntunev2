#!/usr/bin/env python3
"""Acceptance tests: duplicate-member diagnostic name truncation bounds.

Contract for the Builder: the duplicate-member diagnostic shows the
member name escaped (``json.dumps(..., ensure_ascii=True)``) and
length-bounded at ``MEMBER_NAME_DISPLAY_MAX = 32`` characters of raw
name, appending ``...`` OUTSIDE the quotes when truncated. Member
values, raw JSON, and exception content never appear. Short names
render in full with no ellipsis; hostile multi-kilobyte names render
bounded; the empty member name is unaffected (renders ``""``).

Measured boundary on the current checker (``config_parity_check.py``):
a 32-character name renders in full with no ellipsis, while a
33-character name is truncated to its first 32 characters plus ``...``.
This suite pins both sides of that boundary, so either loosening the
bound (long names flood the log) or tightening it (short names gain a
spurious ellipsis) turns red. The ``...`` marker is a display suffix,
not JSON: only the quoted prefix parses (``json.loads`` of the prefix
yields the first 32 raw characters).

All duplicate fixtures below are raw JSON text (never dicts via
json.dumps): a serialized dict cannot carry duplicate member names.
Temporary fixture dirs only; the shipped ``config/`` fixtures stay
read-only.

Run from the repo root:

  python3 .github/scripts/test_config_diagnostic_truncation.py
"""

import json
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github" / "scripts" / "config_parity_check.py"

# Bound mirrored from the checker: assertions below pin this exact
# value, so any change to the checker's cap fails here first.
MEMBER_NAME_DISPLAY_MAX = 32

VALID_SCHEMA = """{
  "schema_version": 1,
  "description": "synthetic truncation-fixture schema",
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
    schema = Path(tmp) / "schema.json"
    staging = Path(tmp) / "staging.json"
    prod = Path(tmp) / "prod.json"
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


def duplicate_staging(member_json, first_value="1", last_value="2"):
    """Staging doc duplicating one raw-JSON member name (valid scalars)."""
    return (
        '{\n'
        f'  {member_json}: {first_value},\n'
        '  "session.ttl_secs": 60,\n'
        '  "auth.rp_id": "staging.example.invalid",\n'
        f'  {member_json}: {last_value}\n'
        '}\n'
    )


def rendered_name(output):
    """Extract the rendered member-name token from the diagnostic line."""
    match = re.search(r"duplicate JSON member name (.*): \S+\s*$",
                      output, re.MULTILINE)
    assert match is not None, f"no duplicate diagnostic found: {output}"
    return match.group(1)


class DiagnosticTruncationTests(unittest.TestCase):
    def assert_duplicate_exit_2(self, code, out):
        self.assertEqual(code, 2, out)
        self.assertIn("duplicate", out.lower(), out)
        self.assertNotIn("Traceback", out)

    def test_32_char_name_renders_in_full_with_no_ellipsis(self):
        name = "n" * 32
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(
                tmp, staging_text=duplicate_staging(json.dumps(name)))
        self.assert_duplicate_exit_2(code, out)
        shown = rendered_name(out)
        self.assertEqual(json.dumps(name), shown, out)
        self.assertFalse(shown.endswith("..."), out)

    def test_33_char_name_truncated_to_32_chars_plus_ellipsis(self):
        name = "n" * 33
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(
                tmp, staging_text=duplicate_staging(json.dumps(name)))
        self.assert_duplicate_exit_2(code, out)
        shown = rendered_name(out)
        self.assertEqual(json.dumps(name[:32]) + "...", shown, out)
        self.assertTrue(shown.endswith("..."), out)
        # The full 33-char quoted name must not appear anywhere.
        self.assertNotIn(json.dumps(name), out)

    def test_10000_char_name_bounded_ending_ellipsis_valid_json(self):
        name = "a" * 10_000
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(
                tmp, staging_text=duplicate_staging(json.dumps(name)))
        self.assert_duplicate_exit_2(code, out)
        shown = rendered_name(out)
        self.assertTrue(shown.endswith("..."), out)
        # Bounded: far below the ~64-char card ceiling (currently 37).
        self.assertLessEqual(len(shown), 64, out)
        # Valid JSON quoting: the quoted prefix parses to the first
        # 32 raw characters; the "..." suffix stays outside the quotes.
        core = shown[:-3]
        self.assertEqual(name[:32], json.loads(core), out)
        # The hostile run itself must not leak into the log.
        self.assertNotIn(name, out)
        self.assertNotIn("a" * 100, out)

    def test_truncation_preserves_escaping(self):
        # Special characters inside the shown window stay escaped, and
        # the decoded raw name (with its literal newline) never appears.
        name = 'k"e\\y\n' + "z" * 10_000
        self.assertGreater(len(name), MEMBER_NAME_DISPLAY_MAX)
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(
                tmp, staging_text=duplicate_staging(
                    json.dumps(name, ensure_ascii=True)))
        self.assert_duplicate_exit_2(code, out)
        shown = rendered_name(out)
        self.assertTrue(shown.endswith("..."), out)
        self.assertLessEqual(len(shown), 64, out)
        self.assertEqual(name[:32], json.loads(shown[:-3]), out)
        self.assertNotIn(name, out)

    def test_empty_member_name_unaffected(self):
        # A duplicated empty-string member is still a duplicate (exit 2)
        # and its rendering is unaffected by truncation: '""', no ellipsis.
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(
                tmp, staging_text=duplicate_staging('""'))
        self.assert_duplicate_exit_2(code, out)
        self.assertEqual('""', rendered_name(out), out)

    def test_valid_documents_accepted_control(self):
        with tempfile.TemporaryDirectory() as tmp:
            code, out = run_check(tmp)
        self.assertEqual(code, 0, out)


if __name__ == "__main__":
    unittest.main(verbosity=2)
