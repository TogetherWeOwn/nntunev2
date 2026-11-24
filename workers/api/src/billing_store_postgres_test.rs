use super::{MirrorBinding, PgStore, StoreError};
use crate::billing_reconcile::{
    reconcile_exact_target, LiveRow, PaidInvoiceRow, PriceBinding, ReconcileInput,
    ReconcileOutcome, ReconciledRow, RemoteSubscription,
};
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
    dispatch_with(user, outcome, Writer::Owned).await
}

enum Writer {
    Owned,
    Release,
}

async fn dispatch_with(
    user: &str,
    outcome: &ReconcileOutcome,
    writer: Writer,
) -> Result<(), StoreError> {
    with_fixture_store(async |store| match writer {
        Writer::Owned => {
            store
                .store_reconcile_owned(user.to_owned(), outcome.clone())
                .await
        }
        Writer::Release => super::Store::store_reconcile_outcome(store, user, outcome).await,
    })
    .await
}

async fn with_fixture_store<T>(run: impl AsyncFnOnce(&PgStore) -> T) -> T {
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
    let result = run(&store).await;
    // Join before discarding the store: the call must have released its client.
    let driver = driver.take().expect("store must acquire one connection");
    tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .expect("billing connection did not close")
        .unwrap()
        .unwrap();
    result
}

async fn checkout_reservation_and_price_mapping(observer: &Client) {
    use super::Store;
    use crate::billing_reconcile::CreateOpOutcome;
    const OP: &str = "7f000000-0000-4000-8000-000000000001";
    const RACE: &str = "7f000000-0000-4000-8000-000000000002";
    const PRICE: &str = "price_checkout_fixture";
    let reserve = async |operation, user, customer| {
        with_fixture_store(async |store| {
            store
                .create_checkout_operation(operation, user, PRICE, Some(customer))
                .await
        })
        .await
    };
    observer
        .batch_execute(
            "INSERT INTO billing_accounts VALUES ('native_billing_user', 'cus_already_owned');
         INSERT INTO plan_prices VALUES
           ('price_checkout_fixture', 'plus', 'usd', 800, 'month', false, true),
           ('price_wrong_scope', 'pro', 'eur', 1, 'year', false, true);",
        )
        .await
        .unwrap();
    // The existing subscription owns USER's live slot.
    assert!(reserve(OP, USER, "cus_already_owned").await.is_err());
    // The unique customer bind fails after reserving a flight. It must roll back.
    assert!(reserve(OP, OTHER_USER, "cus_already_owned").await.is_err());
    let count: i64 = observer
        .query_one("SELECT count(*) FROM billing_operations", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0);
    observer
        .execute(
            "UPDATE users SET deleting_at = now() WHERE id = $1",
            &[&OTHER_USER],
        )
        .await
        .unwrap();
    assert!(reserve(OP, OTHER_USER, "cus_checkout").await.is_err());
    observer
        .execute(
            "UPDATE users SET deleting_at = NULL WHERE id = $1",
            &[&OTHER_USER],
        )
        .await
        .unwrap();

    let (left, right) = tokio::join!(
        reserve(OP, OTHER_USER, "cus_checkout"),
        reserve(RACE, OTHER_USER, "cus_checkout")
    );
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_eq!(left.op(), right.op());
    assert_ne!(
        matches!(left, CreateOpOutcome::Created(_)),
        matches!(right, CreateOpOutcome::Created(_))
    );
    assert!(left.op().replayable);
    let snapshot = with_fixture_store(async |store| {
        store.load_reconcile_input(OTHER_USER, None).await.unwrap()
    })
    .await;
    assert_eq!(snapshot.stripe_customer_id.as_deref(), Some("cus_checkout"));
    assert!(snapshot
        .bindings
        .iter()
        .any(|binding| binding.price_id == PRICE && binding.selectable));
    assert!(snapshot
        .bindings
        .iter()
        .any(|binding| binding.price_id == "price_wrong_scope" && !binding.selectable));

    observer
        .execute(
            "UPDATE billing_operations SET created_at = now() - interval '24 hours'",
            &[],
        )
        .await
        .unwrap();
    assert!(
        !with_fixture_store(async |store| store
            .open_checkout_operation(OTHER_USER)
            .await
            .unwrap()
            .unwrap())
        .await
        .replayable
    );
    observer.batch_execute("DELETE FROM billing_operations; DELETE FROM billing_accounts; DELETE FROM plan_prices;").await.unwrap();
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
        include_str!("../../../migrations/0006_account_deletion.sql"),
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
    checkout_reservation_and_price_mapping(&observer).await;
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

    let mut conflict = stale.clone();
    conflict.explicit_stale = false;
    conflict.read_plan = Some("plus".into());
    let mut preceding = conflict.history[0].clone();
    preceding.stripe_invoice_id = "in_native_before_conflict".into();
    conflict.history[0].amount_cents += 1;
    conflict.history.insert(0, preceding);
    assert!(dispatch(USER, &conflict).await.is_err());
    assert_eq!(snapshot(&observer).await, committed);

    dispatch(USER, &stale)
        .await
        .expect("replay against owned target");
    assert_eq!(snapshot(&observer).await, committed);

    check_legacy_mixed_plan_history(&observer, seed).await;
    drop(observer);
    driver.await.unwrap().unwrap();
}

