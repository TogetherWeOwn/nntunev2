use nntune_api::billing_reconcile::{
    insert_paid_invoice_history_statement, reconcile_lock_statement,
    store_reconciled_subscription_statement, ReconcileOutcome,
};
use tokio_postgres::types::{ToSql, Type};
use tokio_postgres::Client;

pub(super) async fn try_put(
    db: &mut Client,
    user: &str,
    outcome: &ReconcileOutcome,
) -> Result<(), tokio_postgres::Error> {
    let tx = db.transaction().await?;
    tx.query_typed(reconcile_lock_statement(), &[(&user, Type::TEXT)])
        .await?;
    let stmt = tx
        .prepare_typed(
            store_reconciled_subscription_statement(),
            &[
                Type::TEXT,
                Type::TEXT,
                Type::TEXT,
                Type::TEXT,
                Type::TEXT,
                Type::TEXT,
                Type::TEXT,
                Type::INT8,
                Type::BOOL,
                Type::INT8,
                Type::TEXT,
                Type::INT8,
                Type::TEXT,
                Type::TEXT,
                Type::BOOL,
            ],
        )
        .await?;
    let r = &outcome.row;
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
        &outcome.read_anchor,
        &outcome.read_plan,
        &outcome.explicit_stale,
    ];
    tx.query(&stmt, &params).await?;
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
                Type::INT8_ARRAY,
                Type::INT8_ARRAY,
            ],
        )
        .await?;
    for h in &outcome.history {
        let (starts, ends) = h.coverage_bounds();
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
                &starts,
                &ends,
            ],
        )
        .await?;
    }
    tx.commit().await
}
