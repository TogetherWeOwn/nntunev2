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
}

#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Absent,
    Owned {
        predicate_stale: bool,
        other_live_holder: bool,
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
            },
            ObservedTarget {
                identity: identity("user-a", "sub-b"),
                predicate_stale: false,
                other_live_holder: false,
            },
        ] {
            assert_eq!(classify_target(&requested, &[row]), Err(Refused));
        }
        let rows = [
            ObservedTarget {
                identity: identity("user-a", "sub-a"),
                predicate_stale: false,
                other_live_holder: false,
            },
            ObservedTarget {
                identity: identity("user-a", "sub-a"),
                predicate_stale: false,
                other_live_holder: false,
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
        };
        let own_fresh = Target::Owned {
            predicate_stale: false,
            other_live_holder: false,
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
}
