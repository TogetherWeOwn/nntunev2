#!/usr/bin/env python3
"""Require the complete production-adapter preparation bank on Actions.

This is not a local CI mirror. Focused local reproduction of one failing case
uses the separately governed build path, not this full acceptance-bank runner.
"""

import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
EXPECTED = {
    "disposable_config_accepts_only_single_loopback_nntune_ci",
    "disposable_config_rejects_hostaddr_nonloopback_multihost_and_wrong_database",
    "postgres_absent_admission_persists_exact_bundle_with_clean_health",
    "postgres_same_operation_recovers_exact_bundle_and_fresh_mint_is_rejected",
    "postgres_competing_jobs_for_one_absent_hash_produce_one_candidate",
    "postgres_stale_observation_is_rejected_before_any_custody_insert",
    "postgres_attempt_lease_owner_and_status_fence_every_mutation",
    "postgres_invalid_partial_parameters_fail_before_any_insertion",
    "postgres_managed_custody_is_immutable_and_legacy_id_health_writes_are_rejected",
    "postgres_schema_replay_preserves_admitted_bundle_and_foreign_keys_reject_wrong_association",
}


def main():
    if (
        os.environ.get("GITHUB_ACTIONS") != "true"
        or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
    ):
        sys.exit("The full posted-generation bank runs only on GitHub-hosted Actions")
    if not os.environ.get("NNTUNE_TEST_DATABASE_URL"):
        sys.exit("NNTUNE_TEST_DATABASE_URL is required; the bank cannot skip")
    command = [
        "cargo", "test", "--locked", "--manifest-path", "workers/api/Cargo.toml",
        "--features", "postgres-tests", "--test", "posted_generations_postgres",
    ]
    listing = subprocess.run(
        command + ["--", "--list"], cwd=ROOT, text=True,
        stdout=subprocess.PIPE, check=True,
    ).stdout
    actual = {
        line.removesuffix(": test") for line in listing.splitlines()
        if line.endswith(": test")
    }
    if actual != EXPECTED:
        sys.exit("Posted-generation test inventory changed or did not compile completely")
    result = subprocess.run(
        command + ["--", "--test-threads=1"], cwd=ROOT, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False, timeout=180,
    )
    output = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", result.stdout or "")
    print(output, flush=True)
    if result.returncode != 0:
        sys.exit("Posted-generation production-adapter bank failed")
    if f"test result: ok. {len(EXPECTED)} passed; 0 failed; 0 ignored;" not in output:
        sys.exit("Posted-generation bank did not execute every required test")
    for name in EXPECTED:
        if not re.search(r"(?m)^test " + re.escape(name) + r" \.\.\. ok$", output):
            sys.exit("A required posted-generation case did not execute and pass")
    print(f"Posted-generation preparation: {len(EXPECTED)} required cases passed", flush=True)


if __name__ == "__main__":
    main()
