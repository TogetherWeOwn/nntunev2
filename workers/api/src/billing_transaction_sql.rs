//! SQL and execution for the owned billing transaction.
//!
//! These statements alone do not implement a transaction or protect history.
//! The lock and lookup must run on the same explicitly READ COMMITTED backend,
//! and the lookup must be a separate statement after the lock is acquired.
//! Production writes must resolve to this same trusted relation. Disposable
//! fixtures need their own trusted relation-specific SQL; no schema name is an
//! input to these production primitives.

use tokio_postgres::types::{ToSql, Type};

use crate::billing_reconcile::PaidInvoiceRow;

fn valid_history_coverage(history: &PaidInvoiceRow) -> bool {
    history.covered_periods.is_empty()
        || (history.amount_cents > 0
            && !history.stripe_invoice_id.trim().is_empty()
            && history.currency.len() == 3
            && history.currency.bytes().all(|b| b.is_ascii_lowercase())
            && history
                .covered_periods
                .iter()
                .all(|(start, end)| start < end))
}

/// Excludes subscription DML until the enclosing transaction ends.
pub const LOCK_SUBSCRIPTIONS: &str = "LOCK TABLE public.subscriptions IN SHARE ROW EXCLUSIVE MODE";

/// Global key lookup for staleness, live-slot ownership, and conflicts that
/// require a fresh fold.
/// `IS TRUE` makes SQL NULL semantics yield an unambiguous stale predicate.
/// The owner is deliberately not part of the lookup predicate.
/// Keep the stale predicate in sync with the upsert's UPDATE guards. Both
/// run under the same lock, so this lookup can explain an empty RETURNING result.
pub const LOOKUP_TARGET: &str = "SELECT user_id, stripe_subscription_id, \
    (to_timestamp($2::bigint) IS NOT NULL AND paid_through_at IS NOT NULL AND \
    (to_timestamp($2::bigint) < paid_through_at OR \
    (to_timestamp($2::bigint) = paid_through_at AND (($3::text IS NULL AND \
    paid_invoice_id IS NOT NULL) OR paid_invoice_id IS DISTINCT FROM $4::text OR \
    plan_id IS DISTINCT FROM $5::text)))) IS TRUE AS predicate_stale, \
    EXISTS (SELECT 1 FROM public.subscriptions AS live WHERE live.user_id = subscriptions.user_id \
    AND live.stripe_subscription_id <> subscriptions.stripe_subscription_id \
    AND live.status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')) AS other_live_holder, \
    (to_timestamp($2::bigint) = paid_through_at AND \
    (paid_invoice_id IS DISTINCT FROM $4::text OR plan_id IS DISTINCT FROM $5::text)) \
    IS TRUE AS equal_end_provenance_changed, \
    (paid_invoice_id = $3::text AND plan_id = $6::text AND cancel_at_period_end <> $7::boolean) \
    IS TRUE AS matching_proof_cancel_conflict \
    FROM public.subscriptions WHERE stripe_subscription_id = $1";

/// The production subscription write has the same 15
/// parameters as `store_reconciled_subscription_statement`, and returns the
/// written owner and subscription key for caller-side identity checking. A
/// missing row is not itself a classification of the reason for suppression.
/// This statement alone does not couple the write to the lock, lookup or history.
pub const WRITE_SUBSCRIPTION: &str = include_str!("billing_transaction_write.sql");

// A conflict may hydrate legacy coverage only after proving all immutable
// fields match. Exact replays don't update the row.
const WRITE_HISTORY: &str = crate::billing_reconcile::insert_paid_invoice_history!(
    "public.paid_invoice_history",
    "ON CONFLICT (stripe_invoice_id) DO UPDATE SET covered_periods = EXCLUDED.covered_periods
     WHERE history.covered_periods IS NULL AND EXCLUDED.covered_periods IS NOT NULL
       AND history.amount_cents > 0
       AND (history.user_id, history.stripe_subscription_id, history.plan_id,
            history.period_start, history.period_end, history.amount_cents,
            history.currency, history.paid_at)
         = (EXCLUDED.user_id, EXCLUDED.stripe_subscription_id, EXCLUDED.plan_id,
            EXCLUDED.period_start, EXCLUDED.period_end, EXCLUDED.amount_cents,
            EXCLUDED.currency, EXCLUDED.paid_at)
     RETURNING 1"
);

