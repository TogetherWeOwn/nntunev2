#!/usr/bin/env python3
"""Acceptance tests: wrangler binding parity check across workers and envs.

These tests are RED until the paired build card lands
`.github/scripts/check_wrangler_binding_parity.py`. They pin only the
script's observable contract, not its internals:

* `check_parity(workers_dir, inventory_path)` returns a list of drift
  strings; empty means parity.
* `workers_dir` holds one subdir per worker, each with a `wrangler.toml`
  using real wrangler syntax (top-level blocks describe staging,
  `[env.production.*]` overrides describe prod).
* `inventory_path` is a markdown file holding an environments-matrix
  table in the same shape as the operations runbook's matrix
  (pipe table, one row per concern, staging and prod resource names
  in backticks).
* Every drift string names the worker, the env (`staging`/`prod`),
  and the missing or skewed binding (binding name plus the expected
  resource). No secret values ever appear in drift output.

Run: `python3 .github/scripts/test_wrangler_binding_parity.py`
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from check_wrangler_binding_parity import check_parity

FIXTURES = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "fixtures", "binding-parity"
)


def run_case(case):
    return check_parity(
        os.path.join(FIXTURES, case),
        os.path.join(FIXTURES, "inventory.md"),
    )


class TestBindingParity(unittest.TestCase):
    def test_match_fixtures_pass(self):
        self.assertEqual([], run_case("match"))

    def test_missing_binding_fails_naming_worker_and_env(self):
        errors = run_case("missing-binding")
        self.assertTrue(
            errors, "expected drift errors for the missing-binding fixtures"
        )
        self.assertTrue(
            any("ingest" in e and "staging" in e for e in errors),
            "no drift line names the worker and env: %r" % (errors,),
        )
        self.assertTrue(
            any("INGEST_QUEUE" in e for e in errors),
            "no drift line names the missing binding: %r" % (errors,),
        )

    def test_env_skew_fails_naming_worker_and_env(self):
        errors = run_case("env-skew")
        self.assertTrue(
            errors, "expected drift errors for the env-skew fixtures"
        )
        self.assertTrue(
            any("stream" in e and "prod" in e for e in errors),
            "no drift line names the worker and env: %r" % (errors,),
        )
        self.assertTrue(
            any("AUDIO_CACHE" in e for e in errors),
            "no drift line names the skewed binding: %r" % (errors,),
        )


if __name__ == "__main__":
    unittest.main()
