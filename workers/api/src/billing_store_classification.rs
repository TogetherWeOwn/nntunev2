//! Classification primitives for the future coupled reconciliation transaction.
//!
//! These functions do not acquire locks, decode database rows, or perform writes.
//! Callers must supply a fresh global-key observation made after acquiring the
//! subscription table exclusion on the same transaction backend. The stale
//! predicate must be evaluated by PostgreSQL using the production input casts.
//! Used by the dormant owned Worker store; not a live reconciliation boundary.

use crate::billing_reconcile::ReconcileOutcome;

#[derive(Debug, PartialEq, Eq)]
pub struct Identity<'a> {
    pub user_id: &'a str,
    pub subscription_id: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ObservedTarget<'a> {
    pub identity: Identity<'a>,
    pub predicate_stale: bool,
    pub other_live_holder: bool,
    pub equal_end_provenance_changed: bool,
    pub matching_proof_cancel_conflict: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Absent,
    Owned {
        predicate_stale: bool,
        other_live_holder: bool,
        equal_end_provenance_changed: bool,
        matching_proof_cancel_conflict: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Accepted {
    StaleOwned,
    StaleAbsentEmpty,
    AppliedOwned,
    PredicateStaleOwned,
    LiveSlotSuppressedOwned,
}

/// A refusal is intentionally opaque to the caller. Database decoding errors
/// must also become store failures before invoking these classifiers.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused;

/// Only the exact subscription named by the fold can authorize history.
pub fn validate_history_target(outcome: &ReconcileOutcome) -> Result<(), Refused> {
    if outcome
        .history
        .iter()
        .any(|item| item.stripe_subscription_id != outcome.row.stripe_subscription_id)
    {
        return Err(Refused);
    }
    Ok(())
}

/// Classify a fresh, global-key observation before any subscription or history
/// write. A foreign, mismatched, or multiple-row result is never absence.
pub fn classify_target(
    requested: &Identity<'_>,
    rows: &[ObservedTarget<'_>],
) -> Result<Target, Refused> {
    match rows {
        [] => Ok(Target::Absent),
        [row] if row.identity == *requested => Ok(Target::Owned {
            predicate_stale: row.predicate_stale,
            other_live_holder: row.other_live_holder,
            equal_end_provenance_changed: row.equal_end_provenance_changed,
            matching_proof_cancel_conflict: row.matching_proof_cancel_conflict,
        }),
        _ => Err(Refused),
    }
}

/// Explicit-stale operations must skip subscription DML entirely.
pub fn classify_explicit_stale(target: &Target, history_empty: bool) -> Result<Accepted, Refused> {
    match (target, history_empty) {
        (Target::Owned { .. }, _) => Ok(Accepted::StaleOwned),
        (Target::Absent, true) => Ok(Accepted::StaleAbsentEmpty),
        (Target::Absent, false) => Err(Refused),
    }
}

/// A non-stale absence fold cannot write an owned row that appeared before
/// the lock, even when the paid-end predicate would suppress the update.
pub fn validate_read_provenance(
    outcome: &ReconcileOutcome,
    owned_target_exists: bool,
) -> Result<(), Refused> {
    if owned_target_exists && !outcome.explicit_stale && outcome.read_plan.is_none() {
        return Err(Refused);
    }
    Ok(())
}

/// When equal-end proof changed, the caller must refold before releasing an
/// owned slot or accepting a conflicting cancellation flag on the current proof.
pub fn validate_equal_end_provenance(
    outcome: &ReconcileOutcome,
    target: &Target,
) -> Result<(), Refused> {
    match target {
        Target::Owned {
            equal_end_provenance_changed: true,
            matching_proof_cancel_conflict,
            ..
        } if !outcome.explicit_stale
            && (*matching_proof_cancel_conflict
                || crate::billing_reconcile::is_terminal_status(&outcome.row.status)) =>
        {
            Err(Refused)
        }
        _ => Ok(()),
    }
}

/// Only a slot-reserving fold with exact-target read provenance can commit
/// history for a write suppressed by a different subscription's live slot.
pub fn has_slot_suppression_proof(outcome: &ReconcileOutcome) -> bool {
    // Match uq_subscriptions_live_slot and the conflict UPDATE guard.
    outcome.read_plan.is_some()
        && matches!(
            outcome.row.status.as_str(),
            "incomplete" | "active" | "past_due" | "unpaid" | "paused"
        )
}

/// Classify checked RETURNING rows for a non-explicit write, before history.
/// SQL and constraint errors must not be converted into an empty result.
pub fn classify_write(
    requested: &Identity<'_>,
    target: &Target,
    returned: &[Identity<'_>],
    slot_suppression_proof: bool,
) -> Result<Accepted, Refused> {
    match returned {
        [identity] if identity == requested => Ok(Accepted::AppliedOwned),
        [] => match target {
            Target::Owned {
                predicate_stale: true,
                ..
            } => Ok(Accepted::PredicateStaleOwned),
            Target::Owned {
                other_live_holder: true,
                ..
            } if slot_suppression_proof => Ok(Accepted::LiveSlotSuppressedOwned),
            _ => Err(Refused),
        },
        _ => Err(Refused),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity<'a>(user_id: &'a str, subscription_id: &'a str) -> Identity<'a> {
        Identity {
            user_id,
            subscription_id,
        }
    }

    #[test]
    fn global_observation_rejects_foreign_mismatched_and_multiple_rows() {
        let requested = identity("user-a", "sub-a");
        assert_eq!(classify_target(&requested, &[]), Ok(Target::Absent));
        for row in [
            ObservedTarget {
                identity: identity("user-b", "sub-a"),
                predicate_stale: true,
                other_live_holder: false,
                equal_end_provenance_changed: false,
                matching_proof_cancel_conflict: false,
            },
            ObservedTarget {
                identity: identity("user-a", "sub-b"),
                predicate_stale: false,
                other_live_holder: false,
                equal_end_provenance_changed: false,
                matching_proof_cancel_conflict: false,
            },
        ] {
            assert_eq!(classify_target(&requested, &[row]), Err(Refused));
        }
        let rows = [
            ObservedTarget {
                identity: identity("user-a", "sub-a"),
                predicate_stale: false,
                other_live_holder: false,
                equal_end_provenance_changed: false,
                matching_proof_cancel_conflict: false,
            },
            ObservedTarget {
                identity: identity("user-a", "sub-a"),
                predicate_stale: false,
                other_live_holder: false,
                equal_end_provenance_changed: false,
                matching_proof_cancel_conflict: false,
            },
        ];
        assert_eq!(classify_target(&requested, &rows), Err(Refused));
    }

    #[test]
    fn explicit_stale_requires_owned_target_or_empty_absence() {
        let requested = identity("user-a", "sub-a");
        let own = [ObservedTarget {
            identity: identity("user-a", "sub-a"),
            predicate_stale: false,
            other_live_holder: false,
            equal_end_provenance_changed: true,
            matching_proof_cancel_conflict: false,
        }];
        let target = classify_target(&requested, &own).unwrap();
        assert_eq!(
            classify_explicit_stale(&target, false),
            Ok(Accepted::StaleOwned)
        );
        assert_eq!(
            classify_explicit_stale(&target, true),
            Ok(Accepted::StaleOwned)
        );
        assert_eq!(
            classify_explicit_stale(&Target::Absent, true),
            Ok(Accepted::StaleAbsentEmpty)
        );
        assert_eq!(
            classify_explicit_stale(&Target::Absent, false),
            Err(Refused)
        );
    }

    #[test]
    fn write_result_requires_exact_identity_or_proved_owned_suppression() {
        let requested = identity("user-a", "sub-a");
        let own_stale = Target::Owned {
            predicate_stale: true,
            other_live_holder: false,
            equal_end_provenance_changed: false,
            matching_proof_cancel_conflict: false,
        };
        let own_fresh = Target::Owned {
            predicate_stale: false,
            other_live_holder: false,
            equal_end_provenance_changed: false,
            matching_proof_cancel_conflict: false,
        };
        assert_eq!(
            classify_write(
                &requested,
                &Target::Absent,
                &[identity("user-a", "sub-a")],
                false
            ),
            Ok(Accepted::AppliedOwned)
        );
        assert_eq!(
            classify_write(&requested, &own_stale, &[], false),
            Ok(Accepted::PredicateStaleOwned)
        );
        assert_eq!(
            classify_write(&requested, &own_fresh, &[], true),
            Err(Refused)
        );
        assert_eq!(
            classify_write(&requested, &Target::Absent, &[], true),
            Err(Refused)
        );
        let occupied = Target::Owned {
            predicate_stale: false,
            other_live_holder: true,
            equal_end_provenance_changed: true,
            matching_proof_cancel_conflict: false,
        };
        assert_eq!(
            classify_write(&requested, &occupied, &[], true),
            Ok(Accepted::LiveSlotSuppressedOwned)
        );
        assert_eq!(
            classify_write(&requested, &occupied, &[], false),
            Err(Refused)
        );
        for wrong in [identity("user-b", "sub-a"), identity("user-a", "sub-b")] {
            assert_eq!(
                classify_write(&requested, &own_stale, &[wrong], true),
                Err(Refused)
            );
        }
        assert_eq!(
            classify_write(
                &requested,
                &own_stale,
                &[identity("user-a", "sub-a"), identity("user-a", "sub-a")],
                true
            ),
            Err(Refused)
        );
    }

    fn outcome_with(
        subscription_id: &str,
        status: &str,
        explicit_stale: bool,
        read_plan: Option<&str>,
        read_anchor: Option<&str>,
    ) -> ReconcileOutcome {
        ReconcileOutcome {
            row: crate::billing_reconcile::ReconciledRow {
                stripe_subscription_id: subscription_id.into(),
                stripe_customer_id: "cus-a".into(),
                stripe_item_id: "si-a".into(),
                stripe_price_id: "price-plus".into(),
                plan_id: "plus".into(),
                status: status.into(),
                current_period_end_unix: Some(200),
                cancel_at_period_end: false,
                paid_through_unix: Some(200),
                paid_invoice_id: Some("in-a".into()),
                grace_until_unix: None,
            },
            history: Vec::new(),
            fail_closed: None,
            explicit_stale,
            read_plan: read_plan.map(str::to_string),
            read_anchor: read_anchor.map(str::to_string),
        }
    }

    fn history_row(
        invoice_id: &str,
        subscription_id: &str,
    ) -> crate::billing_reconcile::PaidInvoiceRow {
        crate::billing_reconcile::PaidInvoiceRow {
            stripe_invoice_id: invoice_id.into(),
            stripe_subscription_id: subscription_id.into(),
            plan_id: "plus".into(),
            period_start_unix: 100,
            period_end_unix: 200,
            amount_cents: 800,
            currency: "usd".into(),
            paid_at_unix: 100,
            covered_periods: vec![(100, 200)],
        }
    }

    #[test]
    fn validate_history_target_rejects_foreign_subscription_invoices() {
        let mut outcome = outcome_with("sub-a", "active", false, Some("plus"), Some("in-a"));
        assert_eq!(validate_history_target(&outcome), Ok(()));
        outcome.history = vec![history_row("in-1", "sub-a"), history_row("in-2", "sub-a")];
        assert_eq!(validate_history_target(&outcome), Ok(()));
        outcome.history.push(history_row("in-foreign", "sub-b"));
        assert_eq!(validate_history_target(&outcome), Err(Refused));
        outcome.explicit_stale = true;
        assert_eq!(validate_history_target(&outcome), Err(Refused));
    }

    #[test]
    fn validate_read_provenance_refuses_delayed_absence_folds_on_existing_targets() {
        for status in [
            "active",
            "past_due",
            "paused",
            "canceled",
            "incomplete_expired",
        ] {
            let absence = outcome_with("sub-a", status, false, None, None);
            assert_eq!(validate_read_provenance(&absence, true), Err(Refused));
            assert_eq!(validate_read_provenance(&absence, false), Ok(()));
            for read_anchor in [Some("in-a"), None] {
                let owned = outcome_with("sub-a", status, false, Some("plus"), read_anchor);
                assert_eq!(validate_read_provenance(&owned, true), Ok(()));
                assert_eq!(validate_read_provenance(&owned, false), Ok(()));
            }
            let stale = outcome_with("sub-a", status, true, None, None);
            assert_eq!(validate_read_provenance(&stale, true), Ok(()));
        }
    }

    fn owned_target(stale: bool, holder: bool, changed: bool, cancel: bool) -> Target {
        Target::Owned {
            predicate_stale: stale,
            other_live_holder: holder,
            equal_end_provenance_changed: changed,
            matching_proof_cancel_conflict: cancel,
        }
    }

    #[test]
    fn validate_equal_end_provenance_refuses_stale_terminal_release_and_cancel_conflicts() {
        let changed = owned_target(true, false, true, false);
        let changed_cancel = owned_target(true, false, true, true);
        let matching_cancel = owned_target(false, false, false, true);
        let check =
            |out: &ReconcileOutcome, target: &Target| validate_equal_end_provenance(out, target);
        for terminal in ["canceled", "incomplete_expired"] {
            let rel = outcome_with("sub-a", terminal, false, Some("plus"), Some("in-old"));
            assert_eq!(check(&rel, &changed), Err(Refused));
            assert_eq!(check(&rel, &changed_cancel), Err(Refused));
            assert_eq!(check(&rel, &matching_cancel), Ok(()));
            assert_eq!(check(&rel, &Target::Absent), Ok(()));
            let stale = outcome_with("sub-a", terminal, true, Some("plus"), Some("in-old"));
            assert_eq!(check(&stale, &changed_cancel), Ok(()));
        }
        for status in [
            "incomplete",
            "active",
            "past_due",
            "unpaid",
            "paused",
            "trialing",
        ] {
            let fold = outcome_with("sub-a", status, false, Some("plus"), Some("in-old"));
            assert_eq!(check(&fold, &changed), Ok(()));
            assert_eq!(check(&fold, &changed_cancel), Err(Refused));
            assert_eq!(check(&fold, &matching_cancel), Ok(()));
            let stale = outcome_with("sub-a", status, true, Some("plus"), Some("in-old"));
            assert_eq!(check(&stale, &changed_cancel), Ok(()));
        }
    }

    #[test]
    fn slot_suppression_proof_excludes_absence_terminal_and_unknown_folds() {
        let req = identity("user-a", "sub-a");
        let occupied = owned_target(false, true, false, false);
        for status in ["incomplete", "active", "past_due", "unpaid", "paused"] {
            for read_anchor in [Some("in-a"), None] {
                let owned = outcome_with("sub-a", status, false, Some("plus"), read_anchor);
                let proof = has_slot_suppression_proof(&owned);
                assert!(proof);
                assert_eq!(
                    classify_write(&req, &occupied, &[], proof),
                    Ok(Accepted::LiveSlotSuppressedOwned)
                );
            }
            let absence = outcome_with("sub-a", status, false, None, None);
            let proof = has_slot_suppression_proof(&absence);
            assert!(!proof);
            assert_eq!(classify_write(&req, &occupied, &[], proof), Err(Refused));
        }
        for status in [
            "canceled",
            "incomplete_expired",
            "trialing",
            "unknown_state",
        ] {
            for read_plan in [Some("plus"), None] {
                let out = outcome_with("sub-a", status, false, read_plan, None);
                let proof = has_slot_suppression_proof(&out);
                assert!(!proof);
                assert_eq!(classify_write(&req, &occupied, &[], proof), Err(Refused));
            }
        }
    }
}
