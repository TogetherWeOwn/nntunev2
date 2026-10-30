#!/usr/bin/env python3
"""Fail-closed and restoration tests for the disposable mutation harness."""

import contextlib
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import run_atomic_registration_mutation_tests as runner


TARGET = "worker_entry::atomic_registration_store_acceptance::postgres_registration_commit_consumes_optional_invite"
REASON = "challenge, magic, invite, credential count"
FAILURE = (
    "running 1 test\n"
    f"test {TARGET} ... FAILED\n"
    f"assertion `left == right` failed: {REASON}\n"
    "  left: (true, true, false, 1)\n"
    " right: (true, true, true, 1)\n"
    f"\nfailures:\n    {TARGET}\n"
    "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 339 filtered out;\n"
)
SOURCE_FIXTURE = b"""async fn unrelated() {
    claims.push(SingleUseClaim::Invite { record });
}
async fn complete_registration_on(
) {
    claims.push(SingleUseClaim::Invite { record });
    if claim_single_use_on(&transaction, claim).await? != 1 {}
}
async fn cleanup() {}
"""

REAPER_SOURCE_FIXTURE = b"""async fn complete_registration_on(
) {
    let protected_user_id = None;
}
async fn reap_unverified_users_on(
    protected_user_id: Option<&str>,
) -> Result<(), Error> {
    let cutoff = 0;
    if cutoff == 0 {
        use_binding(protected_user_id);
    }
}
async fn cleanup() {}
"""


