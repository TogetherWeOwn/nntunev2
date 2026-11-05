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
    "posted_generation_health::postgres_finish_requires_every_slot_on_both_sides_and_only_stamps_verified_at",
    "posted_generation_health::postgres_duplicate_and_concurrent_confirmations_are_idempotent_per_slot_and_side",
    "posted_generation_health::postgres_new_round_rotates_token_and_rejects_every_old_health_callback",
    "posted_generation_health::postgres_lease_status_and_attempt_fences_gate_every_health_mutation",
    "posted_generation_health::postgres_lost_ack_recovery_returns_current_health_revision_with_exact_evidence",
    "posted_generation_health::postgres_invalid_slots_are_rejected_without_mutation_or_burning_the_handle",
    "posted_generation_takeover::postgres_expired_takeover_resumes_exact_bundle_and_keeps_progress_and_repost_budget",
    "posted_generation_takeover::postgres_live_operation_lease_is_busy_and_stale_inputs_do_not_mutate",
    "posted_generation_takeover::postgres_concurrent_takeover_by_one_new_attempt_is_idempotent",
    "posted_generation_takeover::postgres_lost_ack_replay_and_recover_do_not_rotate_the_claim_again",
    "posted_generation_takeover::postgres_attempt_expiry_during_registry_lock_wait_refuses_takeover",
    "posted_generation_takeover::postgres_expired_new_attempt_and_job_status_or_owner_fence_refuse_takeover",
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
