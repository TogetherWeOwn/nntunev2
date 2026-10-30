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


def all_prefix_names():
    return set(runner.EXPECTED) | set(runner.EXPECTED_CONFIG)


def config_green_output():
    lines = ["running 4 tests"]
    for name in sorted(runner.EXPECTED_CONFIG):
        lines.append(f"test {name} ... ok")
    lines.append("")
    lines.append("test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 339 filtered out;")
    return "\n".join(lines) + "\n"


class StoreRunnerInventoryGuardTests(unittest.TestCase):
    def run_main(self, normal_names, ignored_names=None, config_result=None, db_effect=None):
        if ignored_names is None:
            ignored_names = set(runner.EXPECTED)
        if config_result is None:
            config_result = (0, config_green_output())
        calls = []

        def fake_run(command, **kwargs):
            calls.append(command)
            if "--list" in command:
                if "--ignored" in command:
                    return subprocess.CompletedProcess(command, 0, stdout=listing(ignored_names))
                return subprocess.CompletedProcess(command, 0, stdout=listing(normal_names))
            if "--ignored" in command:
                if isinstance(db_effect, Exception):
                    raise db_effect
                if isinstance(db_effect, subprocess.CompletedProcess):
                    return db_effect
                return subprocess.CompletedProcess(command, 0)
            if isinstance(config_result, Exception):
                raise config_result
            code, stdout = config_result
            return subprocess.CompletedProcess(command, code, stdout=stdout)

        with patch.dict(os.environ, ENVIRONMENT, clear=True):
            with patch.object(runner.subprocess, "run", fake_run):
                with contextlib.redirect_stdout(io.StringIO()):
                    try:
                        runner.main()
                    except SystemExit as exit_error:
                        return calls, exit_error, None
                    except (RuntimeError, subprocess.CalledProcessError) as run_error:
                        return calls, None, run_error
        return calls, None, None

    def assert_rejected_before_execution(self, listed_names):
        calls, exit_error, run_error = self.run_main(all_prefix_names(), listed_names)
        self.assertIsNone(run_error)
        self.assertIsNotNone(exit_error, "inventory mismatch must exit non-zero")
        self.assertIn("PostgreSQL acceptance module mismatch", str(exit_error))
        self.assertEqual(len(calls), 2, "only the two inventory listings may run")
        self.assertIn("--list", calls[0], "first call must be the normal listing")
        self.assertNotIn("--ignored", calls[0])
        self.assertIn("--list", calls[1], "second call must be the ignored listing")
        self.assertIn("--ignored", calls[1])
        self.assertFalse(
            any("--list" not in call for call in calls),
            "no execution command may start after an inventory mismatch",
        )

    def assert_config_rejected_before_execution(self, normal_names):
        calls, exit_error, run_error = self.run_main(normal_names, set(runner.EXPECTED))
        self.assertIsNone(run_error)
        self.assertIsNotNone(exit_error, "config mismatch must exit non-zero")
        self.assertIn("PostgreSQL config module mismatch", str(exit_error))
        self.assertEqual(len(calls), 1, "only the normal listing may run")
        self.assertIn("--list", calls[0], "the sole command must be the listing")
        self.assertNotIn("--ignored", calls[0])
        self.assertFalse(
            any("--list" not in call for call in calls),
            "no execution command may start after a config mismatch",
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

    def test_expected_inventory_has_fifteen_db_cases_and_four_config_cases(self):
        self.assertEqual(len(runner.EXPECTED), 15)
        self.assertIn(
            runner.PREFIX + "postgres_magic_issuance_skips_locked_expired_proof_and_retries",
            runner.EXPECTED,
        )
        self.assertIn(
            runner.PREFIX + "postgres_reap_holds_selected_parent_lock_through_delete",
            runner.EXPECTED,
        )
        self.assertEqual(len(runner.EXPECTED_CONFIG), 4)
        for name in (
            "disposable_database_config_accepts_standard_loopback_service",
            "disposable_database_config_rejects_hostaddr_overrides",
            "disposable_database_config_rejects_wrong_or_missing_database",
            "disposable_database_config_rejects_nonloopback_and_multiple_hosts",
        ):
            self.assertIn(runner.PREFIX + name, runner.EXPECTED_CONFIG)
        self.assertTrue(runner.EXPECTED.isdisjoint(runner.EXPECTED_CONFIG))

    def test_all_cargo_commands_include_postgres_feature(self):
        calls, exit_error, run_error = self.run_main(all_prefix_names(), set(runner.EXPECTED))
        self.assertIsNone(exit_error)
        self.assertIsNone(run_error)
        self.assertEqual(len(calls), 4)
        for command in calls:
            with self.subTest(command=command):
                self.assertIn("--features", command)
                features_index = command.index("--features")
                self.assertEqual(command[features_index + 1], "postgres-tests")
                self.assertTrue(command[command.index("--lib") + 1].startswith(runner.PREFIX))

    def test_exact_expected_inventory_executes_the_bank(self):
        calls, exit_error, run_error = self.run_main(all_prefix_names(), set(runner.EXPECTED))
        self.assertIsNone(exit_error, "the exact inventory must not be rejected")
        self.assertIsNone(run_error)
        self.assertEqual(len(calls), 4, "normal listing, ignored listing, config bank, db bank")
        self.assertIn("--list", calls[0])
        self.assertNotIn("--ignored", calls[0])
        self.assertIn("--list", calls[1])
        self.assertIn("--ignored", calls[1])
        self.assertNotIn("--list", calls[2])
        self.assertNotIn("--ignored", calls[2])
        self.assertIn(runner.PREFIX + "disposable_database_config_", calls[2])
        self.assertIn("--ignored", calls[3])
        self.assertNotIn("--list", calls[3])
        self.assertIn(runner.PREFIX, calls[3])

    def test_config_execution_filter_excludes_the_ignored_database_bank(self):
        calls, exit_error, run_error = self.run_main(all_prefix_names(), set(runner.EXPECTED))
        self.assertIsNone(exit_error)
        self.assertIsNone(run_error)
        config_filter = calls[2][calls[2].index("--lib") + 1]
        self.assertEqual(config_filter, runner.PREFIX + "disposable_database_config_")
        self.assertTrue(all(config_filter in name for name in runner.EXPECTED_CONFIG))
        self.assertFalse(any(config_filter in name for name in runner.EXPECTED))

    def test_normal_list_minus_ignored_yields_config_in_both_libtest_modes(self):
        for normal_names in (all_prefix_names(), set(runner.EXPECTED_CONFIG)):
            with self.subTest(normal_count=len(normal_names)):
                calls, exit_error, run_error = self.run_main(normal_names, set(runner.EXPECTED))
                self.assertIsNone(exit_error)
                self.assertIsNone(run_error)
                self.assertEqual(len(calls), 4)

    def test_config_missing_inventory_is_rejected_before_execution(self):
        missing_all = set(all_prefix_names())
        missing_all.pop()
        # Ensure a config case is missing, not only an ignored one.
        if missing_all >= set(runner.EXPECTED_CONFIG):
            missing_all.pop()
            while missing_all >= set(runner.EXPECTED_CONFIG):
                missing_all.pop()
        self.assert_config_rejected_before_execution(missing_all)

    def test_config_empty_inventory_is_rejected_before_execution(self):
        self.assert_config_rejected_before_execution(set())

    def test_config_extra_inventory_is_rejected_before_execution(self):
        extra = set(all_prefix_names())
        extra.add(runner.PREFIX + "disposable_database_config_unlisted_surprise")
        self.assert_config_rejected_before_execution(extra)

    def test_config_bank_requires_exact_four_passing(self):
        green = config_green_output()
        cases = (
            (1, green),
            (0, green.replace("4 passed", "3 passed")),
            (0, green.replace("0 failed", "1 failed")),
            (0, "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;"),
            (0, green.replace(sorted(runner.EXPECTED_CONFIG)[0], "other_case")),
        )
        for code, output in cases:
            with self.subTest(code=code, output=output[:80]):
                calls, exit_error, run_error = self.run_main(
                    all_prefix_names(), set(runner.EXPECTED), config_result=(code, output),
                )
                self.assertIsNone(exit_error)
                self.assertIsNotNone(run_error, "config bank must fail closed")
                self.assertIsInstance(run_error, RuntimeError)
                self.assertEqual(len(calls), 3, "listings then config bank only")
                self.assertFalse(
                    any("--ignored" in call and "--list" not in call for call in calls),
                    "no destructive bank may start after a config failure",
                )

    def test_failed_bank_keeps_its_failure_after_hosted_protected_control(self):
        for runner_environment, expected_calls in (("github-hosted", 5), ("self-hosted", 4), ("", 4)):
            with self.subTest(runner_environment=runner_environment):
                calls = []
                bank_failure = subprocess.CalledProcessError(101, ["cargo", "test"])

                def fake_run(command, **kwargs):
                    calls.append(command)
                    if "--list" in command:
                        if "--ignored" in command:
                            return subprocess.CompletedProcess(command, 0, stdout=listing(runner.EXPECTED))
                        return subprocess.CompletedProcess(command, 0, stdout=listing(all_prefix_names()))
                    if "--protected-only" in command:
                        return subprocess.CompletedProcess(command, 0)
                    if "--ignored" in command:
                        raise bank_failure
                    return subprocess.CompletedProcess(command, 0, stdout=config_green_output())

                environment = dict(ENVIRONMENT, RUNNER_ENVIRONMENT=runner_environment)
                with patch.dict(os.environ, environment, clear=True):
                    with patch.object(runner.subprocess, "run", fake_run):
                        with contextlib.redirect_stdout(io.StringIO()):
                            with self.assertRaises(subprocess.CalledProcessError) as failure:
                                runner.main()
                self.assertIs(failure.exception, bank_failure, "diagnostics must never turn a red bank green")
                self.assertEqual(len(calls), expected_calls)
                if runner_environment == "github-hosted":
                    self.assertIn("--protected-only", calls[-1])
                    self.assertIn("workers/api/tests/run_atomic_registration_mutation_tests.py", calls[-1])

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
