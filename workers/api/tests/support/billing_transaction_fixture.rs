//! Disposable-schema runner for the unused coupled billing transaction SQL.
//! Only the generated Fx schema is substituted for the fixed `public` relation.

use nntune_api::billing_reconcile::{insert_paid_invoice_history_statement, ReconcileOutcome};
use nntune_api::billing_store_classification::{
    classify_explicit_stale, classify_target, classify_write, has_slot_suppression_proof,
    validate_history_target, validate_read_provenance, validate_terminal_provenance, Accepted,
    Identity, ObservedTarget, Target,
};
use nntune_api::billing_transaction_sql::{
    LookupBinds, LOCK_SUBSCRIPTIONS, LOOKUP_TARGET, WRITE_SUBSCRIPTION,
};
use tokio_postgres::types::{ToSql, Type};
use tokio_postgres::Client;

#[derive(Debug, PartialEq, Eq)]
pub enum Class {
    Wrote,
    OwnStale,
    LiveSlotSuppressed,
    AbsentEmpty,
}

#[derive(Debug)]
pub enum FixtureError {
    Postgres(tokio_postgres::Error),
    Refusal(String),
}

impl From<&str> for FixtureError {
    fn from(message: &str) -> Self {
        Self::Refusal(message.into())
    }
}

/// `schema` must be the Fx-generated identifier, never request or user input.
pub fn fixed_sql(schema: &str) -> [String; 3] {
    assert!(
        schema.starts_with("billing_test_")
            && schema
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    );
    [
        (LOCK_SUBSCRIPTIONS, 1),
        (LOOKUP_TARGET, 2),
        (WRITE_SUBSCRIPTION, 2),
    ]
    .map(|(sql, count)| {
        assert_eq!(sql.matches("public.subscriptions").count(), count);
        let out = sql.replace("public.subscriptions", &format!("{schema}.subscriptions"));
        assert!(!out.contains("public."));
        out
    })
}

