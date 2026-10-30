#!/usr/bin/env python3
"""Require and execute the production-seam tests on disposable PostgreSQL.

Ordinary native tests do not require a database. This runner opts in to the
ignored PostgreSQL tests and fails if the private test module was not compiled,
if a test disappeared, if the disposable URL is missing, or if a test fails.
GitHub-hosted CI uses Cargo directly, like the other CI jobs; local invocations
must use the repository's bounded Cargo pool.
"""

import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
PREFIX = "worker_entry::atomic_registration_store_acceptance::"
EXPECTED = {
    PREFIX + name
    for name in (
        "postgres_registration_commit_without_invite_is_single_use",
        "postgres_registration_commit_consumes_optional_invite",
        "postgres_registration_duplicate_rolls_back_and_same_proofs_retry",
        "postgres_registration_zero_row_magic_claim_rolls_back_earlier_challenge",
        "postgres_registration_challenge_loser_spends_no_later_proof",
        "postgres_registration_invite_loser_rolls_back_both_earlier_claims",
        "postgres_registration_competing_transactions_enroll_exactly_one_credential",
        "postgres_challenge_cleanup_skips_locked_expired_row_and_retries",
        "postgres_reap_before_prune_preserves_in_flight_enrollment",
        "postgres_reap_after_enrollment_rollback_cleans_expired_signup",
        "postgres_reap_delete_rechecks_locked_candidates_before_deleting",
        "postgres_reap_bounded_batch_drains_cap_plus_one",
        "postgres_reap_preserves_explicit_protected_user_and_dependents",
        "postgres_magic_issuance_skips_locked_expired_proof_and_retries",
        "postgres_reap_holds_selected_parent_lock_through_delete",
    )
}
EXPECTED_CONFIG = {
    PREFIX + name
    for name in (
        "disposable_database_config_accepts_standard_loopback_service",
        "disposable_database_config_rejects_hostaddr_overrides",
        "disposable_database_config_rejects_wrong_or_missing_database",
        "disposable_database_config_rejects_nonloopback_and_multiple_hosts",
    )
}


def _parse_listing(output):
    return {
        line.removesuffix(": test")
        for line in output.splitlines()
        if line.startswith(PREFIX) and line.endswith(": test")
    }


def _require_config_bank_green(result, expected):
    output = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", result.stdout or "")
    if result.returncode != 0:
        raise RuntimeError(
            "disposable config bank did not pass\n" + output
        )
    if "test result: ok. 4 passed; 0 failed; 0 ignored;" not in output:
        raise RuntimeError(
            "disposable config bank did not report exactly 4 passing\n" + output
        )
    for target in sorted(expected):
        if not re.search(r"(?m)^test " + re.escape(target) + r" \.\.\. ok$", output):
            raise RuntimeError(
                f"disposable config case did not execute exactly once and pass: {target}\n" + output
            )
    return output


def main():
    if not os.environ.get("NNTUNE_TEST_DATABASE_URL"):
        sys.exit("NNTUNE_TEST_DATABASE_URL is required; PostgreSQL acceptance cannot skip")
    cargo = (
        ["cargo"]
        if os.environ.get("GITHUB_ACTIONS") == "true"
        else [sys.executable, "scripts/cargo_cache.py", "run", "--"]
    )
    command = cargo + [
        "test", "--locked", "--features", "postgres-tests",
        "--manifest-path", "workers/api/Cargo.toml", "--lib", PREFIX,
    ]
    # Normal listing may return every compiled test matching the prefix or only
    # the non-ignored subset, depending on the libtest version. Subtracting the
    # ignored inventory yields the native config subset in either case.
    listed_normal = subprocess.run(
        command + ["--", "--list"],
        cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
    ).stdout
    actual_config = _parse_listing(listed_normal) - EXPECTED
    if actual_config != EXPECTED_CONFIG:
        missing = ", ".join(sorted(EXPECTED_CONFIG - actual_config)) or "none"
        extra = ", ".join(sorted(actual_config - EXPECTED_CONFIG)) or "none"
        sys.exit(f"PostgreSQL config module mismatch; missing: {missing}; extra: {extra}")
    listed = subprocess.run(
        command + ["--", "--list", "--ignored"],
        cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
    ).stdout
    actual = _parse_listing(listed)
    if actual != EXPECTED:
        missing = ", ".join(sorted(EXPECTED - actual)) or "none"
        extra = ", ".join(sorted(actual - EXPECTED)) or "none"
        sys.exit(f"PostgreSQL acceptance module mismatch; missing: {missing}; extra: {extra}")
    print(f"PostgreSQL config: executing {len(actual_config)} native guard tests", flush=True)
    config_command = command[:-1] + [PREFIX + "disposable_database_config_"]
    config_result = subprocess.run(
        config_command + ["--", "--nocapture", "--test-threads=1"],
        cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        check=False,
    )
    config_output = _require_config_bank_green(config_result, EXPECTED_CONFIG)
    print(config_output, flush=True)
    print(f"PostgreSQL acceptance: executing {len(actual)} production-seam tests", flush=True)
    try:
        subprocess.run(
            command + ["--", "--ignored", "--nocapture", "--test-threads=1"],
            cwd=ROOT, check=True,
        )
    except subprocess.CalledProcessError:
        # Blind batching tests intentionally start RED. Prove the independent
        # protected-id mutant in the same disposable job, without skipping or
        # converting the original bank failure into a successful check.
        if (
            os.environ.get("GITHUB_ACTIONS") == "true"
            and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
        ):
            subprocess.run(
                [sys.executable, "workers/api/tests/run_atomic_registration_mutation_tests.py", "--protected-only"],
                cwd=ROOT, check=True,
            )
        raise


if __name__ == "__main__":
    main()