const MATCH_HISTORY_REPLAY: &str = concat!(
    "SELECT 1 FROM public.paid_invoice_history
     WHERE stripe_invoice_id = $2
       AND (user_id, stripe_subscription_id, plan_id, period_start, period_end,
            amount_cents, currency, paid_at)
         = ($1, $3, $4, to_timestamp($5::bigint), to_timestamp($6::bigint),
            $7, $8, to_timestamp($9::bigint))
       AND covered_periods IS NOT DISTINCT FROM ",
    crate::billing_reconcile::paid_invoice_coverage!()
);

/// Ordered parameters for the global lookup: key, incoming paid-through epoch,
/// incoming paid invoice ID, read anchor, read plan, incoming plan and cancel flag.
#[derive(Debug, PartialEq, Eq)]
pub struct LookupBinds<'a> {
    pub subscription_id: &'a str,
    pub paid_through_seconds: Option<i64>,
    pub paid_invoice_id: Option<&'a str>,
    pub read_anchor: Option<&'a str>,
    pub read_plan: Option<&'a str>,
    pub plan: &'a str,
    pub cancel_at_period_end: bool,
}

impl<'a> LookupBinds<'a> {
    pub fn typed(&self) -> [(&(dyn ToSql + Sync), Type); 7] {
        [
            (&self.subscription_id, Type::TEXT),
            (&self.paid_through_seconds, Type::INT8),
            (&self.paid_invoice_id, Type::TEXT),
            (&self.read_anchor, Type::TEXT),
            (&self.read_plan, Type::TEXT),
            (&self.plan, Type::TEXT),
            (&self.cancel_at_period_end, Type::BOOL),
        ]
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
    ObserveEqualEndProvenanceRefusal(std::rc::Rc<std::cell::Cell<bool>>),
}

#[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
pub(crate) const PRECOMMIT_GATE: i64 = 781349054;

pub(crate) async fn apply_reconcile_transaction(
    transaction: &mut impl crate::postgres::PgBilling,
    user_id: String,
    outcome: crate::billing_reconcile::ReconcileOutcome,
    require_open_owner: bool,
    #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))] probe: ReconcileProbe,
) -> Result<(), crate::handlers::StoreError> {
    reconcile_transaction(
        transaction,
        &user_id,
        require_open_owner,
        #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
        probe,
        async |_| Ok(outcome),
    )
    .await
    .map(|_| ())
}

/// Load and fold the observation after acquiring the per-user lock. Take the
/// table-wide write lock only after the observation is complete.
pub(crate) async fn reconcile_transaction<B: crate::postgres::PgBilling>(
    transaction: &mut B,
    user_id: &str,
    require_open_owner: bool,
    #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))] probe: ReconcileProbe,
    observe: impl AsyncFnOnce(
        &B,
    ) -> Result<
        crate::billing_reconcile::ReconcileOutcome,
        crate::handlers::StoreError,
    >,
) -> Result<crate::billing_reconcile::ReconcileOutcome, crate::handlers::StoreError> {
    use crate::billing_store_classification::{
        classify_explicit_stale, classify_target, classify_write, has_slot_suppression_proof,
        validate_equal_end_provenance, validate_read_provenance, Identity, ObservedTarget, Target,
    };
    use crate::{billing_reconcile::reconcile_lock_statement, handlers::StoreError};
    let work = async {
        transaction
            .query_typed(reconcile_lock_statement(), &[(&user_id, Type::TEXT)])
            .await
            .map_err(|_| StoreError)?;
        // Hold the owner row until commit, before taking the subscriptions
        // table lock. Erasure scheduling either wins first or waits for us.
        if require_open_owner
            && transaction
                .query_typed_opt(
                    "SELECT id FROM public.users WHERE id = $1 AND deleting_at IS NULL FOR SHARE",
                    &[(&user_id, Type::TEXT)],
                )
                .await
                .map_err(|_| StoreError)?
                .is_none()
        {
            return Err(StoreError);
        }
        let outcome = observe(transaction).await?;
        crate::billing_store_classification::validate_history_target(&outcome)
            .map_err(|_| StoreError)?;
        if !outcome.history.iter().all(valid_history_coverage) {
            return Err(StoreError);
        }
        let row = &outcome.row;
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
            plan: &row.plan_id,
            cancel_at_period_end: row.cancel_at_period_end,
        };
        let observed_rows = transaction
            .query_typed(
                crate::billing_transaction_sql::LOOKUP_TARGET,
                &binds.typed(),
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
                    matching_proof_cancel_conflict: found
                        .try_get("matching_proof_cancel_conflict")?,
                })
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(|_| StoreError)?;
        let requested = Identity {
            user_id,
            subscription_id: &row.stripe_subscription_id,
        };
        let target = classify_target(&requested, &observed).map_err(|_| {
            #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
            if let ReconcileProbe::ObserveTargetRefusal(refused) = &probe {
                refused.set(true);
            }
            StoreError
        })?;
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
            validate_equal_end_provenance(&outcome, &target).map_err(|_| {
                #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
                if let ReconcileProbe::ObserveEqualEndProvenanceRefusal(refused) = &probe {
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
            let params: &[(&(dyn ToSql + Sync), Type)] = &[
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
            ];
            let written = transaction
                .query_typed_opt(WRITE_HISTORY, params)
                .await
                .map_err(|_| StoreError)?;
            // ON CONFLICT locks the conflicting row even if its WHERE fails.
            // A separate READ COMMITTED statement sees that version, including
            // an insert that won the unique key after the upsert's snapshot.
            if written.is_none()
                && transaction
                    .query_typed_opt(MATCH_HISTORY_REPLAY, params)
                    .await
                    .map_err(|_| StoreError)?
                    .is_none()
            {
                return Err(StoreError);
            }
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
                    .query_typed_one(
                        "SELECT EXISTS (SELECT 1 FROM public.subscriptions
                              WHERE user_id = $1 AND stripe_subscription_id = $2
                                AND status = 'active' AND paid_invoice_id = 'in_worker_second')
                            AND (SELECT array_agg(stripe_invoice_id ORDER BY stripe_invoice_id)
                                   FROM public.paid_invoice_history
                                  WHERE user_id = $1 AND stripe_subscription_id = $2)
                                IS NOT DISTINCT FROM ARRAY['in_worker_first', 'in_worker_second']",
                        &[
                            (&user_id, Type::TEXT),
                            (&row.stripe_subscription_id, Type::TEXT),
                        ],
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
            let written = transaction.query_typed_one(
                "SELECT (SELECT count(*) FROM public.subscriptions WHERE stripe_subscription_id = 'sub_worker_fault'), (SELECT count(*) FROM public.paid_invoice_history WHERE stripe_subscription_id = 'sub_worker_fault')", &[],
            ).await.map_err(|_| StoreError)?;
            if written.get::<_, i64>(0) != 1 || written.get::<_, i64>(1) != 2 {
                return Err(StoreError);
            }
            if matches!(probe, ReconcileProbe::WorkerDeadline) {
                transaction
                    .disable_billing_watchdogs()
                    .await
                    .map_err(|_| StoreError)?;
            }
            // Readiness follows acknowledgement of the timeout settings.
            transaction
                .query_typed(
                    "SELECT pg_advisory_xact_lock($1)",
                    &[(&781349053_i64, Type::INT8)],
                )
                .await
                .map_err(|_| StoreError)?;
            std::future::pending::<()>().await;
        }
        Ok::<_, StoreError>(outcome)
    };
    // A failed COMMIT acknowledgement remains an unknown outcome.
    let outcome = match work.await {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = transaction.rollback_billing().await;
            return Err(error);
        }
    };
    transaction.commit_billing().await.map_err(|_| StoreError)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_history_coverage_requires_paid_invoice_evidence() {
        let mut history = PaidInvoiceRow {
            stripe_invoice_id: "in_synthetic".into(),
            stripe_subscription_id: "sub_synthetic".into(),
            plan_id: "plus".into(),
            period_start_unix: 10,
            period_end_unix: 20,
            amount_cents: 1,
            currency: "usd".into(),
            paid_at_unix: 10,
            covered_periods: vec![(10, 20)],
        };
        assert!(valid_history_coverage(&history));
        history.covered_periods.push((30, 30));
        assert!(!valid_history_coverage(&history));
        history.covered_periods.pop();
        history.amount_cents = 0;
        assert!(!valid_history_coverage(&history));
        history.covered_periods.clear();
        assert!(valid_history_coverage(&history));
    }

