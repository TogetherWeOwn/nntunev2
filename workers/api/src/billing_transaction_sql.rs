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

/// Global key lookup and the existing upsert's conflict-WHERE suppression
/// predicate. `IS TRUE` makes SQL NULL semantics yield an unambiguous boolean.
/// The owner is deliberately not part of the lookup predicate.
pub const LOOKUP_TARGET: &str = "SELECT user_id, stripe_subscription_id, \
    (to_timestamp($2::bigint) IS NOT NULL AND paid_through_at IS NOT NULL AND \
    (to_timestamp($2::bigint) < paid_through_at OR \
    (to_timestamp($2::bigint) = paid_through_at AND $3::text IS NULL AND \
    paid_invoice_id IS NOT NULL))) IS TRUE AS predicate_stale \
    FROM public.subscriptions WHERE stripe_subscription_id = $1";

/// Unused production write for the future transaction. It has the same 15
/// parameters as `store_reconciled_subscription_statement`, and returns the
/// written owner and subscription key for caller-side identity checking. A
/// missing row is not itself a classification of the reason for suppression.
/// This statement alone does not couple the write to the lock, lookup or history.
pub const WRITE_SUBSCRIPTION: &str = include_str!("billing_transaction_write.sql");

/// Ordered parameters for the global lookup: key, incoming paid-through epoch,
/// incoming paid invoice ID. Do not use an owner filter to hide foreign rows.
#[derive(Debug, PartialEq, Eq)]
pub struct LookupBinds<'a> {
    pub subscription_id: &'a str,
    pub paid_through_seconds: Option<i64>,
    pub paid_invoice_id: Option<&'a str>,
}

impl<'a> LookupBinds<'a> {
    pub fn ordered(&self) -> (&str, Option<i64>, Option<&str>) {
        (
            self.subscription_id,
            self.paid_through_seconds,
            self.paid_invoice_id,
        )
    }
}

#[cfg(target_arch = "wasm32")]
#[allow(dead_code)] // Not reachable from a release reconciliation caller.
pub(crate) async fn apply_reconcile_transaction(
    transaction: &mut tokio_postgres::Client,
    user_id: String,
    outcome: crate::billing_reconcile::ReconcileOutcome,
    _deadline_probe: bool,
) -> Result<(), crate::handlers::StoreError> {
    use crate::billing_store_classification::{
        classify_explicit_stale, classify_target, classify_write, Identity, ObservedTarget,
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
        let observed_rows = transaction
            .query_typed(
                crate::billing_transaction_sql::LOOKUP_TARGET,
                &[
                    (&row.stripe_subscription_id, Type::TEXT),
                    (&row.paid_through_unix, Type::INT8),
                    (&row.paid_invoice_id, Type::TEXT),
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
                })
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()
            .map_err(|_| StoreError)?;
        let requested = Identity {
            user_id: &user_id,
            subscription_id: &row.stripe_subscription_id,
        };
        let target = classify_target(&requested, &observed).map_err(|_| StoreError)?;
        if outcome.explicit_stale {
            classify_explicit_stale(&target, outcome.history.is_empty()).map_err(|_| StoreError)?;
        } else {
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
            classify_write(&requested, &target, &returned).map_err(|_| StoreError)?;
        }
        for history in &outcome.history {
            transaction
                .query_typed(
                    crate::billing_reconcile::insert_paid_invoice_history_statement(),
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
                    ],
                )
                .await
                .map_err(|_| StoreError)?;
        }
        // The deadline probe runs only after all billing writes.
        #[cfg(feature = "billing-owned-wasm-test")]
        if _deadline_probe {
            let written = transaction.query_one(
                "SELECT (SELECT count(*) FROM public.subscriptions WHERE stripe_subscription_id = 'sub_worker_fault'), (SELECT count(*) FROM public.paid_invoice_history WHERE stripe_subscription_id = 'sub_worker_fault')", &[],
            ).await.map_err(|_| StoreError)?;
            if written.get::<_, i64>(0) != 1 || written.get::<_, i64>(1) != 2 {
                return Err(StoreError);
            }
            transaction.batch_execute("SET LOCAL idle_in_transaction_session_timeout = 0; SET LOCAL transaction_timeout = 0").await.map_err(|_| StoreError)?;
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
        assert_eq!(LOOKUP_TARGET, "SELECT user_id, stripe_subscription_id, (to_timestamp($2::bigint) IS NOT NULL AND paid_through_at IS NOT NULL AND (to_timestamp($2::bigint) < paid_through_at OR (to_timestamp($2::bigint) = paid_through_at AND $3::text IS NULL AND paid_invoice_id IS NOT NULL))) IS TRUE AS predicate_stale FROM public.subscriptions WHERE stripe_subscription_id = $1");
    }

    #[test]
    fn write_matches_existing_statement_except_relation_guard_and_returning() {
        let existing = crate::billing_reconcile::store_reconciled_subscription_statement();
        let expected = existing.replacen(
            "INSERT INTO subscriptions ",
            "INSERT INTO public.subscriptions ",
            1,
        ) + " AND subscriptions.user_id = EXCLUDED.user_id RETURNING user_id, stripe_subscription_id";
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
    fn lookup_bind_order_preserves_nullable_inputs() {
        let binds = LookupBinds {
            subscription_id: "sub-example",
            paid_through_seconds: Some(42),
            paid_invoice_id: Some("invoice-example"),
        };
        assert_eq!(
            binds.ordered(),
            ("sub-example", Some(42), Some("invoice-example"))
        );
        let nulls = LookupBinds {
            subscription_id: "sub-null",
            paid_through_seconds: None,
            paid_invoice_id: None,
        };
        assert_eq!(nulls.ordered(), ("sub-null", None, None));
    }
}