async fn check_legacy_mixed_plan_history(observer: &Client, mut seed: ReconcileOutcome) {
    const TARGET: &str = "sub_native_mixed_plan";
    const CUSTOMER: &str = "cus_native_mixed_plan";
    const ITEM: &str = "si_native_mixed_plan";
    const LEGACY: &str = "in_native_mixed_plan";
    const FRESH: &str = "in_native_mixed_plan_fresh";
    const PLUS: &str = "price_native_mixed_plus";
    const PRO: &str = "price_native_mixed_pro";
    seed.row.stripe_subscription_id = TARGET.into();
    seed.row.stripe_customer_id = CUSTOMER.into();
    seed.row.stripe_item_id = ITEM.into();
    seed.row.stripe_price_id = PLUS.into();
    seed.row.current_period_end_unix = Some(NOW + 15);
    seed.row.paid_through_unix = Some(NOW + 15);
    seed.row.paid_invoice_id = Some(LEGACY.into());
    dispatch(OTHER_USER, &seed)
        .await
        .expect("create mixed-plan target");

    let line = |id, price, start: i64, end: i64| {
        json!({
            "id": id, "amount": 400, "price": price, "currency": "usd",
            "period": {"start": NOW + start, "end": NOW + end}
        })
    };
    let invoice = |id, amount_paid, lines| {
        serde_json::from_value(json!({
            "id": id, "customer": CUSTOMER, "subscription": TARGET, "status": "paid",
            "amount_paid": amount_paid, "currency": "usd", "status_transitions": {"paid_at": NOW},
            "lines": {"data": lines, "has_more": false}
        }))
        .unwrap()
    };
    let remote = RemoteSubscription {
        subscription: serde_json::from_value(json!({
            "id": TARGET, "customer": CUSTOMER, "status": "active", "current_period_end": NOW + 60,
            "items": {"data": [{"id": ITEM, "price": PLUS}]}
        }))
        .unwrap(),
        invoices: vec![
            invoice(FRESH, 400, vec![line("fresh_plus", PLUS, 15, 60)]),
            invoice(
                LEGACY,
                1200,
                vec![
                    line("early_pro", PRO, 1, 10),
                    line("interleaved_plus", PLUS, 0, 15),
                    line("late_pro", PRO, 10, 30),
                ],
            ),
        ],
    };
    let local = LiveRow {
        stripe_subscription_id: TARGET.into(),
        stripe_customer_id: CUSTOMER.into(),
        stripe_item_id: ITEM.into(),
        stripe_price_id: PLUS.into(),
        plan_id: "plus".into(),
        status: "active".into(),
        paid_through_unix: seed.row.paid_through_unix,
        paid_invoice_id: seed.row.paid_invoice_id.clone(),
        grace_until_unix: None,
    };
    let fold = |live| {
        reconcile_exact_target(
            TARGET,
            ReconcileInput {
                stripe_customer_id: Some(CUSTOMER.into()),
                live: Some(live),
                bindings: [(PLUS, "plus"), (PRO, "pro")]
                    .map(|(price, plan)| PriceBinding {
                        price_id: price.into(),
                        plan_id: plan.into(),
                        selectable: true,
                        livemode: false,
                    })
                    .into(),
            },
            &remote,
            NOW,
        )
        .unwrap()
    };
    let fresh = fold(local.clone());
    assert!(!fresh.explicit_stale && fresh.fail_closed.is_none());
    assert_eq!(fresh.row.paid_invoice_id.as_deref(), Some(FRESH));
    assert_eq!(fresh.row.paid_through_unix, Some(NOW + 60));
    assert_eq!(fresh.history[0].stripe_invoice_id, FRESH);
    let current = &fresh.history[1];
    assert_eq!(current.stripe_invoice_id, LEGACY);
    assert_eq!(current.plan_id, "pro");
    assert_eq!(
        (current.period_start_unix, current.period_end_unix),
        (NOW + 1, NOW + 30)
    );
    assert_eq!(
        current.covered_periods,
        [(NOW, NOW + 15), (NOW + 1, NOW + 10), (NOW + 10, NOW + 30)]
    );

    observer
        .execute(
            "INSERT INTO public.paid_invoice_history (
            stripe_invoice_id, user_id, stripe_subscription_id, plan_id, period_start, period_end,
            amount_cents, currency, paid_at
         ) VALUES ($1, $2, $3, 'plus', to_timestamp($4::bigint), to_timestamp($5::bigint),
                   $6, $7, to_timestamp($8::bigint))",
            &[
                &LEGACY,
                &OTHER_USER,
                &TARGET,
                &NOW,
                &(NOW + 15),
                &current.amount_cents,
                &current.currency,
                &current.paid_at_unix,
            ],
        )
        .await
        .unwrap();
    // #902 only counted the item plan. #926 lost the first Pro start
    // when Plus temporarily held the latest end.
    for (plan, start, end) in [("plus", NOW, NOW + 15), ("pro", NOW + 10, NOW + 30)] {
        assert_eq!(
            observer
                .execute(
                    "UPDATE public.paid_invoice_history SET plan_id = $2,
             period_start = to_timestamp($3::bigint), period_end = to_timestamp($4::bigint)
             WHERE stripe_invoice_id = $1",
                    &[&LEGACY, &plan, &start, &end],
                )
                .await
                .unwrap(),
            1
        );
        let before = snapshot(observer).await;
        assert!(
            dispatch(OTHER_USER, &fresh).await.is_err(),
            "{plan} {start} {end}"
        );
        assert_eq!(
            snapshot(observer).await,
            before,
            "history conflict must roll back the whole fold"
        );
    }

    let coverage_matches = "SELECT covered_periods = tstzmultirange(tstzrange(
             to_timestamp($2::bigint), to_timestamp($3::bigint), '[)'))
         FROM public.paid_invoice_history WHERE stripe_invoice_id = $1";
    let mut legacy_replay = fold(local.clone());
    legacy_replay.explicit_stale = true;
    legacy_replay
        .history
        .retain(|row| row.stripe_invoice_id == LEGACY);
    assert_eq!(legacy_replay.history.len(), 1);
    dispatch_with(OTHER_USER, &legacy_replay, Writer::Release)
        .await
        .expect("release writer hydrates a nonempty legacy invoice batch");
    assert_eq!(
        observer
            .query_one(coverage_matches, &[&LEGACY, &NOW, &(NOW + 30)])
            .await
            .unwrap()
            .get::<_, Option<bool>>(0),
        Some(true)
    );
    let hydrated = snapshot(observer).await;
    assert!(dispatch(OTHER_USER, &fresh).await.is_err());
    assert_eq!(snapshot(observer).await, hydrated);

    assert_eq!(observer.execute(
        "UPDATE public.paid_invoice_history SET plan_id = $2, period_start = to_timestamp($3::bigint),
         period_end = to_timestamp($4::bigint), covered_periods = NULL WHERE stripe_invoice_id = $1",
        &[&LEGACY, &current.plan_id, &current.period_start_unix, &current.period_end_unix],
    ).await.unwrap(), 1);
    dispatch(OTHER_USER, &fresh)
        .await
        .expect("matching mixed-plan history hydrates");
    let committed = snapshot(observer).await;
    let target = committed
        .0
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["stripe_subscription_id"] == TARGET)
        .unwrap();
    assert_eq!(target["paid_invoice_id"], FRESH);
    let paid_through_unix: Option<i64> = observer
        .query_one(
            "SELECT extract(epoch FROM paid_through_at)::bigint FROM public.subscriptions
         WHERE user_id = $1 AND stripe_subscription_id = $2",
            &[&OTHER_USER, &TARGET],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(paid_through_unix, Some(NOW + 60));
    assert!(committed
        .1
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["stripe_invoice_id"] == FRESH));
    assert_eq!(
        observer
            .query_one(coverage_matches, &[&LEGACY, &NOW, &(NOW + 30)],)
            .await
            .unwrap()
            .get::<_, Option<bool>>(0),
        Some(true)
    );
    let replay = fold(LiveRow {
        paid_through_unix,
        paid_invoice_id: target["paid_invoice_id"].as_str().map(str::to_owned),
        ..local
    });
    assert!(replay
        .history
        .iter()
        .any(|row| row.stripe_invoice_id == LEGACY));
    dispatch(OTHER_USER, &replay)
        .await
        .expect("matching mixed-plan replay");
    assert_eq!(snapshot(observer).await, committed);
}
