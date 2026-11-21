use super::{MirrorBinding, PgStore, StoreError};
use crate::billing_reconcile::{PaidInvoiceRow, ReconcileOutcome, ReconciledRow};
use serde_json::{json, Value};
use std::{cell::RefCell, rc::Rc, time::Duration};
use tokio_postgres::{Client, NoTls};

// This database is created from template0 by the exact-test Actions step.
const CONNECTION: &str =
    "host=127.0.0.1 user=postgres password=ci-throwaway dbname=nntune_billing_native sslmode=disable";
const USER: &str = "native_billing_user";
const OTHER_USER: &str = "native_billing_other";
const SUBSCRIPTION: &str = "sub_native_billing";
const INVOICE: &str = "in_native_billing";
const NOW: i64 = 1_786_032_000;

async fn dispatch(user: &str, outcome: &ReconcileOutcome) -> Result<(), StoreError> {
    let driver = Rc::new(RefCell::new(None));
    let driver_slot = Rc::clone(&driver);
    let store = PgStore {
        env: None,
        trace_id: "native-billing-store-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        test_client: RefCell::new(None),
        test_connect: Some(Box::new(move || {
            let driver_slot = Rc::clone(&driver_slot);
            Box::pin(async move {
                let (client, connection) =
                    tokio_postgres::connect(CONNECTION, NoTls).await.unwrap();
                assert!(driver_slot
                    .replace(Some(tokio::spawn(connection)))
                    .is_none());
                Ok(client)
            })
        })),
        test_mirror: None,
    };
    let result = store
        .store_reconcile_owned(user.to_owned(), outcome.clone())
        .await;
    // Join before discarding the store: the call must have released its client.
    let driver = driver.take().expect("store must acquire one connection");
    tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .expect("billing connection did not close")
        .unwrap()
        .unwrap();
    result
}

async fn snapshot(observer: &Client) -> (Value, Value) {
    let row = observer
        .query_one(
            "SELECT COALESCE((SELECT jsonb_agg(s ORDER BY s.stripe_subscription_id) FROM public.subscriptions s), '[]')::text,
                    COALESCE((SELECT jsonb_agg(h ORDER BY h.stripe_invoice_id) FROM public.paid_invoice_history h), '[]')::text",
            &[],
        )
        .await
        .unwrap();
    let decode = |column| serde_json::from_str(row.get::<_, &str>(column)).unwrap();
    (decode(0_usize), decode(1_usize))
}

#[tokio::test]
#[ignore = "requires the dedicated api-migrations fixture database"]
async fn billing_store_refuses_unowned_history_and_replays_when_owned() {
    let (mut observer, connection) = tokio_postgres::connect(CONNECTION, NoTls).await.unwrap();
    let driver = tokio::spawn(connection);
    let setup = observer.transaction().await.unwrap();
    let fresh: bool = setup
        .query_one(
            "SELECT pg_try_advisory_xact_lock(781349054) AND NOT EXISTS (
                 SELECT 1 FROM pg_catalog.pg_class WHERE relnamespace = 'public'::regnamespace
             )",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        fresh,
        "refusing to migrate a nonempty or busy fixture database"
    );
    for migration in [
        include_str!("../../../migrations/0002_auth_vault.sql"),
        include_str!("../../../migrations/0021_billing_schema.sql"),
        include_str!("../../../migrations/0026_plan_reseed_no_trial.sql"),
        include_str!("../../../migrations/0029_paid_invoice_history.sql"),
        include_str!("../../../migrations/0059_paid_invoice_covered_periods.sql"),
    ] {
        setup.batch_execute(migration).await.unwrap();
    }
    setup
        .execute(
            "INSERT INTO public.users (id, email, created_at) VALUES
             ($1, 'native-billing@example.test', $3), ($2, 'native-billing-other@example.test', $3)",
            &[&USER, &OTHER_USER, &NOW],
        )
        .await
        .unwrap();
    setup.commit().await.unwrap();

    let seed = ReconcileOutcome {
        row: ReconciledRow {
            stripe_subscription_id: SUBSCRIPTION.into(),
            stripe_customer_id: "cus_native_billing".into(),
            stripe_item_id: "si_native_billing".into(),
            stripe_price_id: "price_native_billing".into(),
            plan_id: "plus".into(),
            status: "active".into(),
            current_period_end_unix: Some(NOW + 100),
            cancel_at_period_end: false,
            paid_through_unix: None,
            paid_invoice_id: None,
            grace_until_unix: None,
        },
        history: vec![],
        fail_closed: None,
        explicit_stale: false,
        read_plan: None,
        read_anchor: None,
    };
    let mut stale = seed.clone();
    stale.explicit_stale = true;
    stale.row.cancel_at_period_end = true;
    let empty = stale.clone();
    stale.history.push(PaidInvoiceRow {
        stripe_invoice_id: INVOICE.into(),
        stripe_subscription_id: SUBSCRIPTION.into(),
        plan_id: "plus".into(),
        period_start_unix: NOW,
        period_end_unix: NOW + 100,
        amount_cents: 800,
        currency: "usd".into(),
        paid_at_unix: NOW,
        covered_periods: vec![(NOW, NOW + 100)],
    });

    dispatch(USER, &empty)
        .await
        .expect("absent target without history is a no-op");
    assert_eq!(snapshot(&observer).await, (json!([]), json!([])));
    assert!(dispatch(USER, &stale).await.is_err());
    assert_eq!(snapshot(&observer).await, (json!([]), json!([])));

    dispatch(USER, &seed).await.expect("create owned target");
    let (owned, history) = snapshot(&observer).await;
    assert_eq!(owned.as_array().unwrap().len(), 1);
    assert_eq!(owned[0]["stripe_subscription_id"], SUBSCRIPTION);
    assert_eq!(owned[0]["user_id"], USER);
    assert_eq!(history, json!([]));
    for foreign in [&seed, &stale, &empty] {
        assert!(dispatch(OTHER_USER, foreign).await.is_err());
        assert_eq!(snapshot(&observer).await, (owned.clone(), history.clone()));
    }

    dispatch(USER, &stale)
        .await
        .expect("retry against owned target");
    let committed = snapshot(&observer).await;
    assert_eq!(committed.0, owned, "stale outcome must not update target");
    let invoices = committed.1.as_array().unwrap();
    assert_eq!(invoices.len(), 1);
    assert_eq!(invoices[0]["stripe_invoice_id"], INVOICE);
    assert_eq!(invoices[0]["stripe_subscription_id"], SUBSCRIPTION);
    assert_eq!(invoices[0]["user_id"], USER);
    assert_eq!(invoices[0]["amount_cents"], 800);

    dispatch(USER, &stale)
        .await
        .expect("replay against owned target");
    assert_eq!(snapshot(&observer).await, committed);
    drop(observer);
    driver.await.unwrap().unwrap();
}
