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
# Retirement is irreversible on this disposable service. Run it separately,
# after every test that needs to seed an old-writer compatibility fixture.
RETIREMENT_CASE = (
    "zz_posted_generation_retirement::"
    "zz_postgres_retirement_lifecycle_serializes_inserts_health_snapshots_and_replay"
)
EXPECTED = {
    RETIREMENT_CASE,
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
    "posted_generation_abandon::postgres_abandon_applies_atomically_preserves_custody_and_evidence_and_replays",
    "posted_generation_abandon::postgres_abandon_round_zero_candidate_and_replay_survives_registry_drift",
    "posted_generation_abandon::postgres_abandon_with_stale_health_revision_refuses_all_writes",
    "posted_generation_abandon::postgres_abandon_with_old_round_refuses_and_changes_nothing",
    "posted_generation_abandon::postgres_abandon_after_newer_attempt_or_takeover_rejects_old_claim_and_current_claim_works",
    "posted_generation_abandon::postgres_abandon_attempt_expiry_during_registry_lock_wait_refuses",
    "posted_generation_abandon::postgres_abandon_concurrent_original_handle_applies_once_and_replays_once",
    "posted_generation_abandon::postgres_abandoned_candidate_loses_authority_and_unique_hash_blocks_replacement",
    "posted_generation_attempt_acquisition::startup::postgres_startup_atomically_starts_admitted_job_without_granting_attempt_authority",
    "posted_generation_attempt_acquisition::startup::postgres_startup_invalid_missing_foreign_admission_and_terminal_bindings_write_nothing",
    "posted_generation_attempt_acquisition::startup::postgres_startup_deleting_owner_refuses_new_start_and_existing_receipt_replay",
    "posted_generation_attempt_acquisition::startup::postgres_startup_restart_reloads_exact_receipt_and_pending_bundle_without_renewal",
    "posted_generation_attempt_acquisition::startup::postgres_startup_reset_to_queued_never_revives_receipt_or_attempt_history",
    "posted_generation_attempt_acquisition::startup::postgres_startup_running_replay_and_explicit_predecessor_rules_never_borrow_authority",
    "posted_generation_attempt_acquisition::startup::postgres_startup_concurrent_same_and_distinct_requests_preserve_exact_acquisition",
    "posted_generation_attempt_acquisition::startup::postgres_startup_receipt_insert_error_rolls_back_running_transition",
    "posted_generation_attempt_acquisition::startup::postgres_startup_rechecks_owner_and_job_after_independent_session_lock_waits",
    "posted_generation_attempt_acquisition::durable::postgres_durable_prepare_invalid_missing_foreign_and_stopped_requests_write_nothing",
    "posted_generation_attempt_acquisition::durable::postgres_durable_prepare_restart_before_acquisition_reuses_exact_persisted_proposal",
    "posted_generation_attempt_acquisition::durable::postgres_durable_prepare_restart_after_lost_acquisition_response_recovers_pending_bundle",
    "posted_generation_attempt_acquisition::durable::postgres_durable_prepare_replay_mismatches_and_busy_new_keys_never_remint_or_borrow",
    "posted_generation_attempt_acquisition::durable::postgres_durable_prepare_expired_and_superseded_receipts_never_renew_or_advance",
    "posted_generation_attempt_acquisition::durable::postgres_durable_prepare_concurrent_same_request_persists_one_proposal_and_acquires_once",
    "posted_generation_attempt_acquisition::durable::postgres_durable_prepare_distinct_requests_never_recover_the_winners_token",
    "posted_generation_attempt_acquisition::durable::postgres_durable_receipt_immutability_constraints_and_migration_replay_preserve_identity",
    "posted_generation_attempt_acquisition::postgres_acquisition_prepare_rejects_invalid_input_writes_nothing_and_is_redacted",
    "posted_generation_attempt_acquisition::postgres_acquisition_lost_response_simulation_recovers_same_attempt_and_pending_custody",
    "posted_generation_attempt_acquisition::postgres_acquisition_duplicate_acquire_and_recover_do_not_renew_or_write",
    "posted_generation_attempt_acquisition::postgres_acquisition_wrong_request_identity_drift_status_and_absence_refuse",
    "posted_generation_attempt_acquisition::postgres_acquisition_expired_receipt_and_superseded_attempts_refuse_without_writes",
    "posted_generation_attempt_acquisition::postgres_acquisition_concurrent_same_request_acquires_once_with_identical_handles",
    "posted_generation_attempt_acquisition::postgres_acquisition_concurrent_distinct_requests_have_one_winner_and_no_shared_token",
    "posted_generation_attempt_acquisition::postgres_acquisition_recovery_rechecks_job_and_attempt_lock_waits_on_the_database_clock",
    "posted_generation_checkpoint::postgres_repost_counts_first_second_and_cap_per_slot_and_side_without_confirming",
    "posted_generation_checkpoint::postgres_confirmed_slot_skips_repost_while_the_other_lane_stays_ready",
    "posted_generation_checkpoint::postgres_transport_and_refused_outcomes_leave_counts_health_and_permit_untouched",
    "posted_generation_checkpoint::postgres_duplicate_and_concurrent_callbacks_with_one_permit_count_once",
    "posted_generation_checkpoint::postgres_stale_round_revision_lease_job_and_attempt_fences_reject_repost_calls",
    "posted_generation_checkpoint::postgres_lost_ack_recovery_and_takeover_keep_counts_and_retire_old_permits",
    "posted_generation_checkpoint::postgres_invalid_slots_and_unbegun_round_are_refused_and_snapshot_is_bound_and_redacted",
    "posted_generation_checkpoint::postgres_finished_verification_refuses_prepare_and_record_without_counting",
    "posted_generation_driver::delivery::postgres_pending_delivery_discovers_exact_custody_and_resumes_without_local_progress",
    "posted_generation_driver::delivery::postgres_pending_delivery_invalid_missing_foreign_and_stopped_work_never_reaches_transport",
    "posted_generation_driver::delivery::postgres_pending_delivery_unbegun_waiting_and_expired_work_does_not_renew_or_start_rounds",
    "posted_generation_driver::delivery::postgres_pending_delivery_requires_explicit_takeover_and_keeps_the_durable_cursor",
    "posted_generation_driver::delivery::postgres_pending_delivery_keeps_original_callback_fences_without_network_locks",
    "posted_generation_recovery::postgres_pending_discovery_rejects_registry_changes_between_discovery_and_recovery",
    "posted_generation_recovery::postgres_pending_discovery_rechecks_both_leases_after_registry_wait",
    "posted_generation_recovery::postgres_pending_discovery_rechecks_both_leases_after_health_wait",
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
    "posted_generation_failure::postgres_failure_definitive_applies_atomically_with_receipt_and_preserves_everything",
    "posted_generation_failure::postgres_failure_missing_below_cap_or_before_the_last_repost_is_insufficient_until_fresh_stat",
    "posted_generation_failure::postgres_failure_transport_positive_and_conflicting_intact_content_never_conclude",
    "posted_generation_failure::postgres_failure_round_zero_waiting_and_untracked_schedules_are_not_terminal",
    "posted_generation_failure::postgres_failure_stale_revision_round_lease_job_and_attempt_fences_reject_without_writes",
    "posted_generation_failure::postgres_failure_after_newer_attempt_or_takeover_rejects_old_claim_and_current_claim_works",
    "posted_generation_failure::postgres_failure_and_abandonment_receipts_cannot_impersonate_each_other",
    "posted_generation_failure::postgres_failure_concurrent_original_handle_applies_once_and_the_other_does_not_write",
    "posted_generation_failure::postgres_driver_missing_after_three_reposts_through_five_passes_returns_verification_failed",
    "posted_generation_failure::postgres_driver_transport_only_through_five_passes_keeps_original_without_terminal_state",
    "posted_generation_driver::postgres_due_driver_renews_live_leases_but_waiting_delivery_does_not",
    "posted_generation_lease::postgres_live_renewal_extends_both_leases_and_preserves_original_fences_and_progress",
    "posted_generation_lease::postgres_renewal_never_shortens_either_live_deadline",
    "posted_generation_lease::postgres_renewal_refuses_either_expired_lease_without_partial_extension",
    "posted_generation_lease::postgres_renewal_refuses_stopped_job_owner_drift_and_old_health_revision",
    "posted_generation_lease::postgres_superseded_attempt_and_claim_cannot_renew_after_takeover",
    "posted_generation_lease::postgres_concurrent_renewals_keep_one_claim_and_allow_original_handle_callbacks",
    "posted_generation_lease::postgres_renewal_rechecks_both_leases_after_registry_lock_wait",
    "posted_generation_legacy_fence::registration::postgres_registration_first_blocks_and_refuses_read_committed_legacy_callback",
    "posted_generation_legacy_fence::registration::postgres_registration_rollback_releases_legacy_callback_without_registering",
    "posted_generation_legacy_fence::registration::postgres_callback_first_registration_waits_and_never_promotes_old_timestamp",
    "posted_generation_legacy_fence::registration::postgres_callback_rollback_registration_waits_and_keeps_original_custody",
    "posted_generation_legacy_fence::registration::postgres_registration_invalidates_older_repeatable_and_serializable_snapshots",
    "posted_generation_legacy_fence::registration::postgres_competing_registrations_serialize_without_changing_the_bundle",
    "posted_generation_legacy_fence::registration::postgres_registration_replay_keeps_fences_after_historical_schema_replay",
    "posted_generation_legacy_fence::postgres_legacy_fence_refuses_each_mutable_health_field_by_id_and_hash",
    "posted_generation_legacy_fence::postgres_legacy_fence_refuses_the_actual_old_sql_helpers",
    "posted_generation_legacy_fence::postgres_legacy_fence_allows_registered_no_op_updates",
    "posted_generation_legacy_fence::postgres_legacy_fence_keeps_unregistered_compatibility_until_registered",
    "posted_generation_legacy_fence::postgres_legacy_fence_bulk_update_rolls_back_registered_and_unregistered",
    "posted_generation_legacy_fence::postgres_legacy_fence_survives_0042_replay_with_bundle_unchanged",
    "posted_generation_legacy_fence::postgres_legacy_fence_survives_exclusive_historical_0037_replay",
    "posted_generation_legacy_fence::postgres_legacy_fence_leaves_new_managed_candidate_on_generation_api",
    "posted_generation_presence::postgres_presence_permits_confirm_every_slot_and_side_without_content_proof",
    "posted_generation_presence::postgres_missing_and_unavailable_unconfirm_one_slot_and_invalidate_intact_content_only",
    "posted_generation_presence::postgres_missing_during_the_round_blocks_finish_until_the_slot_is_reconfirmed",
    "posted_generation_presence::postgres_original_permits_are_stale_after_revision_round_takeover_job_and_lease_changes",
    "posted_generation_presence::postgres_same_permit_race_applies_once_and_lost_ack_recovery_never_double_spends",
    "posted_generation_presence::postgres_presence_prepare_refuses_invalid_slots_and_unbegun_rounds_without_writes",
    "posted_generation_presence::postgres_fresh_identical_presence_callbacks_are_idempotent_without_a_revision_bump",
    "posted_generation_presence_provenance::postgres_changed_negative_provenance_retires_older_callbacks_but_identical_does_not",
    "posted_generation_presence_provenance::postgres_accounted_repost_makes_old_negative_unavailable_until_a_fresh_stat",
    "posted_generation_presence_provenance::postgres_direct_confirmation_discards_negative_provenance_and_never_forges_present",
    "posted_generation_presence_provenance::postgres_new_round_is_empty_and_takeover_keeps_typed_observation",
    "posted_generation_presence_provenance::postgres_presence_provenance_columns_reject_invalid_shapes",
    "posted_generation_presence_provenance::postgres_migration_0041_replay_keeps_legacy_null_rows_and_typed_observations",
    "posted_generation_rechecks::postgres_begun_round_persists_waiting_schedule_and_early_prepare_mutates_nothing",
    "posted_generation_rechecks::postgres_recheck_passes_back_off_to_the_cap_without_negative_health_or_repost_counts",
    "posted_generation_rechecks::postgres_lost_ack_and_duplicate_or_parallel_completion_count_exactly_once",
    "posted_generation_rechecks::postgres_confirmations_refresh_the_handle_within_a_pass_and_old_revisions_are_rejected",
    "posted_generation_rechecks::postgres_takeover_preserves_schedule_and_count_and_invalidates_the_old_permit",
    "posted_generation_rechecks::postgres_round_verification_lease_and_job_fences_reject_recheck_calls",
    "posted_generation_rechecks::postgres_schema_replay_preserves_schedule_and_backfills_missing_rows_as_untracked",
    "posted_generation_recovery::postgres_recovery_rechecks_both_leases_after_registry_wait",
    "posted_generation_recovery::postgres_admission_replay_rechecks_both_leases_after_registry_wait",
    "posted_generation_recovery::postgres_recovery_rechecks_both_leases_after_health_wait",
    "posted_generation_recovery::postgres_admission_replay_rechecks_both_leases_after_health_wait",
    "posted_generation_recovery::postgres_recovery_and_replay_reject_each_changed_health_fence_after_wait",
    "posted_generation_recovery::postgres_recovery_and_replay_resume_exact_bundle_after_unchanged_lock_waits",
    "posted_generation_repair_content::postgres_answered_reposts_invalidate_prior_intact_content_for_their_side_only",
    "posted_generation_repair_content::postgres_unanswered_reposts_and_absent_or_failed_content_are_never_rewritten",
    "posted_generation_repair_content::postgres_repost_invalidation_preserves_historical_rounds_and_the_other_side",
    "posted_generation_repair_content::postgres_stale_permits_and_duplicate_callbacks_cannot_resurrect_invalidated_content",
    "posted_generation_repair_content::postgres_lost_ack_recovery_and_takeover_preserve_the_invalidation",
    "posted_generation_repair_content::postgres_invalid_stored_content_identity_rejects_the_repost_and_rolls_back_accounting",
    "posted_generation_repair_content::postgres_concurrent_answered_reposts_invalidate_and_count_exactly_once",
    "posted_generation_content::driver::postgres_content_driver_binds_stored_bundle_and_side_without_granting_authority",
    "posted_generation_content::driver::postgres_content_driver_explicit_rechecks_invalidate_prior_intact_on_every_failure",
    "posted_generation_content::driver::postgres_content_driver_rejects_partial_and_mismatched_reports_without_writes",
    "posted_generation_content::driver::postgres_content_driver_unbegun_and_stale_handles_do_not_start_a_read",
    "posted_generation_content::driver::postgres_content_driver_concurrent_fence_changes_reject_original_read_callbacks_without_locks",
    "posted_generation_content::driver::postgres_content_driver_cancelled_read_writes_no_observation_and_recovery_reuses_custody",
    "posted_generation_content::driver::postgres_content_driver_lost_ack_recovery_rechecks_idempotently_without_reminting",
    "posted_generation_content::driver::postgres_content_driver_renews_both_leases_before_read_and_cancel_preserves_all_other_state",
    "posted_generation_content::driver::postgres_content_driver_expired_lease_refuses_read_without_partial_renewal",
    "posted_generation_content::driver::postgres_content_permit_preparation_alone_does_not_renew_delivery_leases",
    "posted_generation_content::driver::delivery_races::postgres_content_driver_expiry_between_preparation_and_renewal_refuses_without_read_or_writes",
    "posted_generation_content::driver::delivery_races::postgres_content_driver_health_fence_between_preparation_and_renewal_refuses_without_read_or_writes",
    "posted_generation_content::driver::delivery_races::postgres_content_driver_unchanged_fence_between_preparation_and_renewal_still_reads",
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
    "posted_generation_retained::postgres_retained_prepare_binds_exact_custody_is_redacted_and_dropped_permit_writes_nothing",
    "posted_generation_retained::postgres_retained_nonaffirmative_invalidation_only_withdraws_health_and_fences_old_observations",
    "posted_generation_retained::postgres_retained_pending_candidates_and_non_retainable_health_are_refused",
    "posted_generation_retained::postgres_retained_wrong_missing_and_removed_references_are_refused",
    "posted_generation_retained::postgres_retained_stale_health_revision_round_or_token_rejects_the_original_callback",
    "posted_generation_retained::postgres_retained_expired_superseded_attempts_and_stopped_jobs_reject_prepare_and_callback",
    "posted_generation_retained::postgres_retained_same_permit_on_two_connections_applies_exactly_once",
    "posted_generation_retained::postgres_retained_attempt_expiry_during_registry_or_health_lock_wait_rejects_the_callback",
    "posted_generation_retained::postgres_retained_legacy_row_without_plan_hash_exposes_custody_only_and_invalidates",
    "posted_generation_retained::postgres_retained_reference_reinsert_after_prepare_stales_negative_callback_and_keeps_state",
    "posted_generation_retained::postgres_retained_reference_reinsert_while_callback_waits_on_reference_row_refuses_it",
    "posted_generation_retained::postgres_retained_noop_reference_upsert_preserves_the_original_permit",
    "posted_generation_retained::postgres_retained_reference_token_survives_0044_and_historical_0037_replay",
    "posted_generation_retained::postgres_retained_reference_identity_and_token_are_immutable_and_tokens_are_distinct",
    "posted_generation_retained::content::postgres_retained_content_reference_reinsert_after_prepare_stales_intact_and_negative_reports",
    "posted_generation_retained::content::postgres_retained_content_reference_reinsert_while_callback_waits_on_reference_row_refuses_it",
    "posted_generation_retained::content::postgres_retained_content_noop_reference_upsert_preserves_the_original_permit",
    "posted_generation_retained::content::postgres_retained_content_prepare_binds_exact_custody_and_success_stores_evidence_on_both_sides",
    "posted_generation_retained::content::postgres_retained_content_reports_mismatching_any_dimension_are_rejected_without_writes",
    "posted_generation_retained::content::postgres_retained_content_failed_states_atomically_withdraw_health_and_keep_other_evidence",
    "posted_generation_retained::content::postgres_retained_content_stale_fences_reject_the_original_callback_and_prepare",
    "posted_generation_retained::content::postgres_retained_content_same_permit_on_two_connections_applies_exactly_once",
    "posted_generation_retained::content::postgres_retained_content_concurrent_mutation_during_async_read_stales_callback_without_locks",
    "posted_generation_retained::content::postgres_retained_content_cancelled_read_writes_no_observation",
    "posted_generation_retained::content::postgres_retained_content_prepare_refuses_round_zero_pending_and_unknown_references",
    "posted_generation_retained::content::postgres_retained_content_two_intact_sides_never_upgrade_unknown_and_bad_stored_identity_refuses",
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
    phases = [
        (["--skip", RETIREMENT_CASE], EXPECTED - {RETIREMENT_CASE}),
        (["--exact", RETIREMENT_CASE], {RETIREMENT_CASE}),
    ]
    for filters, required in phases:
        result = subprocess.run(
            command + ["--", "--test-threads=1", *filters], cwd=ROOT, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False, timeout=180,
        )
        output = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", result.stdout or "")
        print(output, flush=True)
        if result.returncode != 0:
            sys.exit("Posted-generation production-adapter bank failed")
        if f"test result: ok. {len(required)} passed; 0 failed; 0 ignored;" not in output:
            sys.exit("Posted-generation bank did not execute every required test")
        for name in required:
            if not re.search(r"(?m)^test " + re.escape(name) + r" \.\.\. ok$", output):
                sys.exit("A required posted-generation case did not execute and pass")
    print(f"Posted-generation preparation: {len(EXPECTED)} required cases passed", flush=True)


if __name__ == "__main__":
    main()
