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
    "posted_generation_checkpoint::postgres_repost_counts_first_second_and_cap_per_slot_and_side_without_confirming",
    "posted_generation_checkpoint::postgres_confirmed_slot_skips_repost_while_the_other_lane_stays_ready",
    "posted_generation_checkpoint::postgres_transport_and_refused_outcomes_leave_counts_health_and_permit_untouched",
    "posted_generation_checkpoint::postgres_duplicate_and_concurrent_callbacks_with_one_permit_count_once",
    "posted_generation_checkpoint::postgres_stale_round_revision_lease_job_and_attempt_fences_reject_repost_calls",
    "posted_generation_checkpoint::postgres_lost_ack_recovery_and_takeover_keep_counts_and_retire_old_permits",
    "posted_generation_checkpoint::postgres_invalid_slots_and_unbegun_round_are_refused_and_snapshot_is_bound_and_redacted",
    "posted_generation_checkpoint::postgres_finished_verification_refuses_prepare_and_record_without_counting",
    "posted_generation_driver::postgres_driver_invalid_bounds_and_waiting_schedule_make_no_network_or_mutation",
    "posted_generation_driver::postgres_driver_max_slots_bounds_cursor_and_resumes_remaining_slots_to_presence_only_finish",
    "posted_generation_driver::postgres_driver_missing_reposts_same_slot_side_and_posted_or_duplicate_waits_for_later_stat",
    "posted_generation_driver::postgres_driver_transport_answers_never_post_or_spend_budget",
    "posted_generation_driver::postgres_driver_passes_reset_cursor_and_backoff_exhausts_to_keep_original_without_negative_effects",
    "posted_generation_driver::postgres_driver_resume_after_stat_callback_crash_skips_confirmed_slot_without_remint",
    "posted_generation_driver::postgres_driver_concurrent_mutation_during_network_makes_callback_stale_without_lock_held",
    "posted_generation_driver::postgres_migration_0040_replay_preserves_cursor_and_budget_and_corrupt_cursor_fails_closed",
    "posted_generation_driver::postgres_driver_end_cursor_recovers_completion_without_network_or_false_confirmation",
    "posted_generation_driver::postgres_driver_finished_presence_replays_with_zero_network",
    "posted_generation_presence::postgres_presence_permits_confirm_every_slot_and_side_without_content_proof",
    "posted_generation_presence::postgres_missing_and_unavailable_unconfirm_one_slot_and_invalidate_intact_content_only",
    "posted_generation_presence::postgres_missing_during_the_round_blocks_finish_until_the_slot_is_reconfirmed",
    "posted_generation_presence::postgres_original_permits_are_stale_after_revision_round_takeover_job_and_lease_changes",
    "posted_generation_presence::postgres_same_permit_race_applies_once_and_lost_ack_recovery_never_double_spends",
    "posted_generation_presence::postgres_presence_prepare_refuses_invalid_slots_and_unbegun_rounds_without_writes",
    "posted_generation_presence::postgres_fresh_identical_presence_callbacks_are_idempotent_without_a_revision_bump",
    "posted_generation_rechecks::postgres_begun_round_persists_waiting_schedule_and_early_prepare_mutates_nothing",
    "posted_generation_rechecks::postgres_recheck_passes_back_off_to_the_cap_without_negative_health_or_repost_counts",
    "posted_generation_rechecks::postgres_lost_ack_and_duplicate_or_parallel_completion_count_exactly_once",
    "posted_generation_rechecks::postgres_confirmations_refresh_the_handle_within_a_pass_and_old_revisions_are_rejected",
    "posted_generation_rechecks::postgres_takeover_preserves_schedule_and_count_and_invalidates_the_old_permit",
    "posted_generation_rechecks::postgres_round_verification_lease_and_job_fences_reject_recheck_calls",
    "posted_generation_rechecks::postgres_schema_replay_preserves_schedule_and_backfills_missing_rows_as_untracked",
    "posted_generation_content::driver::postgres_content_driver_binds_stored_bundle_and_side_without_granting_authority",
    "posted_generation_content::driver::postgres_content_driver_explicit_rechecks_invalidate_prior_intact_on_every_failure",
    "posted_generation_content::driver::postgres_content_driver_rejects_partial_and_mismatched_reports_without_writes",
    "posted_generation_content::driver::postgres_content_driver_unbegun_and_stale_handles_do_not_start_a_read",
    "posted_generation_content::driver::postgres_content_driver_concurrent_fence_changes_reject_original_read_callbacks_without_locks",
    "posted_generation_content::driver::postgres_content_driver_cancelled_read_writes_no_observation_and_recovery_reuses_custody",
    "posted_generation_content::driver::postgres_content_driver_lost_ack_recovery_rechecks_idempotently_without_reminting",
    "posted_generation_content::postgres_presence_finish_alone_proves_no_content_and_two_intact_reports_store_only_evidence",
    "posted_generation_content::postgres_mismatched_reports_are_rejected_without_mutation_or_burning_the_permit",
    "posted_generation_content::postgres_same_permit_race_applies_once_and_lost_ack_recovery_is_idempotent_with_fresh_permits",
    "posted_generation_content::postgres_nonaffirmative_results_replace_intact_without_health_budget_or_cleanup_writes",
    "posted_generation_content::postgres_new_round_rejects_old_permits_and_keeps_old_content_out_of_the_current_checkpoint",
    "posted_generation_content::postgres_takeover_preserves_content_evidence_and_invalidates_the_old_permit",
    "posted_generation_content::postgres_lease_status_attempt_and_revision_fences_gate_content_preparation_and_callbacks",
    "posted_generation_content::postgres_schema_replay_preserves_content_evidence_without_backfill_and_checks_guard_rows",
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