    #[test]
    fn fixed_relation_and_global_lookup_are_pinned() {
        assert_eq!(
            LOCK_SUBSCRIPTIONS,
            "LOCK TABLE public.subscriptions IN SHARE ROW EXCLUSIVE MODE"
        );
        assert_eq!(LOOKUP_TARGET, "SELECT user_id, stripe_subscription_id, (to_timestamp($2::bigint) IS NOT NULL AND paid_through_at IS NOT NULL AND (to_timestamp($2::bigint) < paid_through_at OR (to_timestamp($2::bigint) = paid_through_at AND (($3::text IS NULL AND paid_invoice_id IS NOT NULL) OR paid_invoice_id IS DISTINCT FROM $4::text OR plan_id IS DISTINCT FROM $5::text)))) IS TRUE AS predicate_stale, EXISTS (SELECT 1 FROM public.subscriptions AS live WHERE live.user_id = subscriptions.user_id AND live.stripe_subscription_id <> subscriptions.stripe_subscription_id AND live.status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')) AS other_live_holder, (to_timestamp($2::bigint) = paid_through_at AND (paid_invoice_id IS DISTINCT FROM $4::text OR plan_id IS DISTINCT FROM $5::text)) IS TRUE AS equal_end_provenance_changed, (paid_invoice_id = $3::text AND plan_id = $6::text AND cancel_at_period_end <> $7::boolean) IS TRUE AS matching_proof_cancel_conflict FROM public.subscriptions WHERE stripe_subscription_id = $1");
    }

