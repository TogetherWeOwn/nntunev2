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
# Phase 1 runs 287 serial PostgreSQL tests, so 180 seconds cannot cover it:
# the bank was observed still progressing when the phase timeout killed it.
PHASE_TIMEOUT_SECONDS = 900
# Retirement is irreversible on this disposable service. Run it separately,
# after every test that needs to seed an old-writer compatibility fixture.
RETIREMENT_CASE = (
    "zz_posted_generation_retirement::"
    "zz_postgres_retirement_lifecycle_serializes_inserts_health_snapshots_and_replay"
)
EXPECTED = {
    RETIREMENT_CASE,
    "posted_generation_retained::repair::postgres_retained_repair_start_is_idempotent_refuses_without_origin_and_never_resets_progress",
    "posted_generation_retained::repair::postgres_retained_repair_all_missing_answered_reposts_hit_cap_then_five_passes_conclude_failure",
    "posted_generation_retained::repair::postgres_retained_repair_transport_and_refused_posts_never_spend_or_conclude_failure",
    "posted_generation_retained::repair::postgres_retained_repair_presence_complete_is_not_publication_or_failure_authority",
    "posted_generation_retained::repair::postgres_retained_repair_bounded_cursor_restarts_and_read_only_recovery_retires_old_callbacks",
    "posted_generation_retained::repair::postgres_retained_repair_selection_aba_and_reference_recreation_block_everything",
    "posted_generation_retained::repair::postgres_retained_repair_health_mutation_during_stat_or_post_stales_callback_and_keeps_state",
    "posted_generation_retained::repair::postgres_retained_repair_expiry_behind_final_health_update_rolls_back_evidence",
    "posted_generation_retained::repair::postgres_retained_repair_cancelled_callbacks_record_no_answer_and_keep_the_fence",
    "posted_generation_retained::failure::postgres_retained_failure_snapshots_repair_evidence_and_preserves_custody_on_exact_replay",
    "posted_generation_retained::failure::postgres_retained_failure_unknown_partial_transport_and_pre_repair_evidence_never_conclude",
    "posted_generation_retained::failure::postgres_retained_failure_intact_content_vetoes_absence_and_mismatched_content_refuses",
    "posted_generation_retained::failure::postgres_retained_failure_requires_current_selected_origin_and_rejects_selection_aba",
    "posted_generation_retained::failure::postgres_retained_failure_reference_recreation_and_changed_health_retire_original_permits",
    "posted_generation_retained::failure::postgres_retained_failure_receipt_error_rolls_back_health_and_same_permit_can_retry",
    "posted_generation_retained::failure::postgres_retained_failure_concurrent_decisions_commit_once_and_lease_expiry_after_wait_refuses",
    "posted_generation_retained::failure::postgres_retained_failure_decision_is_immutable_and_forward_migration_replay_preserves_receipt",
    "posted_generation_retained::provenance::postgres_retained_unknown_provenance_binds_original_attempt_reference_round_and_selection",
    "posted_generation_retained::provenance::postgres_retained_content_provenance_keeps_each_nonaffirmative_verdict_and_side",
    "posted_generation_retained::provenance::postgres_retained_provenance_distinguishes_selection_aba_from_historical_reference_health",
    "posted_generation_retained::provenance::postgres_retained_stale_callbacks_create_no_provenance",
    "posted_generation_retained::provenance::postgres_retained_provenance_and_content_roll_back_with_health_failure",
    "posted_generation_retained::provenance::postgres_retained_provenance_is_immutable_and_migration_replay_preserves_it",
    "posted_generation_registry_admission::postgres_admission_missing_legacy_registration_refuses_without_adopting_custody",
    "posted_generation_registry_admission::postgres_admission_selected_health_never_implicitly_reuses_or_replaces",
    "posted_generation_registry_admission::postgres_admission_registered_history_uses_registry_without_scalar_physical_lookup",
    "posted_generation_registry_admission::postgres_admission_unregistered_history_is_inconsistent_not_an_arbitrary_winner",
    "posted_generation_registry_admission::postgres_admission_pending_history_replays_only_the_exact_stored_candidate",
    "posted_generation_registry_admission::postgres_admission_history_does_not_repair_a_stale_registry_observation",
    "posted_generation_media_v1::postgres_media_v1_candidate_recovery_replay_and_retained_custody_keep_exact_metadata",
    "posted_generation_media_v1::postgres_media_v1_keyring_open_authenticates_the_recovered_candidate_ciphertext",
    "posted_generation_dispatch::tagged::postgres_tagged_producer_commits_byte_derived_storage_identity_and_replays",
    "posted_generation_dispatch::tagged::postgres_tagged_producer_disabled_or_unadmitted_never_reaches_storage",
    "posted_generation_dispatch::tagged::postgres_tagged_producer_revalidates_role_stop_original_owner_and_admission_after_storage",
    "posted_generation_dispatch::tagged::postgres_tagged_producer_ambiguous_put_and_divergent_retry_keep_one_identity",
    "posted_generation_dispatch::tagged::postgres_tagged_producer_readback_failure_retains_object_without_dispatch_or_attempt",
    "posted_generation_dispatch::tagged::postgres_tagged_producer_refuses_plan_alias_and_invalid_file_before_storage",
    "posted_generation_dispatch::postgres_dispatch_default_disabled_has_no_database_or_io_effect",
    "posted_generation_dispatch::postgres_dispatch_admin_admission_prepares_and_pipeline_or_drifted_jobs_refuse",
    "posted_generation_dispatch::postgres_dispatch_prepare_reuses_pointer_across_restart_and_changed_inputs_refuse",
    "posted_generation_dispatch::postgres_dispatch_pointer_wire_form_rejects_unknown_forged_and_missing_fields",
    "posted_generation_dispatch::postgres_dispatch_recovery_before_start_never_creates_receipt_or_attempt",
    "posted_generation_dispatch::postgres_dispatch_send_starts_atomically_and_queue_recovery_after_drop_returns_same_handle",
    "posted_generation_dispatch::postgres_dispatch_ambiguous_queue_ack_retains_pending_pointer_without_token_or_budget_reset",
    "posted_generation_dispatch::postgres_dispatch_wrong_immutable_bytes_or_versions_fail_before_send",
    "posted_generation_dispatch::postgres_dispatch_current_role_deletion_job_and_original_drift_refuse_without_writes",
    "posted_generation_dispatch::postgres_dispatch_expired_and_superseded_requests_recover_and_send_refuse_without_startup",
    "posted_generation_dispatch::postgres_dispatch_concurrent_same_and_distinct_preparation_and_startup_callers",
    "posted_generation_dispatch::postgres_dispatch_trigger_injection_rolls_back_and_immutability_survives_migration_replay",
    "posted_generation_dispatch::postgres_dispatch_rechecks_role_job_deletion_and_lease_after_unlocked_object_io",
    "posted_generation_attempt_acquisition::startup::coordinator::postgres_coordinator_start_acquires_atomically_and_restart_replays_without_renewal",
    "posted_generation_attempt_acquisition::startup::coordinator::postgres_coordinator_start_completes_existing_preparation_after_client_drop",
    "posted_generation_attempt_acquisition::startup::coordinator::postgres_coordinator_start_acquisition_error_rolls_back_job_receipt_and_attempt",
    "posted_generation_attempt_acquisition::startup::coordinator::postgres_coordinator_start_foreign_missing_stopped_deleting_and_reset_jobs_refuse",
    "posted_generation_attempt_acquisition::startup::coordinator::postgres_coordinator_start_concurrent_requests_commit_one_exact_attempt",
    "posted_generation_attempt_acquisition::startup::coordinator::postgres_coordinator_start_expired_and_superseded_receipts_require_explicit_next_request",
    "posted_generation_attempt_acquisition::startup::coordinator::postgres_coordinator_start_replay_rechecks_attempt_lease_after_observed_lock_wait",
    "posted_generation_attempt_acquisition::startup::coordinator::postgres_coordinator_start_rechecks_owner_and_job_after_observed_lock_waits",
    "posted_generation_driver::delivery::durable::postgres_durable_delivery_restart_recovers_exact_attempt_and_cursor_without_handles",
    "posted_generation_driver::delivery::durable::postgres_durable_delivery_missing_or_unacquired_receipts_never_create_authority",
    "posted_generation_driver::delivery::durable::postgres_durable_delivery_rejects_mismatched_admission_owner_request_and_attempt",
    "posted_generation_driver::delivery::durable::postgres_durable_delivery_expired_or_superseded_request_never_acquires_or_takes_over",
    "posted_generation_driver::delivery::durable::postgres_durable_delivery_waiting_unbegun_stopped_and_deleting_work_retains_state",
    "posted_generation_driver::delivery::durable::postgres_durable_delivery_network_callbacks_keep_original_fences_and_release_locks",
    "posted_generation_driver::delivery::durable::postgres_durable_recovery_rechecks_after_owner_job_and_attempt_lock_waits",
    "posted_generation_driver::delivery::durable::postgres_durable_delivery_malformed_coordinates_and_bounds_refuse_before_work",
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
    "posted_generation_attempt_acquisition::startup::owner_fence::postgres_owner_deletion_fences_all_attempt_entries_after_startup_without_mutation",
    "posted_generation_attempt_acquisition::startup::owner_fence::postgres_owner_deletion_fences_existing_candidate_handles_and_original_callbacks",
    "posted_generation_attempt_acquisition::startup::owner_fence::postgres_owner_deletion_attempt_entries_recheck_after_owner_lock_wait",
    "posted_generation_attempt_acquisition::startup::owner_fence::postgres_owner_deletion_candidate_fence_rechecks_after_owner_lock_wait",
    "posted_generation_attempt_acquisition::startup::owner_fence::postgres_owner_fence_is_held_before_job_wait_and_through_attempt_commit",
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


ADMISSION_PREFIX = "worker_entry::admin_dispatch_postgres::"
ADMISSION_EXPECTED = {
    ADMISSION_PREFIX + name for name in (
        "postgres_real_admission_dispatch_replays_original_after_client_drop",
        "postgres_real_admission_rejection_cannot_prepare_or_start",
        "postgres_real_admission_demoted_and_deleting_actors_leave_no_dispatchable_job",
        "postgres_real_admission_duplicate_actor_never_rebinds_original_dispatch_owner",
        "postgres_real_admission_audit_error_rolls_back_before_dispatch_and_retry_keeps_identity",
        "postgres_real_admission_concurrent_replay_and_duplicate_send_keep_one_attempt",
        "sealed::postgres_sealed_plan_restart_replays_exact_envelope_and_prior_key",
        "sealed::postgres_sealed_plan_disabled_invalid_and_unadmitted_write_nothing",
        "sealed::postgres_sealed_plan_divergent_replay_provenance_and_version_refuse",
        "sealed::postgres_sealed_plan_current_admission_fences_both_entries",
        "sealed::postgres_sealed_plan_concurrent_writers_converge_without_resealing",
        "sealed::postgres_sealed_plan_immutability_and_migration_replay_preserve_custody",
        "sealed::postgres_sealed_plan_unknown_wrong_key_and_cross_row_envelopes_fail_closed",
        "sealed::preparation::postgres_private_preparation_composes_actual_sealed_plan_startup_and_readonly_recovery",
        "sealed::preparation::postgres_private_preparation_exact_replay_keeps_envelope_and_divergence_refuses",
        "sealed::preparation::postgres_private_preparation_malformed_and_foreign_provenance_write_nothing",
        "sealed::preparation::postgres_private_preparation_open_requires_exact_dispatch_plan_version_and_digest",
        "sealed::preparation::postgres_private_preparation_rechecks_role_after_unlocked_private_open",
    )
}


def main():
    if (
        os.environ.get("GITHUB_ACTIONS") != "true"
        or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
    ):
        sys.exit("The full posted-generation bank runs only on GitHub-hosted Actions")
    if not os.environ.get("NNTUNE_TEST_DATABASE_URL"):
        sys.exit("NNTUNE_TEST_DATABASE_URL is required; the bank cannot skip")
    common = [
        "cargo", "test", "--locked", "--manifest-path", "workers/api/Cargo.toml",
        "--features", "postgres-tests",
    ]
    command = common + ["--test", "posted_generations_postgres"]
    admission = common + ["--lib", ADMISSION_PREFIX]
    for bank, expected in [(command, EXPECTED), (admission, ADMISSION_EXPECTED)]:
        listing = subprocess.run(
            bank + ["--", "--list"], cwd=ROOT, text=True,
            stdout=subprocess.PIPE, check=True,
        ).stdout
        actual = {
            line.removesuffix(": test") for line in listing.splitlines()
            if line.endswith(": test")
        }
        if actual != expected:
            sys.exit("Posted-generation test inventory changed or did not compile completely")
    phases = [
        (command, ["--skip", RETIREMENT_CASE], EXPECTED - {RETIREMENT_CASE}),
        (admission, [], ADMISSION_EXPECTED),
        (command, ["--exact", RETIREMENT_CASE], {RETIREMENT_CASE}),
    ]
    for bank, filters, required in phases:
        try:
            result = subprocess.run(
                bank + ["--", "--test-threads=1", *filters], cwd=ROOT, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False,
                timeout=PHASE_TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired as expired:
            partial = expired.stdout or ""
            if isinstance(partial, bytes):
                partial = partial.decode(errors="replace")
            partial = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", partial)
            print(partial, flush=True)
            completed = len(re.findall(r"(?m)^test .* \.\.\. ok$", partial))
            sys.exit(
                f"Posted-generation bank phase timed out after "
                f"{PHASE_TIMEOUT_SECONDS}s with {completed} completed cases; "
                f"the phase needs more time or is hung mid-case"
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
    total = len(EXPECTED) + len(ADMISSION_EXPECTED)
    print(f"Posted-generation preparation: {total} required cases passed", flush=True)


if __name__ == "__main__":
    main()
