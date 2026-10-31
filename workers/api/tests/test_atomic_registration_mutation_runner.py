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

MAGIC_SOURCE_FIXTURE = b"""async fn prune_expired_challenges_on(
) {
    let sql = "DELETE FROM auth_challenges WHERE id IN (SELECT id FOR UPDATE SKIP LOCKED)";
}
impl Store for PgStore {
    async fn insert_magic(&self) {
        let sql = "WITH pruned AS (DELETE FROM magic_link_tokens WHERE user_id = $2 FOR UPDATE SKIP LOCKED)) INSERT";
    }
    async fn magic_by_hash(&self) {
    }
}
async fn after_probe(
) {
    let probe = "async fn insert_magic(";
}
"""

REAPER_RELEASE_FIXTURE = b"""async fn complete_registration_on(
) {
}
async fn reap_unverified_users_on(
    client: &mut Client,
) {
    let transaction = client.build_transaction().start().await?;
    let candidates = transaction.query(&lock_sql(), &[]).await?;
    let ids = candidates
        .iter()
        .map(|row| row.try_get("id"))
        .collect::<Result<Vec<_>, _>>()?;
    #[cfg(all(test, not(target_arch = "wasm32")))]
    reaper_selection_pause().await;
    if !ids.is_empty() {
        transaction.execute(&reap_sql(), &[&ids]).await?;
    }
    transaction.commit().await?;
}
async fn cleanup() {}
"""

SUPPORT_FIXTURE = b"""fn validate_disposable_database_config(
    config: &Config,
) -> Result<(), &'static str> {
    if config.get_dbname() != Some("nntune_ci") {
        return Err("only the explicitly named disposable nntune_ci database is permitted");
    }
    if !config.get_hostaddrs().is_empty() {
        return Err("hostaddr overrides are not permitted for the disposable test service");
    }
    if config.get_hosts().len() != 1 {
        return Err("one disposable test host only");
    }
    Ok(())
}

#[test]
fn disposable_database_config_rejects_hostaddr_overrides() {
}
"""