    #[test]
    fn write_retains_legacy_binds_with_provenance_owner_and_live_slot_guards() {
        let existing = crate::billing_reconcile::store_reconciled_subscription_statement();
        let expected = existing.replacen(
            "INSERT INTO subscriptions ",
            "INSERT INTO public.subscriptions ",
            1,
        ) + " AND (EXCLUDED.paid_through_at = subscriptions.paid_through_at AND (subscriptions.paid_invoice_id IS DISTINCT FROM $13 OR subscriptions.plan_id IS DISTINCT FROM $14)) IS NOT TRUE AND subscriptions.user_id = EXCLUDED.user_id AND (EXCLUDED.status NOT IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused') OR NOT EXISTS (SELECT 1 FROM public.subscriptions AS live WHERE live.user_id = EXCLUDED.user_id AND live.stripe_subscription_id <> EXCLUDED.stripe_subscription_id AND live.status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused'))) RETURNING user_id, stripe_subscription_id";
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
    fn history_uses_the_fixed_relation_for_inserts_and_replay() {
        assert!(WRITE_HISTORY.starts_with("INSERT INTO public.paid_invoice_history "));
        assert!(MATCH_HISTORY_REPLAY.starts_with("SELECT 1 FROM public.paid_invoice_history"));
    }

    #[test]
    fn lookup_bind_order_preserves_nullable_inputs() {
        let binds = LookupBinds {
            subscription_id: "sub-example",
            paid_through_seconds: Some(42),
            paid_invoice_id: Some("invoice-example"),
            read_anchor: Some("original-anchor"),
            read_plan: Some("plus"),
            plan: "pro",
            cancel_at_period_end: true,
        };
        let nulls = LookupBinds {
            subscription_id: "sub-null",
            paid_through_seconds: None,
            paid_invoice_id: None,
            read_anchor: None,
            read_plan: None,
            plan: "free",
            cancel_at_period_end: false,
        };
        for (binds, expected) in [
            (
                binds,
                [
                    "\"sub-example\"",
                    "Some(42)",
                    "Some(\"invoice-example\")",
                    "Some(\"original-anchor\")",
                    "Some(\"plus\")",
                    "\"pro\"",
                    "true",
                ],
            ),
            (
                nulls,
                [
                    "\"sub-null\"",
                    "None",
                    "None",
                    "None",
                    "None",
                    "\"free\"",
                    "false",
                ],
            ),
        ] {
            let params = binds.typed();
            assert_eq!(
                params.clone().map(|(_, ty)| ty),
                [
                    Type::TEXT,
                    Type::INT8,
                    Type::TEXT,
                    Type::TEXT,
                    Type::TEXT,
                    Type::TEXT,
                    Type::BOOL
                ]
            );
            assert_eq!(params.map(|(value, _)| format!("{value:?}")), expected);
        }
    }
}
