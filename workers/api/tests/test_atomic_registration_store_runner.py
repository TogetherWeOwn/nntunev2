#!/usr/bin/env python3
"""Inventory-guard tests for the PostgreSQL acceptance bank runner.

The runner must compare the compiled, ignored-test inventory against its
explicit EXPECTED enumeration and refuse to execute anything when they
differ. Deleting, bypassing or negating that `actual != EXPECTED` guard must
turn the rejection cases below RED: a missing, empty or extra inventory
would otherwise execute (or silently skip) database tests while still
reporting success.
"""

import contextlib
import io
import os
import subprocess
import unittest
from unittest.mock import patch

import run_atomic_registration_store_tests as runner


ENVIRONMENT = {
    "GITHUB_ACTIONS": "true",
    "NNTUNE_TEST_DATABASE_URL": "postgresql://localhost/nntune_ci",
}


def listing(names):
    lines = [f"{name}: test" for name in sorted(names)]
    lines.append("")
    lines.append(f"{len(names)} tests, 0 benchmarks")
    return "\n".join(lines) + "\n"


class StoreRunnerInventoryGuardTests(unittest.TestCase):
    def run_main(self, listed_names):
        calls = []

        def fake_run(command, **kwargs):
            calls.append(command)
            if "--list" in command:
                return subprocess.CompletedProcess(command, 0, stdout=listing(listed_names))
            return subprocess.CompletedProcess(command, 0)

        with patch.dict(os.environ, ENVIRONMENT, clear=True):
            with patch.object(runner.subprocess, "run", fake_run):
                with contextlib.redirect_stdout(io.StringIO()):
                    try:
                        runner.main()
                    except SystemExit as exit_error:
                        return calls, exit_error
        return calls, None

    def assert_rejected_before_execution(self, listed_names):
        calls, exit_error = self.run_main(listed_names)
        self.assertIsNotNone(exit_error, "inventory mismatch must exit non-zero")
        self.assertIn("PostgreSQL acceptance module mismatch", str(exit_error))
        self.assertEqual(len(calls), 1, "only the inventory listing may run")
        self.assertIn("--list", calls[0], "the sole command must be the listing")
        self.assertFalse(
            any("--ignored" in call and "--list" not in call for call in calls),
            "no execution command may start after an inventory mismatch",
        )

    def test_missing_inventory_is_rejected_before_execution(self):
        missing = set(runner.EXPECTED)
        missing.pop()
        self.assert_rejected_before_execution(missing)

    def test_empty_inventory_is_rejected_before_execution(self):
        self.assert_rejected_before_execution(set())

    def test_extra_inventory_is_rejected_before_execution(self):
        extra = set(runner.EXPECTED)
        extra.add(runner.PREFIX + "postgres_unlisted_surprise")
        self.assert_rejected_before_execution(extra)

    def test_exact_expected_inventory_executes_the_bank(self):
        calls, exit_error = self.run_main(set(runner.EXPECTED))
        self.assertIsNone(exit_error, "the exact inventory must not be rejected")
        self.assertEqual(len(calls), 2, "listing then one execution command")
        self.assertIn("--list", calls[0])
        self.assertIn("--ignored", calls[1])
        self.assertNotIn("--list", calls[1])
        self.assertIn(runner.PREFIX, calls[1])

    def test_missing_database_url_starts_no_command(self):
        calls = []
        environment = dict(ENVIRONMENT)
        del environment["NNTUNE_TEST_DATABASE_URL"]
        with patch.dict(os.environ, environment, clear=True):
            with patch.object(
                runner.subprocess,
                "run",
                lambda command, **kwargs: calls.append(command),
            ):
                with self.assertRaisesRegex(SystemExit, "DATABASE_URL is required"):
                    runner.main()
        self.assertEqual(calls, [], "no listing or execution without a database URL")


if __name__ == "__main__":
    unittest.main()
