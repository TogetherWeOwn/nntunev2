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
    )
}


def main():
    if not os.environ.get("NNTUNE_TEST_DATABASE_URL"):
        sys.exit("NNTUNE_TEST_DATABASE_URL is required; PostgreSQL acceptance cannot skip")
    cargo = (
        ["cargo"]
        if os.environ.get("GITHUB_ACTIONS") == "true"
        else [sys.executable, "scripts/cargo_cache.py", "run", "--"]
    )
    command = cargo + [
        "test", "--locked", "--manifest-path", "workers/api/Cargo.toml", "--lib", PREFIX,
    ]
    listed = subprocess.run(
        command + ["--", "--list", "--ignored"],
        cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
    ).stdout
    actual = {
        line.removesuffix(": test")
        for line in listed.splitlines()
        if line.startswith(PREFIX) and line.endswith(": test")
    }
    if actual != EXPECTED:
        missing = ", ".join(sorted(EXPECTED - actual)) or "none"
        extra = ", ".join(sorted(actual - EXPECTED)) or "none"
        sys.exit(f"PostgreSQL acceptance module mismatch; missing: {missing}; extra: {extra}")
    print(f"PostgreSQL acceptance: executing {len(actual)} production-seam tests", flush=True)
    subprocess.run(
        command + ["--", "--ignored", "--nocapture", "--test-threads=1"],
        cwd=ROOT, check=True,
    )


if __name__ == "__main__":
    main()