class MutationRunnerTests(unittest.TestCase):
    def test_protected_user_mutation_targets_only_the_real_reaper_binding(self):
        mutations = [mutation for mutation in runner.MUTATIONS if mutation[0] == "ignored-protected-user-binding"]
        self.assertEqual(len(mutations), 1, "the protected-user negative control must not be silently absent")
        _, old, new, target, reason, signature = mutations[0]
        self.assertEqual(target, runner.PREFIX + "postgres_reap_preserves_explicit_protected_user_and_dependents")
        self.assertEqual(reason, "protected signup must survive the production reaper")
        changed = runner.mutated_source(REAPER_SOURCE_FIXTURE, old, new, signature)
        self.assertEqual(changed, b"""async fn complete_registration_on(
) {
    let protected_user_id = None;
}
async fn reap_unverified_users_on(
    protected_user_id: Option<&str>,
) -> Result<(), Error> {
    let protected_user_id = protected_user_id.map(|_| "");
    let cutoff = 0;
    if cutoff == 0 {
        use_binding(protected_user_id);
    }
}
async fn cleanup() {}
""")

    def test_protected_user_mutation_restores_bytes_after_unrelated_failure(self):
        mutations = [mutation for mutation in runner.MUTATIONS if mutation[0] == "ignored-protected-user-binding"]
        self.assertEqual(len(mutations), 1, "the protected-user negative control must not be silently absent")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / runner.SOURCE
            source.parent.mkdir(parents=True)
            source.write_bytes(REAPER_SOURCE_FIXTURE)
            environment = {
                "GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                "NNTUNE_TEST_DATABASE_URL": "postgresql://localhost/nntune_ci",
            }
            with patch.dict(os.environ, environment, clear=True), patch.object(runner, "ROOT", root):
                with patch.object(runner, "MUTATIONS", mutations), patch.object(
                    runner.subprocess, "run",
                    return_value=subprocess.CompletedProcess([], 101, stdout="connection refused"),
                ):
                    with self.assertRaisesRegex(RuntimeError, "did not reach its expected assertion"):
                        runner.main()
            self.assertEqual(source.read_bytes(), REAPER_SOURCE_FIXTURE)

    def test_protected_only_control_requires_a_named_red_then_restored_green(self):
        target = runner.PREFIX + "postgres_reap_preserves_explicit_protected_user_and_dependents"
        reason = "protected signup must survive the production reaper"
        red = FAILURE.replace(TARGET, target).replace(REASON, reason)
        green = f"running 1 test\ntest {target} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;"
        for code, output, succeeds in (
            (0, green, True),
            (0, "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored;", False),
            (0, green.replace(target, "another_case"), False),
            (101, "error[E0425]: compilation failed", False),
        ):
            with self.subTest(code=code, output=output), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source = root / runner.SOURCE
                source.parent.mkdir(parents=True)
                source.write_bytes(REAPER_SOURCE_FIXTURE)
                calls = []

                def fake_run(command, **kwargs):
                    calls.append(command)
                    self.assertIn(target, command)
                    self.assertIn("--exact", command)
                    if len(calls) == 1:
                        self.assertIn(b'protected_user_id.map(|_| "")', source.read_bytes())
                        return subprocess.CompletedProcess(command, 101, stdout=red)
                    self.assertEqual(len(calls), 2, "a protected-only control must not recurse into the RED bank")
                    self.assertEqual(source.read_bytes(), REAPER_SOURCE_FIXTURE)
                    return subprocess.CompletedProcess(command, code, stdout=output)

                environment = {
                    "GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                    "NNTUNE_TEST_DATABASE_URL": "postgresql://localhost/nntune_ci",
                }
                with patch.dict(os.environ, environment, clear=True), patch.object(runner, "ROOT", root):
                    with patch.object(runner.subprocess, "run", fake_run):
                        if succeeds:
                            runner.main(protected_only=True)
                        else:
                            with self.assertRaisesRegex(RuntimeError, "did not execute exactly once and pass"):
                                runner.main(protected_only=True)
                self.assertEqual(len(calls), 2)
                self.assertEqual(source.read_bytes(), REAPER_SOURCE_FIXTURE)

    def test_accepts_only_the_named_runtime_assertion_failure(self):
        result = subprocess.CompletedProcess([], 101, stdout=FAILURE)
        self.assertIn("1 failed", runner.require_assertion_failure(result, TARGET, REASON))

    def test_rejects_compiler_failure_survival_zero_tests_and_wrong_assertions(self):
        cases = (
            (101, "error[E0425]: compilation failed"),
            (0, FAILURE),
            (0, "running 0 tests\ntest result: ok. 0 passed; 0 failed;"),
            (101, FAILURE.replace(TARGET, "different_test")),
            (101, FAILURE.replace(REASON, "connection refused")),
            (101, FAILURE.replace("0 ignored", "1 ignored")),
        )
        for code, output in cases:
            with self.subTest(code=code, output=output):
                with self.assertRaisesRegex(RuntimeError, "did not reach its expected assertion"):
                    runner.require_assertion_failure(
                        subprocess.CompletedProcess([], code, stdout=output), TARGET, REASON,
                    )

    def test_changes_only_the_selected_production_helper(self):
        changed = runner.mutated_source(
            SOURCE_FIXTURE, "claims.push(SingleUseClaim::Invite { record });", "let _ = record;",
        )
        self.assertEqual(changed, b"""async fn unrelated() {
    claims.push(SingleUseClaim::Invite { record });
}
async fn complete_registration_on(
) {
    let _ = record;
    if claim_single_use_on(&transaction, claim).await? != 1 {}
}
async fn cleanup() {}
""")

    def test_missing_or_duplicate_mutation_site_is_not_silently_skipped(self):
        for original in (SOURCE_FIXTURE.replace(b" != 1", b" > 1"), SOURCE_FIXTURE.replace(b" != 1", b" != 1 != 1")):
            with self.subTest(original=original):
                with self.assertRaises(RuntimeError):
                    runner.mutated_source(original, " != 1", " > 1")

    def test_cannot_modify_a_local_checkout(self):
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}, clear=True):
            with self.assertRaisesRegex(SystemExit, "ephemeral GitHub-hosted"):
                runner.main()

    def test_database_configuration_is_required_before_any_source_mutation(self):
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}, clear=True):
            with self.assertRaisesRegex(SystemExit, "DATABASE_URL is required"):
                runner.main()

    def test_restores_original_bytes_after_unrelated_or_timeout_failure(self):
        for failure in (
            subprocess.CompletedProcess([], 101, stdout="error[E0425]: compilation failed"),
            subprocess.TimeoutExpired("cargo", 180),
        ):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                source = root / runner.SOURCE
                source.parent.mkdir(parents=True)
                source.write_bytes(SOURCE_FIXTURE)
                environment = {
                    "GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                    "NNTUNE_TEST_DATABASE_URL": "postgresql://localhost/nntune_ci",
                }
                options = {"side_effect": failure} if isinstance(failure, Exception) else {"return_value": failure}
                with patch.dict(os.environ, environment, clear=True), patch.object(runner, "ROOT", root):
                    with patch.object(runner.subprocess, "run", **options):
                        with self.assertRaises((RuntimeError, subprocess.TimeoutExpired)):
                            runner.main()
                self.assertEqual(source.read_bytes(), SOURCE_FIXTURE)


if __name__ == "__main__":
    with contextlib.redirect_stdout(io.StringIO()):
        unittest.main()