def _control_by_name(name):
    matches = [control for control in runner.MUTATIONS if control[0] == name]
    assert len(matches) == 1, f"control {name} must be present exactly once"
    return matches[0]


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
                support = root / runner.SUPPORT
                support.parent.mkdir(parents=True, exist_ok=True)
                support.write_bytes(SUPPORT_FIXTURE)
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
                self.assertEqual(support.read_bytes(), SUPPORT_FIXTURE)

    def test_mutation_inventory_has_seven_controls_with_new_reasons(self):
        self.assertEqual(len(runner.MUTATIONS), 7)
        magic = _control_by_name("missing-magic-skip-locked")
        self.assertEqual(magic[3], runner.PREFIX + "postgres_magic_issuance_skips_locked_expired_proof_and_retries")
        self.assertEqual(magic[4], "magic issuance must not wait for locked expired cleanup")
        self.assertEqual(magic[5], "    async fn insert_magic(")
        reaper = _control_by_name("reaper-releases-selected-parent-lock")
        self.assertEqual(reaper[3], runner.PREFIX + "postgres_reap_holds_selected_parent_lock_through_delete")
        self.assertEqual(reaper[4], "reaper must retain selected parent lock through delete")
        self.assertEqual(reaper[5], "async fn reap_unverified_users_on(")
        self.assertIsNotNone(reaper[1], "post-select mutant must use replacement, not entry injection")
        guard = _control_by_name("allowed-hostaddr-override")
        self.assertEqual(guard[3], runner.PREFIX + "disposable_database_config_rejects_hostaddr_overrides")
        self.assertEqual(guard[4], "hostaddr override must be rejected before connecting")
        self.assertEqual(guard[5], "fn validate_disposable_database_config(")

    def test_magic_skip_locked_mutation_targets_only_pgstore_impl(self):
        _, old, new, _, _, signature = _control_by_name("missing-magic-skip-locked")
        changed = runner.mutated_source(MAGIC_SOURCE_FIXTURE, old, new, signature).decode("utf-8")
        # Prune cleanup outside the impl keeps skipping locked rows.
        self.assertIn(
            'DELETE FROM auth_challenges WHERE id IN (SELECT id FOR UPDATE SKIP LOCKED)',
            changed,
        )
        # String-literal probe of the method name is untouched.
        self.assertIn('"async fn insert_magic("', changed)
        # Only the PgStore issuance prune loses SKIP LOCKED.
        self.assertEqual(changed.count("FOR UPDATE SKIP LOCKED"), 1)
        self.assertIn("WHERE user_id = $2 FOR UPDATE))", changed)
        self.assertIn("impl Store for PgStore", changed)

    def test_magic_mutation_rejects_missing_duplicate_actor_or_site(self):
        _, old, new, _, _, signature = _control_by_name("missing-magic-skip-locked")
        cases = (
            MAGIC_SOURCE_FIXTURE.replace(b"impl Store for PgStore", b"impl Store for OtherStore"),
            MAGIC_SOURCE_FIXTURE.replace(
                b"impl Store for PgStore", b"impl Store for PgStore {}\nimpl Store for PgStore"
            ),
            MAGIC_SOURCE_FIXTURE.replace(b"    async fn insert_magic(", b"    async fn insert_magic_renamed("),
            MAGIC_SOURCE_FIXTURE.replace(
                b"    async fn magic_by_hash(", b"    async fn insert_magic("
            ),
            MAGIC_SOURCE_FIXTURE.replace(b"FOR UPDATE SKIP LOCKED", b"FOR UPDATE"),
            MAGIC_SOURCE_FIXTURE.replace(
                b"FOR UPDATE SKIP LOCKED))", b"FOR UPDATE SKIP LOCKED) FOR UPDATE SKIP LOCKED))"
            ),
        )
        for original in cases:
            with self.subTest(original=original[:60]):
                with self.assertRaises(RuntimeError):
                    runner.mutated_source(original, old, new, signature)
        before_actor = (
            b"    async fn insert_magic(&self) {}\n"
            b"impl Store for PgStore {\n"
            b"    async fn other(&self) {}\n"
            b"}\n"
            b"async fn tail() {}\n"
        )
        with self.assertRaises(RuntimeError):
            runner.mutated_source(before_actor, old, new, signature)

    def test_magic_mutation_rejects_method_after_the_pgstore_impl_has_closed(self):
        _, old, new, _, _, signature = _control_by_name("missing-magic-skip-locked")
        outside = b"""impl Store for PgStore {
    async fn unrelated(&self) {}
}
impl Store for OtherStore {
    async fn insert_magic(&self) {
        let sql = "FOR UPDATE SKIP LOCKED";
    }
    async fn magic_by_hash(&self) {}
}
"""
        with self.assertRaisesRegex(RuntimeError, "PgStore|production helper"):
            runner.mutated_source(outside, old, new, signature)

    def test_magic_mutation_leaves_another_stores_same_named_method_unchanged(self):
        _, old, new, _, _, signature = _control_by_name("missing-magic-skip-locked")
        unrelated = b"""impl Store for OtherStore {
    async fn insert_magic(&self) {
        let sql = "FOR UPDATE SKIP LOCKED";
    }
}
"""
        changed = runner.mutated_source(MAGIC_SOURCE_FIXTURE + unrelated, old, new, signature)
        self.assertTrue(changed.endswith(unrelated))
        self.assertIn(b"WHERE user_id = $2 FOR UPDATE))", changed)

    def test_reaper_release_mutation_inserts_commit_and_fresh_transaction_before_pause(self):
        _, old, new, _, _, signature = _control_by_name("reaper-releases-selected-parent-lock")
        changed = runner.mutated_source(REAPER_RELEASE_FIXTURE, old, new, signature).decode("utf-8")
        commit = "transaction.commit().await.map_err(|_| StoreError)?;"
        fresh = (
            "let transaction = client.build_transaction()"
            ".isolation_level(tokio_postgres::IsolationLevel::ReadCommitted)"
            ".start().await.map_err(|_| StoreError)?;"
        )
        pause_cfg = '#[cfg(all(test, not(target_arch = "wasm32")))]'
        pause_call = "reaper_selection_pause().await;"
        commit_index = changed.index(commit)
        fresh_index = changed.index(fresh)
        cfg_index = changed.index(pause_cfg)
        pause_index = changed.index(pause_call)
        self.assertLess(commit_index, fresh_index)
        self.assertLess(fresh_index, cfg_index)
        self.assertLess(cfg_index, pause_index)
        # Candidate selection, recheck SQL and pause stay intact.
        self.assertIn("transaction.query(&lock_sql()", changed)
        self.assertIn(".collect::<Result<Vec<_>, _>>()?;", changed)
        self.assertIn("transaction.execute(&reap_sql()", changed)
        self.assertIn(pause_call, changed)

    def test_reaper_mutation_rejects_missing_or_duplicate_anchor(self):
        _, old, new, _, _, signature = _control_by_name("reaper-releases-selected-parent-lock")
        missing = REAPER_RELEASE_FIXTURE.replace(
            b".collect::<Result<Vec<_>, _>>()?;", b".collect::<Vec<_>>();"
        )
        duplicate = REAPER_RELEASE_FIXTURE.replace(
            b"    if !ids.is_empty() {",
            old.encode("utf-8") + b"\n    if !ids.is_empty() {",
        )
        for original in (missing, duplicate):
            with self.subTest(original=original[:80]):
                with self.assertRaises(RuntimeError):
                    runner.mutated_source(original, old, new, signature)

    def test_hostaddr_mutation_removes_only_guard_block(self):
        control = _control_by_name("allowed-hostaddr-override")
        _, old, new, _, _, signature = control[:6]
        changed = runner.mutated_source(SUPPORT_FIXTURE, old, new, signature).decode("utf-8")
        self.assertNotIn("get_hostaddrs", changed)
        self.assertNotIn("hostaddr overrides are not permitted", changed)
        self.assertIn('config.get_dbname() != Some("nntune_ci")', changed)
        self.assertIn("config.get_hosts().len() != 1", changed)
        self.assertIn("fn disposable_database_config_rejects_hostaddr_overrides", changed)

    def test_hostaddr_mutation_rejects_missing_or_duplicate_guard(self):
        control = _control_by_name("allowed-hostaddr-override")
        _, old, new, _, _, signature = control[:6]
        missing = SUPPORT_FIXTURE.replace(
            b"    if !config.get_hostaddrs().is_empty() {\n"
            b'        return Err("hostaddr overrides are not permitted for the disposable test service");\n'
            b"    }\n",
            b"",
        )
        duplicate = SUPPORT_FIXTURE.replace(
            b"    if config.get_hosts().len()",
            b"    if !config.get_hostaddrs().is_empty() {\n"
            b'        return Err("hostaddr overrides are not permitted for the disposable test service");\n'
            b"    }\n"
            b"    if config.get_hosts().len()",
        )
        renamed = SUPPORT_FIXTURE.replace(
            b"fn validate_disposable_database_config(", b"fn validate_disposable_database_config_renamed("
        )
        for original in (missing, duplicate, renamed):
            with self.subTest(original=original[:80]):
                with self.assertRaises(RuntimeError):
                    runner.mutated_source(original, old, new, signature)

    def test_hostaddr_mutation_uses_support_path_and_native_command(self):
        control = _control_by_name("allowed-hostaddr-override")
        parsed = runner._parse_control(control)
        self.assertEqual(parsed[6], runner.SUPPORT)
        self.assertFalse(parsed[7])
        native = runner._mutant_command(parsed[3], parsed[7])
        self.assertNotIn("--ignored", native)
        self.assertIn("--exact", native)
        self.assertIn("--features", native)
        self.assertEqual(native[native.index("--features") + 1], "postgres-tests")
        for name in ("missing-magic-skip-locked", "reaper-releases-selected-parent-lock"):
            parsed_db = runner._parse_control(_control_by_name(name))
            self.assertEqual(parsed_db[6], runner.SOURCE)
            self.assertTrue(parsed_db[7])
            ignored_command = runner._mutant_command(parsed_db[3], parsed_db[7])
            self.assertIn("--ignored", ignored_command)
            self.assertIn("--exact", ignored_command)

    def test_all_mutant_commands_include_postgres_feature(self):
        for ignored in (True, False):
            command = runner._mutant_command(runner.PREFIX + "case", ignored)
            with self.subTest(ignored=ignored):
                self.assertIn("--features", command)
                self.assertEqual(command[command.index("--features") + 1], "postgres-tests")
                self.assertIn("--exact", command)
                self.assertIn("--nocapture", command)
                self.assertIn("--test-threads=1", command)
                self.assertEqual(command.count("--"), 1)
                if ignored:
                    self.assertIn("--ignored", command)
                else:
                    self.assertNotIn("--ignored", command)

    def test_protected_only_uses_feature_and_ignored_for_both_calls(self):
        target = runner.PREFIX + "postgres_reap_preserves_explicit_protected_user_and_dependents"
        reason = "protected signup must survive the production reaper"
        red = FAILURE.replace(TARGET, target).replace(REASON, reason)
        green = f"running 1 test\ntest {target} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / runner.SOURCE
            source.parent.mkdir(parents=True)
            source.write_bytes(REAPER_SOURCE_FIXTURE)
            calls = []

            def fake_run(command, **kwargs):
                calls.append(command)
                if len(calls) == 1:
                    return subprocess.CompletedProcess(command, 101, stdout=red)
                return subprocess.CompletedProcess(command, 0, stdout=green)

            environment = {
                "GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                "NNTUNE_TEST_DATABASE_URL": "postgresql://localhost/nntune_ci",
            }
            with patch.dict(os.environ, environment, clear=True), patch.object(runner, "ROOT", root):
                with patch.object(runner.subprocess, "run", fake_run):
                    runner.main(protected_only=True)
            self.assertEqual(len(calls), 2)
            for command in calls:
                self.assertIn("--features", command)
                self.assertEqual(command[command.index("--features") + 1], "postgres-tests")
                self.assertIn("--ignored", command)
                self.assertIn("--exact", command)

    def _run_single_control_restoration(self, control_name, fixture, path, failure):
        control = _control_by_name(control_name)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target_file = root / path
            target_file.parent.mkdir(parents=True, exist_ok=True)
            target_file.write_bytes(fixture)
            calls = []

            def fake_run(command, **kwargs):
                calls.append(command)
                if isinstance(failure, Exception):
                    raise failure
                return failure

            environment = {
                "GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                "NNTUNE_TEST_DATABASE_URL": "postgresql://localhost/nntune_ci",
            }
            with patch.dict(os.environ, environment, clear=True), patch.object(runner, "ROOT", root):
                with patch.object(runner, "MUTATIONS", (control,)), patch.object(
                    runner.subprocess, "run", fake_run
                ):
                    with self.assertRaises((RuntimeError, subprocess.TimeoutExpired)):
                        runner.main()
            self.assertEqual(target_file.read_bytes(), fixture)
            self.assertEqual(len(calls), 1)
            self.assertIn("--features", calls[0])
            return calls[0]

    def test_new_controls_restore_bytes_after_wrong_assertion_compile_error_and_timeout(self):
        cases = (
            ("missing-magic-skip-locked", MAGIC_SOURCE_FIXTURE, runner.SOURCE),
            ("reaper-releases-selected-parent-lock", REAPER_RELEASE_FIXTURE, runner.SOURCE),
            ("allowed-hostaddr-override", SUPPORT_FIXTURE, runner.SUPPORT),
        )
        for control_name, fixture, path in cases:
            control = _control_by_name(control_name)
            parsed = runner._parse_control(control)
            target, reason = parsed[3], parsed[4]
            failures = (
                subprocess.CompletedProcess([], 101, stdout=FAILURE.replace(TARGET, target).replace(REASON, "wrong reason")),
                subprocess.CompletedProcess([], 101, stdout="error[E0425]: compilation failed"),
                subprocess.TimeoutExpired("cargo", 180),
            )
            for failure in failures:
                with self.subTest(control=control_name, failure=failure):
                    command = self._run_single_control_restoration(control_name, fixture, path, failure)
                    self.assertIn(target, command)
                    if control_name == "allowed-hostaddr-override":
                        self.assertNotIn("--ignored", command)
                    else:
                        self.assertIn("--ignored", command)

    def test_full_run_restores_both_files_when_second_control_fails(self):
        magic = _control_by_name("missing-magic-skip-locked")
        guard = _control_by_name("allowed-hostaddr-override")
        magic_target = runner._parse_control(magic)[3]
        magic_reason = runner._parse_control(magic)[4]
        red = FAILURE.replace(TARGET, magic_target).replace(REASON, magic_reason)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / runner.SOURCE
            source.parent.mkdir(parents=True)
            source.write_bytes(MAGIC_SOURCE_FIXTURE)
            support = root / runner.SUPPORT
            support.parent.mkdir(parents=True, exist_ok=True)
            support.write_bytes(SUPPORT_FIXTURE)
            calls = []

            def fake_run(command, **kwargs):
                calls.append(command)
                if len(calls) == 1:
                    self.assertIn(magic_target, command)
                    self.assertIn(b"FOR UPDATE))", source.read_bytes())
                    return subprocess.CompletedProcess(command, 101, stdout=red)
                self.assertIn("hostaddr", command[-5] if len(command) >= 5 else " ".join(command))
                return subprocess.CompletedProcess(command, 101, stdout="error[E0425]: compilation failed")

            environment = {
                "GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                "NNTUNE_TEST_DATABASE_URL": "postgresql://localhost/nntune_ci",
            }
            with patch.dict(os.environ, environment, clear=True), patch.object(runner, "ROOT", root):
                with patch.object(runner, "MUTATIONS", (magic, guard)), patch.object(
                    runner.subprocess, "run", fake_run
                ):
                    with self.assertRaisesRegex(RuntimeError, "did not reach its expected assertion"):
                        runner.main()
            self.assertEqual(source.read_bytes(), MAGIC_SOURCE_FIXTURE)
            self.assertEqual(support.read_bytes(), SUPPORT_FIXTURE)
            self.assertEqual(len(calls), 2)


if __name__ == "__main__":
    with contextlib.redirect_stdout(io.StringIO()):
        unittest.main()
