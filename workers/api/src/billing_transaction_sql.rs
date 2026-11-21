//! Unused SQL primitives for a future coupled billing transaction.
//!
//! These statements alone do not implement a transaction or protect history.
//! The lock and lookup must run on the same explicitly READ COMMITTED backend,
//! and the lookup must be a separate statement after the lock is acquired.
//! Production writes must resolve to this same trusted relation. Disposable
//! fixtures need their own trusted relation-specific SQL; no schema name is an
//! input to these production primitives.

/// Excludes subscription DML until the enclosing transaction ends.
pub const LOCK_SUBSCRIPTIONS: &str = "LOCK TABLE public.subscriptions IN SHARE ROW EXCLUSIVE MODE";

/// Global key lookup for stale, live-slot, and equal-end proof observations.
/// `IS TRUE` makes SQL NULL semantics yield an unambiguous stale predicate.
/// The owner is deliberately not part of the lookup predicate.
pub const LOOKUP_TARGET: &str = "SELECT user_id, stripe_subscription_id, \
    (to_timestamp($2::bigint) IS NOT NULL AND paid_through_at IS NOT NULL AND \
    (to_timestamp($2::bigint) < paid_through_at OR \
    (to_timestamp($2::bigint) = paid_through_at AND $3::text IS NULL AND \
    paid_invoice_id IS NOT NULL))) IS TRUE AS predicate_stale, \
    EXISTS (SELECT 1 FROM public.subscriptions AS live WHERE live.user_id = subscriptions.user_id \
    AND live.stripe_subscription_id <> subscriptions.stripe_subscription_id \
    AND live.status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')) AS other_live_holder, \
    (to_timestamp($2::bigint) = paid_through_at AND \
    (paid_invoice_id IS DISTINCT FROM $4::text OR plan_id IS DISTINCT FROM $5::text)) \
    IS TRUE AS equal_end_provenance_changed \
    FROM public.subscriptions WHERE stripe_subscription_id = $1";

/// Unused production write for the future transaction. It has the same 15
/// parameters as `store_reconciled_subscription_statement`, and returns the
/// written owner and subscription key for caller-side identity checking. A
/// missing row is not itself a classification of the reason for suppression.
/// This statement alone does not couple the write to the lock, lookup or history.
pub const WRITE_SUBSCRIPTION: &str = include_str!("billing_transaction_write.sql");

// The coupled store pins history to the same schema as the lock and target.
const WRITE_HISTORY: &str =
    crate::billing_reconcile::write_paid_invoice_history!("public.paid_invoice_history");

/// Ordered parameters for the global lookup: key, incoming paid-through epoch,
/// incoming paid invoice ID, read anchor, read plan. Do not filter on owner.
#[derive(Debug, PartialEq, Eq)]
pub struct LookupBinds<'a> {
    pub subscription_id: &'a str,
    pub paid_through_seconds: Option<i64>,
    pub paid_invoice_id: Option<&'a str>,
    pub read_anchor: Option<&'a str>,
    pub read_plan: Option<&'a str>,
}

impl<'a> LookupBinds<'a> {
    pub fn ordered(&self) -> (&str, Option<i64>, Option<&str>, Option<&str>, Option<&str>) {
        (
            self.subscription_id,
            self.paid_through_seconds,
            self.paid_invoice_id,
            self.read_anchor,
            self.read_plan,
        )
    }
}

#[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
pub(crate) enum ReconcileProbe {
    None,
    WorkerDeadline,
    ServerWatchdog,
    BeforeCommit,
    BeforeInsertCommit(Option<std::rc::Rc<std::cell::Cell<bool>>>),
    ObserveStaleRefusal(std::rc::Rc<std::cell::Cell<bool>>),
    ObserveTargetRefusal(std::rc::Rc<std::cell::Cell<bool>>),
    ObserveTerminalProvenanceRefusal(std::rc::Rc<std::cell::Cell<bool>>),
}

#[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
pub(crate) const PRECOMMIT_GATE: i64 = 781349054;

