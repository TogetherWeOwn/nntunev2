//! Disposable Worker-only control for the real reconciliation store.
use super::{billing_budget, MirrorBinding, PgStore, Store, StoreError};
use crate::billing_reconcile::{PaidInvoiceRow, ReconcileOutcome, ReconciledRow};

const CONNECTION: &str =
    "host=127.0.0.1 user=postgres password=ci-throwaway dbname=nntune_billing_ack sslmode=disable";

async fn fixture(setup: bool) -> Result<(), StoreError> {
    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |client| Box::pin(async move {
            if setup {
                // Serialize our own fixture setups and refuse every existing public
                // relation: the migrations also alter or drop other billing tables.
                client.batch_execute("BEGIN").await.map_err(|_| StoreError)?;
                let safe: bool = client.query_one(
                    "SELECT pg_try_advisory_xact_lock(781349051) AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public')", &[]
                ).await.map_err(|_| StoreError)?.get(0);
                if !safe { return Err(StoreError); }
                for migration in [
                    include_str!("../../../migrations/0002_auth_vault.sql"),
                    include_str!("../../../migrations/0021_billing_schema.sql"),
                    include_str!("../../../migrations/0026_plan_reseed_no_trial.sql"),
                    include_str!("../../../migrations/0029_paid_invoice_history.sql"),
                ] {
                    client.batch_execute(migration).await.map_err(|_| StoreError)?;
                }
                client.batch_execute("INSERT INTO public.users (id,email,created_at) VALUES ('worker_fault_user','worker-fault@example.test',1786032000); COMMIT").await.map_err(|_| StoreError)?;
            } else {
                let row = client.query_one("SELECT (SELECT count(*) FROM public.subscriptions WHERE stripe_subscription_id = 'sub_worker_fault'), (SELECT count(*) FROM public.paid_invoice_history WHERE stripe_subscription_id = 'sub_worker_fault')", &[]).await.map_err(|_| StoreError)?;
                if row.get::<_, i64>(0) != 0 || row.get::<_, i64>(1) != 0 { return Err(StoreError); }
            }
            Ok(())
        }),
        |_| {},
    ).await
}

async fn deadline_after_writes(outcome: &ReconcileOutcome) -> Result<(), StoreError> {
    use futures_util::future::{select, Either};
    use std::time::Duration;

    let expired = std::rc::Rc::new(std::cell::Cell::new(false));
    let store = PgStore {
        env: None,
        trace_id: "worker-deadline-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: Some(expired.clone()),
    };
    let mut operation = Box::pin(store.store_reconcile_outcome("worker_fault_user", outcome));
    let observe = billing_budget::with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        |client| {
            Box::pin(async move {
                for _ in 0..40 {
                    let rows = client.query("SELECT a.pid FROM pg_catalog.pg_stat_activity a JOIN pg_catalog.pg_locks l ON l.pid = a.pid WHERE a.application_name = 'owned_billing_reconcile_test' AND a.state = 'idle in transaction' AND l.locktype = 'advisory' AND l.classid = 0 AND l.objid = 781349053 AND l.granted", &[]).await.map_err(|_| StoreError)?;
                    if rows.len() > 1 {
                        return Err(StoreError);
                    }
                    if let Some(row) = rows.first() {
                        return Ok(row.get::<_, i32>(0));
                    }
                    worker::Delay::from(Duration::from_millis(50)).await;
                }
                Err(StoreError)
            })
        },
        |_| {},
    );
    let pid = match select(operation.as_mut(), Box::pin(observe)).await {
        Either::Left(_) => return Err(StoreError),
        Either::Right((result, _)) => result?,
    };
    // Require the operation to remain pending beyond PostgreSQL's normal
    // five-second statement limit; then the Worker's bounded work must end it.
    if let Either::Left(_) = select(
        operation.as_mut(),
        Box::pin(worker::Delay::from(Duration::from_secs(5))),
    )
    .await
    {
        return Err(StoreError);
    }
    if operation.await.is_ok() || !expired.get() {
        return Err(StoreError);
    }
    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |client| Box::pin(async move {
            for _ in 0..40 {
                let present: bool = client.query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)", &[&pid]).await.map_err(|_| StoreError)?.get(0);
                if !present { return Ok(()); }
                worker::Delay::from(Duration::from_millis(50)).await;
            }
            Err(StoreError)
        }),
        |_| {},
    ).await?;
    fixture(false).await
}

fn test_outcome() -> ReconcileOutcome {
    let history = |invoice: &str, amount| PaidInvoiceRow {
        stripe_invoice_id: invoice.into(),
        stripe_subscription_id: "sub_worker_fault".into(),
        plan_id: "plus".into(),
        period_start_unix: 1_786_032_000,
        period_end_unix: 1_786_032_100,
        amount_cents: amount,
        currency: "usd".into(),
        paid_at_unix: 1_786_032_000,
    };
    ReconcileOutcome {
        row: ReconciledRow {
            stripe_subscription_id: "sub_worker_fault".into(),
            stripe_customer_id: "cus_worker_fault".into(),
            stripe_item_id: "si_worker_fault".into(),
            stripe_price_id: "price_worker_fault".into(),
            plan_id: "plus".into(),
            status: "active".into(),
            current_period_end_unix: Some(1_786_032_100),
            cancel_at_period_end: false,
            paid_through_unix: Some(1_786_032_100),
            paid_invoice_id: Some("in_worker_second".into()),
            grace_until_unix: None,
        },
        history: vec![
            history("in_worker_first", 800),
            history("in_worker_second", 800),
        ],
        fail_closed: None,
        explicit_stale: false,
        read_plan: None,
        read_anchor: None,
    }
}

pub(crate) async fn test_reconcile_deadline() -> Result<(), &'static str> {
    fixture(true)
        .await
        .map_err(|_| "billing fixture setup failed")?;
    deadline_after_writes(&test_outcome())
        .await
        .map_err(|_| "billing deadline after writes control failed")
}
