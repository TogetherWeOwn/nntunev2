//! Disposable-schema runner for the unused coupled billing transaction SQL.
//! Only the generated Fx schema is substituted for the fixed `public` relation.

use nntune_api::billing_reconcile::{insert_paid_invoice_history_statement, ReconcileOutcome};
use nntune_api::billing_transaction_sql::{LOCK_SUBSCRIPTIONS, LOOKUP_TARGET, WRITE_SUBSCRIPTION};
use tokio_postgres::types::{ToSql, Type};
use tokio_postgres::Client;

#[derive(Debug, PartialEq, Eq)]
pub enum Class {
    Wrote,
    OwnStale,
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
    [LOCK_SUBSCRIPTIONS, LOOKUP_TARGET, WRITE_SUBSCRIPTION].map(|sql| {
        assert_eq!(sql.matches("public.subscriptions").count(), 1);
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
    tx.batch_execute(&lock).await.map_err(e)?;
    let held = tx
        .query_one("SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = pg_backend_pid() AND relation = $1::text::regclass AND mode = 'ShareRowExclusiveLock' AND granted)", &[&format!("{schema}.subscriptions")])
        .await.map_err(e)?;
    if !held.get::<_, bool>(0) {
        return Err("relation lock not held before lookup".into());
    }
    let r = &o.row;
    // Fresh global lookup, a separate statement after the lock; no owner filter.
    let found = tx
        .query_opt(
            &tx.prepare_typed(&lookup, &[Type::TEXT, Type::INT8, Type::TEXT])
                .await
                .map_err(e)?,
            &[
                &r.stripe_subscription_id,
                &r.paid_through_unix,
                &r.paid_invoice_id,
            ],
        )
        .await
        .map_err(e)?;
    let (own, stale) = match found {
        Some(f) => {
            let owner: String = f.try_get(0).map_err(e)?;
            let key: String = f.try_get(1).map_err(e)?;
            let stale: bool = f.try_get(2).map_err(e)?;
            if key != r.stripe_subscription_id || owner != user {
                return Err("foreign or mismatched target".into());
            }
            (Some(true), stale)
        }
        None => (None, false),
    };
    if o.history.iter().any(|h| h.stripe_subscription_id != r.stripe_subscription_id) {
        return Err("history target mismatch".into());
    }
    let class = if o.explicit_stale {
        match own {
            Some(true) => Class::OwnStale,
            Some(false) => return Err("foreign row rejected".into()),
            None if o.history.is_empty() => Class::AbsentEmpty,
            None => return Err("absent row with history rejected".into()),
        }
    } else {
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
        match rows.as_slice() {
            [row] => {
                let owner: String = row.try_get(0).map_err(e)?;
                let key: String = row.try_get(1).map_err(e)?;
                if owner != user || key != r.stripe_subscription_id {
                    return Err("write identity mismatch".into());
                }
                Class::Wrote
            }
            [] if own == Some(true) && stale => Class::OwnStale,
            _ => return Err(FixtureError::Refusal(format!("unrecognized write result: {} rows", rows.len()))),
        }
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