pub async fn run(
    db: &mut Client,
    schema: &str,
    user: &str,
    o: &ReconcileOutcome,
) -> Result<Class, FixtureError> {
    let [lock, lookup, write] = fixed_sql(schema);
    let e = FixtureError::Postgres;
    let tx = db
        .build_transaction()
        .isolation_level(tokio_postgres::IsolationLevel::ReadCommitted)
        .start()
        .await
        .map_err(e)?;
    let result = async {
    // Model the reserved work-phase ceiling; the fixture does not model the
    // Worker connection or its outer deadline.
    tx.batch_execute("SET LOCAL transaction_timeout = 9000; SET LOCAL lock_timeout = 4000; SET LOCAL statement_timeout = 5000; SET LOCAL idle_in_transaction_session_timeout = 5000")
        .await.map_err(e)?;
    tx.query(
        &tx.prepare_typed(nntune_api::billing_reconcile::reconcile_lock_statement(), &[Type::TEXT]).await.map_err(e)?,
        &[&user],
    ).await.map_err(e)?;
    tx.batch_execute(&lock).await.map_err(e)?;
    let held = tx
        .query_one("SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = pg_backend_pid() AND relation = $1::text::regclass AND mode = 'ShareRowExclusiveLock' AND granted)", &[&format!("{schema}.subscriptions")])
        .await.map_err(e)?;
    if !held.get::<_, bool>(0) {
        return Err("relation lock not held before lookup".into());
    }
    let r = &o.row;
    let binds = LookupBinds {
        subscription_id: &r.stripe_subscription_id,
        paid_through_seconds: r.paid_through_unix,
        paid_invoice_id: r.paid_invoice_id.as_deref(),
        read_anchor: o.read_anchor.as_deref(),
        read_plan: o.read_plan.as_deref(),
    };
    let (subscription_id, paid_through, paid_invoice, read_anchor, read_plan) = binds.ordered();
    // Fresh global lookup, a separate statement after the lock; no owner filter.
    let found = tx
        .query(
            &tx.prepare_typed(&lookup, &[Type::TEXT, Type::INT8, Type::TEXT, Type::TEXT, Type::TEXT])
                .await
                .map_err(e)?,
            &[&subscription_id, &paid_through, &paid_invoice, &read_anchor, &read_plan],
        )
        .await
        .map_err(e)?;
    let observed = found
        .iter()
        .map(|row| {
            Ok(ObservedTarget {
                identity: Identity { user_id: row.try_get(0)?, subscription_id: row.try_get(1)? },
                predicate_stale: row.try_get(2)?,
                other_live_holder: row.try_get("other_live_holder")?,
                equal_end_provenance_changed: row.try_get("equal_end_provenance_changed")?,
            })
        })
        .collect::<Result<Vec<_>, tokio_postgres::Error>>()
        .map_err(e)?;
    let requested = Identity { user_id: user, subscription_id: &r.stripe_subscription_id };
    let target = classify_target(&requested, &observed).map_err(|_| "foreign or mismatched target")?;
    validate_history_target(o).map_err(|_| "history target mismatch")?;
    let accepted = if o.explicit_stale {
        classify_explicit_stale(&target, o.history.is_empty())
            .map_err(|_| "absent row with history rejected")?
    } else {
        validate_read_provenance(o, matches!(&target, Target::Owned { .. }))
            .map_err(|_| "missing exact-target read provenance")?;
        validate_terminal_provenance(o, &target)
            .map_err(|_| "changed terminal read provenance")?;
        let types = [Type::TEXT; 7]
            .into_iter()
            .chain([
                Type::INT8,
                Type::BOOL,
                Type::INT8,
                Type::TEXT,
                Type::INT8,
                Type::TEXT,
                Type::TEXT,
                Type::BOOL,
            ])
            .collect::<Vec<_>>();
        let params: [&(dyn ToSql + Sync); 15] = [
            &user,
            &r.stripe_subscription_id,
            &r.stripe_customer_id,
            &r.stripe_item_id,
            &r.stripe_price_id,
            &r.plan_id,
            &r.status,
            &r.current_period_end_unix,
            &r.cancel_at_period_end,
            &r.paid_through_unix,
            &r.paid_invoice_id,
            &r.grace_until_unix,
            &o.read_anchor,
            &o.read_plan,
            &o.explicit_stale,
        ];
        let rows = tx
            .query(&tx.prepare_typed(&write, &types).await.map_err(e)?, &params)
            .await
            .map_err(e)?;
        let returned = rows.iter().map(|row| {
            Ok(Identity { user_id: row.try_get(0)?, subscription_id: row.try_get(1)? })
        }).collect::<Result<Vec<_>, tokio_postgres::Error>>().map_err(e)?;
        classify_write(&requested, &target, &returned, has_slot_suppression_proof(o)).map_err(|_| "unrecognized write result")?
    };
    let class = match accepted {
        Accepted::AppliedOwned => Class::Wrote,
        Accepted::StaleOwned | Accepted::PredicateStaleOwned => Class::OwnStale,
        Accepted::StaleAbsentEmpty => Class::AbsentEmpty,
        Accepted::LiveSlotSuppressedOwned => Class::LiveSlotSuppressed,
    };
    let history = tx
        .prepare_typed(
            insert_paid_invoice_history_statement(),
            &[
                Type::TEXT,
                Type::TEXT,
                Type::TEXT,
                Type::TEXT,
                Type::INT8,
                Type::INT8,
                Type::INT8,
                Type::TEXT,
                Type::INT8,
            ],
        )
        .await
        .map_err(e)?;
    for h in &o.history {
        tx.execute(
            &history,
            &[
                &user,
                &h.stripe_invoice_id,
                &h.stripe_subscription_id,
                &h.plan_id,
                &h.period_start_unix,
                &h.period_end_unix,
                &h.amount_cents,
                &h.currency,
                &h.paid_at_unix,
            ],
        )
        .await
        .map_err(e)?;
    }
    Ok(class)
    }.await;
    match result {
        Ok(class) => {
            tx.commit().await.map_err(e)?;
            Ok(class)
        }
        Err(error) => {
            tx.rollback().await.map_err(e)?;
            Err(error)
        }
    }
}