pub(crate) async fn apply_reconcile_transaction(
    transaction: &mut tokio_postgres::Client,
    user_id: String,
    outcome: crate::billing_reconcile::ReconcileOutcome,
    #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))] probe: ReconcileProbe,
) -> Result<(), crate::handlers::StoreError> {
    use crate::billing_store_classification::{
        classify_explicit_stale, classify_target, classify_write, has_slot_suppression_proof,
        validate_read_provenance, validate_terminal_provenance, Identity, ObservedTarget, Target,
    };
    use crate::{billing_reconcile::reconcile_lock_statement, handlers::StoreError};
    use tokio_postgres::types::Type;
    let row = &outcome.row;
    let work = async {
        transaction
            .query_typed(reconcile_lock_statement(), &[(&user_id, Type::TEXT)])
            .await
            .map_err(|_| StoreError)?;
        transaction
            .query_typed(crate::billing_transaction_sql::LOCK_SUBSCRIPTIONS, &[])
            .await
            .map_err(|_| StoreError)?;
        let binds = LookupBinds {
            subscription_id: &row.stripe_subscription_id,
            paid_through_seconds: row.paid_through_unix,
            paid_invoice_id: row.paid_invoice_id.as_deref(),
            read_anchor: outcome.read_anchor.as_deref(),
            read_plan: outcome.read_plan.as_deref(),
        };
        let (subscription_id, paid_through, paid_invoice, read_anchor, read_plan) = binds.ordered();
        let observed_rows = transaction
            .query_typed(
                crate::billing_transaction_sql::LOOKUP_TARGET,
                &[
                    (&subscription_id, Type::TEXT),
                    (&paid_through, Type::INT8),
                    (&paid_invoice, Type::TEXT),
                    (&read_anchor, Type::TEXT),
                    (&read_plan, Type::TEXT),
                ],
            )
            .await
            .map_err(|_| StoreError)?;
        let observed = observed_rows
            .iter()
            .map(|found| {
                Ok(ObservedTarget {
                    identity: Identity {
                        user_id: found.try_get("user_id")?,
                        subscription_id: found.try_get("stripe_subscription_id")?,
                    },
                    predicate_stale: found.try_get("predicate_stale")?,
                    other_live_holder: found.try_get("other_live_holder")?,
                    equal_end_provenance_changed: found.try_get("equal_end_provenance_changed")?,
                })
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(|_| StoreError)?;
        let requested = Identity {
            user_id: &user_id,
            subscription_id: &row.stripe_subscription_id,
        };
        let target = classify_target(&requested, &observed).map_err(|_| {
            #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
            if let ReconcileProbe::ObserveTargetRefusal(refused) = &probe {
                refused.set(true);
            }
            StoreError
        })?;
        crate::billing_store_classification::validate_history_target(&outcome)
            .map_err(|_| StoreError)?;
        if outcome.explicit_stale {
            classify_explicit_stale(&target, outcome.history.is_empty()).map_err(|_| {
                #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
                if let ReconcileProbe::ObserveStaleRefusal(refused) = &probe {
                    refused.set(true);
                }
                StoreError
            })?;
        } else {
            validate_read_provenance(&outcome, matches!(&target, Target::Owned { .. }))
                .map_err(|_| StoreError)?;
            validate_terminal_provenance(&outcome, &target).map_err(|_| {
                #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
                if let ReconcileProbe::ObserveTerminalProvenanceRefusal(refused) = &probe {
                    refused.set(true);
                }
                StoreError
            })?;
            let written = transaction
                .query_typed(
                    crate::billing_transaction_sql::WRITE_SUBSCRIPTION,
                    &[
                        (&user_id, Type::TEXT),
                        (&row.stripe_subscription_id, Type::TEXT),
                        (&row.stripe_customer_id, Type::TEXT),
                        (&row.stripe_item_id, Type::TEXT),
                        (&row.stripe_price_id, Type::TEXT),
                        (&row.plan_id, Type::TEXT),
                        (&row.status, Type::TEXT),
                        (&row.current_period_end_unix, Type::INT8),
                        (&row.cancel_at_period_end, Type::BOOL),
                        (&row.paid_through_unix, Type::INT8),
                        (&row.paid_invoice_id, Type::TEXT),
                        (&row.grace_until_unix, Type::INT8),
                        (&outcome.read_anchor, Type::TEXT),
                        (&outcome.read_plan, Type::TEXT),
                        (&outcome.explicit_stale, Type::BOOL),
                    ],
                )
                .await
                .map_err(|_| StoreError)?;
            let returned = written
                .iter()
                .map(|found| {
                    Ok(Identity {
                        user_id: found.try_get("user_id")?,
                        subscription_id: found.try_get("stripe_subscription_id")?,
                    })
                })
                .collect::<Result<Vec<_>, tokio_postgres::Error>>()
                .map_err(|_| StoreError)?;
            classify_write(
                &requested,
                &target,
                &returned,
                has_slot_suppression_proof(&outcome),
            )
            .map_err(|_| StoreError)?;
        }
        for history in &outcome.history {
            let (starts, ends) = history.coverage_bounds();
            transaction
                .query_typed(
                    WRITE_HISTORY,
                    &[
                        (&user_id, Type::TEXT),
                        (&history.stripe_invoice_id, Type::TEXT),
                        (&history.stripe_subscription_id, Type::TEXT),
                        (&history.plan_id, Type::TEXT),
                        (&history.period_start_unix, Type::INT8),
                        (&history.period_end_unix, Type::INT8),
                        (&history.amount_cents, Type::INT8),
                        (&history.currency, Type::TEXT),
                        (&history.paid_at_unix, Type::INT8),
                        (&starts, Type::INT8_ARRAY),
                        (&ends, Type::INT8_ARRAY),
                    ],
                )
                .await
                .map_err(|_| StoreError)?;
        }
        // The blocking probes run only after all billing writes and before COMMIT.
        #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
        if matches!(
            probe,
            ReconcileProbe::BeforeCommit | ReconcileProbe::BeforeInsertCommit(_)
        ) {
            if matches!(probe, ReconcileProbe::BeforeInsertCommit(_)) {
                // The observer still sees empty tables even if the gate is too early.
                // Require the writes on this backend before either INSERT case can wait.
                let written: bool = transaction
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM public.subscriptions
                              WHERE user_id = $1 AND stripe_subscription_id = $2
                                AND status = 'active' AND paid_invoice_id = 'in_worker_second')
                            AND (SELECT array_agg(stripe_invoice_id ORDER BY stripe_invoice_id)
                                   FROM public.paid_invoice_history
                                  WHERE user_id = $1 AND stripe_subscription_id = $2)
                                IS NOT DISTINCT FROM ARRAY['in_worker_first', 'in_worker_second']",
                        &[&user_id, &row.stripe_subscription_id],
                    )
                    .await
                    .and_then(|row| row.try_get(0))
                    .map_err(|_| StoreError)?;
                if !written {
                    return Err(StoreError);
                }
            }
            transaction
                .query_typed(
                    "SELECT pg_advisory_xact_lock($1)",
                    &[(&PRECOMMIT_GATE, Type::INT8)],
                )
                .await
                .map_err(|_| StoreError)?;
            if let ReconcileProbe::BeforeInsertCommit(Some(observed)) = &probe {
                observed.set(true);
                return Err(StoreError);
            }
        } else if matches!(
            probe,
            ReconcileProbe::WorkerDeadline | ReconcileProbe::ServerWatchdog
        ) {
            let written = transaction.query_one(
                "SELECT (SELECT count(*) FROM public.subscriptions WHERE stripe_subscription_id = 'sub_worker_fault'), (SELECT count(*) FROM public.paid_invoice_history WHERE stripe_subscription_id = 'sub_worker_fault')", &[],
            ).await.map_err(|_| StoreError)?;
            if written.get::<_, i64>(0) != 1 || written.get::<_, i64>(1) != 2 {
                return Err(StoreError);
            }
            if matches!(probe, ReconcileProbe::WorkerDeadline) {
                transaction.batch_execute("SET LOCAL idle_in_transaction_session_timeout = 0; SET LOCAL transaction_timeout = 0").await.map_err(|_| StoreError)?;
            }
            // Readiness follows acknowledgement of the timeout settings.
            transaction
                .batch_execute("SELECT pg_advisory_xact_lock(781349053)")
                .await
                .map_err(|_| StoreError)?;
            std::future::pending::<()>().await;
        }
        Ok::<(), StoreError>(())
    };
    // A failed COMMIT acknowledgement remains an unknown outcome.
    if let Err(error) = work.await {
        let _ = transaction.batch_execute("ROLLBACK").await;
        return Err(error);
    }
    transaction
        .batch_execute("COMMIT")
        .await
        .map_err(|_| StoreError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_relation_and_global_lookup_are_pinned() {
        assert_eq!(
            LOCK_SUBSCRIPTIONS,
            "LOCK TABLE public.subscriptions IN SHARE ROW EXCLUSIVE MODE"
        );
        assert_eq!(LOOKUP_TARGET, "SELECT user_id, stripe_subscription_id, (to_timestamp($2::bigint) IS NOT NULL AND paid_through_at IS NOT NULL AND (to_timestamp($2::bigint) < paid_through_at OR (to_timestamp($2::bigint) = paid_through_at AND $3::text IS NULL AND paid_invoice_id IS NOT NULL))) IS TRUE AS predicate_stale, EXISTS (SELECT 1 FROM public.subscriptions AS live WHERE live.user_id = subscriptions.user_id AND live.stripe_subscription_id <> subscriptions.stripe_subscription_id AND live.status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')) AS other_live_holder, (to_timestamp($2::bigint) = paid_through_at AND (paid_invoice_id IS DISTINCT FROM $4::text OR plan_id IS DISTINCT FROM $5::text)) IS TRUE AS equal_end_provenance_changed FROM public.subscriptions WHERE stripe_subscription_id = $1");
    }

    #[test]
    fn write_retains_legacy_binds_with_owner_and_live_slot_guards() {
        let existing = crate::billing_reconcile::store_reconciled_subscription_statement();
        let expected = existing.replacen(
            "INSERT INTO subscriptions ",
            "INSERT INTO public.subscriptions ",
            1,
        ) + " AND subscriptions.user_id = EXCLUDED.user_id AND (EXCLUDED.status NOT IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused') OR NOT EXISTS (SELECT 1 FROM public.subscriptions AS live WHERE live.user_id = EXCLUDED.user_id AND live.stripe_subscription_id <> EXCLUDED.stripe_subscription_id AND live.status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused'))) RETURNING user_id, stripe_subscription_id";
        assert_eq!(WRITE_SUBSCRIPTION.trim(), expected);

        // Pin the insert bind order and the three additional fold inputs.
        assert!(WRITE_SUBSCRIPTION.contains("SELECT $1, $2, $3, $4, $5, $6, $7, to_timestamp($8::bigint), $9, to_timestamp($10::bigint), $11, to_timestamp($12::bigint) WHERE NOT $15::boolean"));
        assert!(WRITE_SUBSCRIPTION.contains("subscriptions.plan_id = $14 AND subscriptions.paid_invoice_id IS NOT DISTINCT FROM $13"));
        let mut parameters: Vec<usize> = WRITE_SUBSCRIPTION
            .split('$')
            .skip(1)
            .map(|part| {
                part.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap()
            })
            .collect();
        parameters.sort_unstable();
        parameters.dedup();
        assert_eq!(parameters, (1..=15).collect::<Vec<_>>());
    }

    #[test]
    fn history_write_only_changes_the_legacy_relation() {
        assert_eq!(
            WRITE_HISTORY,
            crate::billing_reconcile::insert_paid_invoice_history_statement().replacen(
                "INSERT INTO paid_invoice_history ",
                "INSERT INTO public.paid_invoice_history ",
                1,
            ),
        );
    }

    #[test]
    fn lookup_bind_order_preserves_nullable_inputs() {
        let binds = LookupBinds {
            subscription_id: "sub-example",
            paid_through_seconds: Some(42),
            paid_invoice_id: Some("invoice-example"),
            read_anchor: Some("original-anchor"),
            read_plan: Some("plus"),
        };
        assert_eq!(
            binds.ordered(),
            (
                "sub-example",
                Some(42),
                Some("invoice-example"),
                Some("original-anchor"),
                Some("plus")
            )
        );
        let nulls = LookupBinds {
            subscription_id: "sub-null",
            paid_through_seconds: None,
            paid_invoice_id: None,
            read_anchor: None,
            read_plan: None,
        };
        assert_eq!(nulls.ordered(), ("sub-null", None, None, None, None));
    }
}
