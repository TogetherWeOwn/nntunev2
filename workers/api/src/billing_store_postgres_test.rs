use super::{MirrorBinding, PgStore, StoreError};
use crate::billing_reconcile::{
    reconcile_exact_target, LiveRow, PaidInvoiceRow, PriceBinding, ReconcileInput,
    ReconcileOutcome, ReconciledRow, RemoteSubscription,
};
use serde_json::{json, Value};
use std::{cell::RefCell, rc::Rc, time::Duration};
use tokio_postgres::error::SqlState;
use tokio_postgres::{Client, NoTls};

// This database is created from template0 by the exact-test Actions step.
const CONNECTION: &str =
    "host=127.0.0.1 user=postgres password=ci-throwaway dbname=nntune_billing_native sslmode=disable";
const CALLER_CONNECTION: &str =
    "host=127.0.0.1 user=billing_native_caller_fixture password=ci-throwaway dbname=nntune_billing_native sslmode=disable";
const USER: &str = "native_billing_user";
const OTHER_USER: &str = "native_billing_other";
const SUBSCRIPTION: &str = "sub_native_billing";
const INVOICE: &str = "in_native_billing";
const NOW: i64 = 1_786_032_000;
const PLUS: &str = "price_native_mixed_plus";
const PRO: &str = "price_native_mixed_pro";

async fn dispatch(user: &str, outcome: &ReconcileOutcome) -> Result<(), StoreError> {
    with_fixture_store(async |store| {
        super::Store::store_paid_reconcile_outcome(store, user, outcome).await
    })
    .await
}

async fn dispatch_caller(
    user: &str,
    target: &str,
    remote: &RemoteSubscription,
) -> Result<ReconcileOutcome, StoreError> {
    dispatch_caller_at(user, target, remote, NOW).await
}

async fn dispatch_caller_at(
    user: &str,
    target: &str,
    remote: &RemoteSubscription,
    now: i64,
) -> Result<ReconcileOutcome, StoreError> {
    let expected_connections = if target == remote.subscription.id {
        2
    } else {
        0
    };
    with_store(CALLER_CONNECTION, expected_connections, async |store| {
        store
            .reconcile_owned_subscription(
                user,
                target,
                remote.subscription.clone(),
                async |subscription| {
                    assert_eq!(subscription.id, target);
                    Ok(remote.invoices.clone())
                },
                now,
            )
            .await
    })
    .await
}

async fn with_fixture_store<T>(run: impl AsyncFnOnce(&PgStore) -> T) -> T {
    with_store(CONNECTION, 1, run).await
}

async fn with_store<T>(
    connection: &'static str,
    expected_connections: usize,
    run: impl AsyncFnOnce(&PgStore) -> T,
) -> T {
    let drivers = Rc::new(RefCell::new(Vec::new()));
    let driver_slots = Rc::clone(&drivers);
    let store = PgStore {
        env: None,
        trace_id: "native-billing-store-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        test_client: RefCell::new(None),
        test_connect: Some(Box::new(move || {
            let driver_slots = Rc::clone(&driver_slots);
            Box::pin(async move {
                let (client, connection) =
                    tokio_postgres::connect(connection, NoTls).await.unwrap();
                driver_slots.borrow_mut().push(tokio::spawn(connection));
                Ok(client)
            })
        })),
        test_mirror: None,
    };
    let result = run(&store).await;
    let drivers = drivers.take();
    assert_eq!(drivers.len(), expected_connections);
    // Join before discarding the Store to verify the caller released every client.
    for driver in drivers {
        tokio::time::timeout(Duration::from_secs(5), driver)
            .await
            .expect("billing connection did not close")
            .unwrap()
            .unwrap();
    }
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
    let sql = async |statement: &str| observer.batch_execute(statement).await.unwrap();
    sql(
        "INSERT INTO billing_accounts VALUES ('native_billing_user', 'cus_already_owned');
         INSERT INTO plans VALUES ('checkout-fixture', 800, 100, 1);
         INSERT INTO plan_prices VALUES
           ('price_checkout_fixture', 'plus', 'usd', 800, 'month', false, true),
           ('price_currency', 'pro', 'eur', 1500, 'month', false, true),
           ('price_interval', 'max', 'usd', 2500, 'year', false, true),
           ('price_amount', 'checkout-fixture', 'usd', 799, 'month', false, true);",
    )
    .await;
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
    sql("UPDATE users SET deleting_at = now() WHERE id = 'native_billing_other';").await;
    assert!(reserve(OP, OTHER_USER, "cus_checkout").await.is_err());
    sql(
        "UPDATE users SET deleting_at = NULL WHERE id = 'native_billing_other';
         SELECT pg_advisory_lock(hashtext('native_billing_other'));",
    )
    .await;
    let commit_subscription = async {
        let waiting = "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype = 'advisory'
            AND NOT granted AND pg_backend_pid() = ANY(pg_blocking_pids(pid)))";
        while !observer
            .query_one(waiting, &[])
            .await
            .unwrap()
            .get::<_, bool>(0)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        sql(
            "INSERT INTO subscriptions (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status)
         VALUES ('native_billing_other', 'sub_reservation_race', 'cus_checkout',
             'si_reservation_race', 'price_checkout_fixture', 'plus', 'active');",
        )
        .await;
    };
    let writer = async {
        let result = tokio::time::timeout(Duration::from_secs(2), commit_subscription).await;
        sql("SELECT pg_advisory_unlock(hashtext('native_billing_other'));").await;
        result
    };
    let (reservation, completed) = tokio::join!(reserve(OP, OTHER_USER, "cus_checkout"), writer);
    completed.expect("checkout never waited on the subscription writer");
    assert!(reservation.is_err());
    sql("DELETE FROM subscriptions WHERE stripe_subscription_id = 'sub_reservation_race'").await;

    let (left, right) = tokio::join!(
        reserve(OP, OTHER_USER, "cus_checkout"),
        reserve(RACE, OTHER_USER, "cus_checkout")
    );
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_eq!(left.op(), right.op());
    let created = |outcome: &CreateOpOutcome| matches!(outcome, CreateOpOutcome::Created(_));
    assert_ne!(created(&left), created(&right));
    assert!(left.op().replayable);
    let snapshot = with_fixture_store(async |store| {
        store.load_reconcile_input(OTHER_USER, None).await.unwrap()
    })
    .await;
    assert_eq!(snapshot.stripe_customer_id.as_deref(), Some("cus_checkout"));
    let selectable: Vec<_> = snapshot
        .bindings
        .iter()
        .filter(|row| row.selectable)
        .map(|row| row.price_id.as_str())
        .collect();
    assert_eq!(selectable, [PRICE]);
    assert_eq!(snapshot.bindings.len(), 4);

    sql("UPDATE billing_operations SET created_at = now() - interval '23 hours 30 minutes'").await;
    let aged =
        with_fixture_store(async |store| store.open_checkout_operation(OTHER_USER).await).await;
    let aged = aged.unwrap().unwrap();
    assert!(!aged.replayable);
    sql("UPDATE public.users SET deleting_at = now() WHERE id = 'native_billing_other';").await;
    for state in ["succeeded", crate::billing_reconcile::SUPERSEDED_OP_STATE] {
        let marked = with_fixture_store(async |store| {
            store
                .update_checkout_operation(&aged.id, state, PRICE, Some("cs_fixture"))
                .await
        })
        .await
        .unwrap();
        assert!(!marked);
    }
    sql("UPDATE public.users SET deleting_at = NULL WHERE id = 'native_billing_other';").await;
    sql("DELETE FROM billing_operations; DELETE FROM billing_accounts; DELETE FROM plan_prices; DELETE FROM plans WHERE id = 'checkout-fixture';").await;
}

async fn hourly_billing_query_rotates_due_targets(observer: &Client) {
    use crate::billing_sweep::{BATCH_SIZE, DUE_TARGETS};
    observer
        .batch_execute(
            "INSERT INTO users (id, email, created_at, deleting_at)
         SELECT 'sweep-' || n, 'sweep-' || n || '@example.test', 1,
                CASE WHEN n = 21 THEN now() ELSE NULL END
         FROM generate_series(1, 21) n;
         INSERT INTO billing_operations (user_id, kind, state, request_key, request_payload)
         SELECT id, 'checkout', 'pending', id, '{}' FROM users WHERE id LIKE 'sweep-%';
         INSERT INTO subscriptions (user_id, stripe_subscription_id, stripe_customer_id,
             stripe_subscription_item_id, stripe_price_id, plan_id, status, paid_through_at)
         VALUES ('native_billing_other', 'sub_future', 'cus_future',
             'si_future', 'price_future', 'plus', 'active', now() + interval '1 day');",
        )
        .await
        .unwrap();
    let mut seen = std::collections::HashSet::new();
    for hour in [0_i64, 1] {
        let rows = observer
            .query(DUE_TARGETS, &[&hour, &BATCH_SIZE])
            .await
            .unwrap();
        assert_eq!(rows.len(), BATCH_SIZE as usize);
        for row in rows {
            let user: String = row.get(0);
            let subscription: Option<String> = row.get(1);
            assert_ne!(user, "sweep-21");
            assert_ne!(user, OTHER_USER, "future paid coverage is not due");
            assert_eq!(
                subscription.as_deref(),
                (user == USER).then_some(SUBSCRIPTION)
            );
            seen.insert(user);
        }
    }
    assert_eq!(seen.len(), 21);
    assert!(seen.contains(USER));
    observer
        .batch_execute(
            "DELETE FROM subscriptions WHERE stripe_subscription_id = 'sub_future';
         DELETE FROM billing_operations WHERE user_id LIKE 'sweep-%';
         DELETE FROM users WHERE id LIKE 'sweep-%';",
        )
        .await
        .unwrap();
}

async fn create_while_deletion_waits(observer: &Client, outcome: &ReconcileOutcome) {
    observer
        .batch_execute("BEGIN; LOCK TABLE public.subscriptions IN SHARE ROW EXCLUSIVE MODE")
        .await
        .unwrap();
    let probe = async {
        let waiting = "SELECT EXISTS (SELECT 1 FROM pg_locks
            WHERE relation = 'public.subscriptions'::regclass AND NOT granted
              AND pg_backend_pid() = ANY(pg_blocking_pids(pid)))";
        let reached = tokio::time::timeout(Duration::from_secs(2), async {
            while !observer
                .query_one(waiting, &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        let blocked = if reached {
            let (deleter, connection) = tokio_postgres::connect(CONNECTION, NoTls).await.unwrap();
            let driver = tokio::spawn(connection);
            deleter
                .batch_execute("SET lock_timeout = '200ms'")
                .await
                .unwrap();
            let result = deleter
                .execute(
                    "UPDATE public.users SET deleting_at = now() WHERE id = $1",
                    &[&USER],
                )
                .await;
            drop(deleter);
            driver.await.unwrap().unwrap();
            matches!(result, Err(err) if err.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE))
        } else {
            false
        };
        observer.batch_execute("COMMIT").await.unwrap();
        blocked
    };
    let (created, blocked) = tokio::join!(dispatch(USER, outcome), probe);
    assert!(
        blocked,
        "owner deletion must wait while the paid transaction is open"
    );
    created.expect("create owned target after releasing the fixture table lock");
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

    create_while_deletion_waits(&observer, &seed).await;
    checkout_reservation_and_price_mapping(&observer).await;
    hourly_billing_query_rotates_due_targets(&observer).await;
    let (owned, history) = snapshot(&observer).await;
    assert_eq!(owned.as_array().unwrap().len(), 1);
    assert_eq!(owned[0]["stripe_subscription_id"], SUBSCRIPTION);
    assert_eq!(owned[0]["user_id"], USER);
    assert_eq!(history, json!([]));
    for foreign in [&seed, &stale, &empty] {
        assert!(dispatch(OTHER_USER, foreign).await.is_err());
        assert_eq!(snapshot(&observer).await, (owned.clone(), history.clone()));
    }

    observer
        .execute(
            "UPDATE public.users SET deleting_at = now() WHERE id = $1",
            &[&USER],
        )
        .await
        .unwrap();
    let mut live_update = seed.clone();
    live_update.read_plan = Some(seed.row.plan_id.clone());
    live_update.row.cancel_at_period_end = true;
    assert!(dispatch(USER, &live_update).await.is_err());
    assert!(dispatch(USER, &stale).await.is_err());
    assert_eq!(snapshot(&observer).await, (owned.clone(), history.clone()));
    observer
        .execute(
            "UPDATE public.users SET deleting_at = NULL WHERE id = $1",
            &[&USER],
        )
        .await
        .unwrap();

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
    scheduled_reconciliation_preserves_later_plan_update(&observer).await;
    concurrent_collision_and_target_identity_preserve_entitlement_and_release_locks(&observer)
        .await;
    drop(observer);
    driver.await.unwrap().unwrap();
}

async fn check_legacy_mixed_plan_history(observer: &Client, mut seed: ReconcileOutcome) {
    const TARGET: &str = "sub_native_mixed_plan";
    const CUSTOMER: &str = "cus_native_mixed_plan";
    const ITEM: &str = "si_native_mixed_plan";
    const LEGACY: &str = "in_native_mixed_plan";
    const FRESH: &str = "in_native_mixed_plan_fresh";
    const RENEWAL: &str = "in_native_mixed_plan_renewal";
    const BOUNDARY: i64 = NOW + 60;
    const RENEWAL_END: i64 = BOUNDARY + 30 * 86_400;
    seed.row.stripe_subscription_id = TARGET.into();
    seed.row.stripe_customer_id = CUSTOMER.into();
    seed.row.stripe_item_id = ITEM.into();
    seed.row.stripe_price_id = PLUS.into();
    seed.row.status = "trialing".into();
    seed.row.current_period_end_unix = Some(NOW + 15);
    seed.row.paid_through_unix = Some(NOW + 15);
    seed.row.paid_invoice_id = Some(LEGACY.into());
    dispatch(OTHER_USER, &seed)
        .await
        .expect("create mixed-plan target");
    observer
        .batch_execute(
            "CREATE ROLE billing_native_caller_fixture LOGIN NOSUPERUSER NOINHERIT
                NOCREATEDB NOCREATEROLE NOBYPASSRLS PASSWORD 'ci-throwaway';
             REVOKE CONNECT ON DATABASE nntune_billing_native FROM PUBLIC;
             GRANT CONNECT ON DATABASE nntune_billing_native TO billing_native_caller_fixture;
             GRANT USAGE ON SCHEMA public TO billing_native_caller_fixture;
             GRANT SELECT ON public.subscriptions, public.billing_accounts
                TO billing_native_caller_fixture;
             GRANT SELECT (id, price_cents) ON public.plans
                TO billing_native_caller_fixture;",
        )
        .await
        .unwrap();
    observer
        .execute(
            "INSERT INTO public.billing_accounts (user_id, stripe_customer_id) VALUES ($1, $2)",
            &[&OTHER_USER, &CUSTOMER],
        )
        .await
        .unwrap();
    observer
        .execute(
            "INSERT INTO public.plan_prices
             (stripe_price_id, plan_id, currency, price_cents, billing_interval, livemode, selectable)
             VALUES ($1, 'plus', 'usd', 800, 'month', false, true),
                    ($2, 'pro', 'usd', 1500, 'month', false, true)",
            &[&PLUS, &PRO],
        )
        .await
        .unwrap();

    let line = |id, price, start: i64, end: i64| {
        json!({
            "id": id, "amount": 400, "price": price, "currency": "usd",
            "period": {"start": start, "end": end}
        })
    };
    let invoice = |id, amount_paid, lines, paid_at| {
        serde_json::from_value(json!({
            "id": id, "customer": CUSTOMER, "subscription": TARGET, "status": "paid",
            "amount_paid": amount_paid, "currency": "usd", "status_transitions": {"paid_at": paid_at},
            "lines": {"data": lines, "has_more": false}
        }))
        .unwrap()
    };
    let remote = RemoteSubscription {
        subscription: serde_json::from_value(json!({
            "id": TARGET, "customer": CUSTOMER, "status": "active", "current_period_end": BOUNDARY,
            "items": {"data": [{"id": ITEM, "price": PLUS}]}
        }))
        .unwrap(),
        invoices: vec![
            invoice(
                FRESH,
                400,
                vec![line("fresh_plus", PLUS, NOW + 15, BOUNDARY)],
                NOW,
            ),
            invoice(
                LEGACY,
                1200,
                vec![
                    line("early_pro", PRO, NOW + 1, NOW + 10),
                    line("interleaved_plus", PLUS, NOW, NOW + 15),
                    line("late_pro", PRO, NOW + 10, NOW + 30),
                ],
                NOW,
            ),
        ],
    };
    let local = LiveRow {
        stripe_subscription_id: TARGET.into(),
        stripe_customer_id: CUSTOMER.into(),
        stripe_item_id: ITEM.into(),
        stripe_price_id: PLUS.into(),
        plan_id: "plus".into(),
        status: seed.row.status.clone(),
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
    assert_eq!(fresh.row.paid_through_unix, Some(BOUNDARY));
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
    let before = snapshot(observer).await;
    let denied_read = with_store(CALLER_CONNECTION, 1, async |store| {
        store
            .reconcile_owned_subscription(
                OTHER_USER,
                TARGET,
                remote.subscription.clone(),
                async |_| panic!("must read price bindings before loading invoices"),
                NOW,
            )
            .await
    })
    .await;
    assert!(denied_read.is_err());
    assert_eq!(snapshot(observer).await, before);
    observer
        .batch_execute(
            "GRANT SELECT ON public.plan_prices TO billing_native_caller_fixture;
             GRANT SELECT (id, deleting_at), UPDATE (id)
                ON public.users TO billing_native_caller_fixture;",
        )
        .await
        .unwrap();

    // An anchored terminal observation without its anchor is explicitly stale.
    // The owned transaction checks the lock and target without writing either table.
    let mut stale = remote.clone();
    stale.subscription.status = "canceled".into();
    stale.invoices.clear();
    check_caller_lock_privilege().await;
    assert!(dispatch_caller(OTHER_USER, TARGET, &stale).await.is_err());
    assert_eq!(snapshot(observer).await, before);
    observer
        .batch_execute(
            "GRANT INSERT, UPDATE ON public.subscriptions TO billing_native_caller_fixture",
        )
        .await
        .unwrap();
    let locked = dispatch_caller(OTHER_USER, TARGET, &stale)
        .await
        .expect("stale no-op only needs the subscription lock and reads");
    assert!(locked.explicit_stale && locked.history.is_empty());
    assert_eq!(snapshot(observer).await, before);
    observer
        .batch_execute(
            "GRANT INSERT, UPDATE (covered_periods),
                SELECT (stripe_invoice_id, user_id, stripe_subscription_id, plan_id,
                    period_start, period_end, amount_cents, currency, paid_at, covered_periods)
                ON public.paid_invoice_history TO billing_native_caller_fixture;",
        )
        .await
        .unwrap();
    assert!(dispatch_caller(OTHER_USER, SUBSCRIPTION, &remote)
        .await
        .is_err());
    assert!(dispatch_caller(USER, TARGET, &remote).await.is_err());
    assert_eq!(snapshot(observer).await, before);
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
            dispatch_caller(OTHER_USER, TARGET, &remote).await.is_err(),
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
    with_fixture_store(async |store| {
        super::Store::store_reconcile_outcome(store, OTHER_USER, &legacy_replay).await
    })
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
    assert!(dispatch_caller(OTHER_USER, TARGET, &remote).await.is_err());
    assert_eq!(snapshot(observer).await, hydrated);

    assert_eq!(observer.execute(
        "UPDATE public.paid_invoice_history SET plan_id = $2, period_start = to_timestamp($3::bigint),
         period_end = to_timestamp($4::bigint), covered_periods = NULL WHERE stripe_invoice_id = $1",
        &[&LEGACY, &current.plan_id, &current.period_start_unix, &current.period_end_unix],
    ).await.unwrap(), 1);
    let before = snapshot(observer).await;
    let mut altered = remote.clone();
    altered.invoices[1].amount_paid += 1;
    assert!(dispatch_caller(OTHER_USER, TARGET, &altered).await.is_err());
    assert_eq!(snapshot(observer).await, before);

    observer
        .batch_execute(
            "REVOKE INSERT ON public.paid_invoice_history FROM billing_native_caller_fixture",
        )
        .await
        .unwrap();
    assert!(dispatch_caller(OTHER_USER, TARGET, &remote).await.is_err());
    assert_eq!(snapshot(observer).await, before);
    observer
        .batch_execute(
            "GRANT INSERT ON public.paid_invoice_history TO billing_native_caller_fixture",
        )
        .await
        .unwrap();
    observer
        .execute(
            "UPDATE public.users SET deleting_at = now() WHERE id = $1",
            &[&OTHER_USER],
        )
        .await
        .unwrap();
    assert!(dispatch_caller(OTHER_USER, TARGET, &remote).await.is_err());
    assert_eq!(snapshot(observer).await, before);
    observer
        .execute(
            "UPDATE public.users SET deleting_at = NULL WHERE id = $1",
            &[&OTHER_USER],
        )
        .await
        .unwrap();
    let applied = dispatch_caller(OTHER_USER, TARGET, &remote)
        .await
        .expect("matching mixed-plan history hydrates");
    assert_eq!(applied, fresh, "caller must read the exact archived target");
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
    assert_eq!(paid_through_unix, Some(BOUNDARY));
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
    let replay = dispatch_caller(OTHER_USER, TARGET, &remote)
        .await
        .expect("matching mixed-plan replay");
    assert!(replay
        .history
        .iter()
        .any(|row| row.stripe_invoice_id == LEGACY));
    assert_eq!(snapshot(observer).await, committed);

    let mut renewal = remote;
    renewal.subscription.current_period_end = Some(RENEWAL_END);
    renewal.invoices.insert(
        0,
        invoice(
            RENEWAL,
            400,
            vec![line("renewal_plus", PLUS, BOUNDARY, RENEWAL_END)],
            BOUNDARY,
        ),
    );
    let mut conflicting = renewal.clone();
    conflicting
        .invoices
        .iter_mut()
        .find(|invoice| invoice.id == FRESH)
        .expect("renewal includes the previous paid invoice")
        .amount_paid += 1;
    assert!(
        dispatch_caller_at(OTHER_USER, TARGET, &conflicting, BOUNDARY)
            .await
            .is_err()
    );
    assert_eq!(snapshot(observer).await, committed);

    let renewed = dispatch_caller_at(OTHER_USER, TARGET, &renewal, BOUNDARY)
        .await
        .expect("paid renewal commits with retained invoice evidence");
    assert_eq!(renewed.read_anchor.as_deref(), Some(FRESH));
    assert_eq!(renewed.history.len(), 3);
    for invoice_id in [RENEWAL, FRESH, LEGACY] {
        assert!(renewed
            .history
            .iter()
            .any(|row| row.stripe_invoice_id == invoice_id));
    }
    let (subscriptions, history) = snapshot(observer).await;
    let target = subscriptions
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["stripe_subscription_id"] == TARGET)
        .unwrap();
    assert_eq!(target["paid_invoice_id"], RENEWAL);
    let history = history.as_array().unwrap();
    let retained = committed.1.as_array().unwrap();
    assert_eq!(history.len(), retained.len() + 1);
    assert!(retained.iter().all(|row| history.contains(row)));
    assert_eq!(
        observer
            .query_one(
                "SELECT period_start = to_timestamp($2::bigint)
                    AND period_end = to_timestamp($3::bigint)
                    AND covered_periods = tstzmultirange(tstzrange(
                        to_timestamp($2::bigint), to_timestamp($3::bigint), '[)'))
                 FROM public.paid_invoice_history WHERE stripe_invoice_id = $1",
                &[&RENEWAL, &BOUNDARY, &RENEWAL_END],
            )
            .await
            .unwrap()
            .get::<_, Option<bool>>(0),
        Some(true)
    );
    let live = with_store(CALLER_CONNECTION, 1, async |store| {
        super::Store::live_subscription(store, OTHER_USER).await
    })
    .await
    .unwrap()
    .expect("renewed subscription is live");
    assert_eq!(live.paid_through_at_unix, Some(RENEWAL_END));
    for now in [BOUNDARY, RENEWAL_END - 1] {
        let effective = crate::billing::resolve_effective_plan(Some(live.as_view()), now);
        assert!(effective.entitled);
        assert_eq!(effective.plan, crate::plan_limits::PlanId::Plus);
    }
}

async fn check_caller_lock_privilege() {
    let (mut client, connection) = tokio_postgres::connect(CALLER_CONNECTION, NoTls)
        .await
        .unwrap();
    let driver = tokio::spawn(connection);
    let tx = client.transaction().await.unwrap();
    let err = tx
        .batch_execute(crate::billing_transaction_sql::LOCK_SUBSCRIPTIONS)
        .await
        .expect_err("SELECT alone must not permit the subscription lock");
    assert_eq!(err.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));
    tx.rollback().await.unwrap();
    drop(client);
    driver.await.unwrap().unwrap();
}

async fn scheduled_reconciliation_preserves_later_plan_update(observer: &Client) {
    use crate::billing_sweep::{scheduled_hour, BATCH_SIZE, CRON, DUE_TARGETS};

    fn subscription_row(rows: &Value) -> &Value {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["stripe_subscription_id"] == SUBSCRIPTION)
            .expect("exact target exists")
    }

    const CUSTOMER: &str = "cus_native_billing";
    const OLD_INVOICE: &str = "in_native_scheduled_observation";
    const NEW_INVOICE: &str = "in_native_paid_plan_update";
    const OLD_END: i64 = NOW + 100;
    const NEW_END: i64 = NOW + 200;
    const RENEWAL_END: i64 = NOW + 300;

    observer
        .execute(
            "INSERT INTO public.billing_accounts (user_id, stripe_customer_id) VALUES ($1, $2)",
            &[&USER, &CUSTOMER],
        )
        .await
        .unwrap();
    let hour = scheduled_hour(CRON, NOW as f64 * 1_000.0).unwrap();
    let (user, target): (String, String) = observer
        .query(DUE_TARGETS, &[&hour, &BATCH_SIZE])
        .await
        .unwrap()
        .iter()
        .find_map(|row| {
            let user: &str = row.get(0);
            let subscription: Option<&str> = row.get(1);
            subscription
                .filter(|_| user == USER)
                .map(|id| (user.to_owned(), id.to_owned()))
        })
        .expect("hourly sweep selects the due account");
    assert_eq!(target, SUBSCRIPTION);

    let invoice = |id, price, amount: i64, start: i64, end: i64| {
        serde_json::from_value::<crate::stripe::Invoice>(json!({
            "id": id, "customer": CUSTOMER, "subscription": SUBSCRIPTION, "status": "paid",
            "amount_paid": amount, "currency": "usd", "status_transitions": {"paid_at": start},
            "lines": {"has_more": false, "data": [{
                "id": format!("il_{id}"), "amount": amount, "price": price, "currency": "usd",
                "period": {"start": start, "end": end}
            }]}
        }))
        .unwrap()
    };
    let old = invoice(OLD_INVOICE, PLUS, 800, NOW, OLD_END);
    let new = invoice(NEW_INVOICE, PRO, 1500, NOW + 10, NEW_END);
    let subscription = |price, status, period_end: i64, cancel_at_period_end: bool| {
        serde_json::from_value::<crate::stripe::Subscription>(json!({
            "id": SUBSCRIPTION, "customer": CUSTOMER, "status": status,
            "current_period_end": period_end, "cancel_at_period_end": cancel_at_period_end,
            "items": {"data": [{"id": "si_native_billing", "price": price}]}
        }))
        .unwrap()
    };
    let update = RemoteSubscription {
        subscription: subscription(PRO, "active", NEW_END, false),
        invoices: vec![old.clone(), new],
    };
    let renewal = RemoteSubscription {
        subscription: subscription(PRO, "past_due", RENEWAL_END, true),
        invoices: update.invoices.clone(),
    };
    let before = snapshot(observer).await;
    let mut latest = None;
    let delayed = with_store(CALLER_CONNECTION, 2, async |store| {
        store
            .reconcile_owned_subscription(
                &user,
                &target,
                subscription(PLUS, "active", OLD_END, false),
                async |_| {
                    // The hourly caller has read the target. These calls commit on
                    // separate connections before its invoice load returns.
                    let updated = dispatch_caller_at(&user, &target, &update, NOW + 10)
                        .await
                        .expect("ordinary paid plan update commits");
                    assert!(!updated.explicit_stale && updated.fail_closed.is_none());
                    assert_eq!(updated.row.plan_id, "pro");
                    assert_eq!(updated.row.paid_through_unix, Some(NEW_END));
                    assert_eq!(updated.row.paid_invoice_id.as_deref(), Some(NEW_INVOICE));
                    let after_update = snapshot(observer).await;

                    let observed = dispatch_caller_at(&user, &target, &renewal, NEW_END + 1)
                        .await
                        .expect("later renewal and cancellation observation commits");
                    assert!(!observed.explicit_stale && observed.fail_closed.is_none());
                    assert_eq!(observed.read_plan.as_deref(), Some("pro"));
                    assert_eq!(observed.read_anchor.as_deref(), Some(NEW_INVOICE));
                    assert_eq!(
                        observed.row.grace_until_unix,
                        Some(NEW_END + crate::billing_reconcile::RENEWAL_GRACE_SECS)
                    );
                    let after_renewal = snapshot(observer).await;
                    let updated = subscription_row(&after_update.0);
                    let observed = subscription_row(&after_renewal.0);
                    assert_eq!(updated["plan_id"], "pro");
                    assert_eq!(observed["plan_id"], updated["plan_id"]);
                    assert_eq!(observed["stripe_price_id"], PRO);
                    assert_eq!(observed["paid_invoice_id"], NEW_INVOICE);
                    assert_eq!(observed["paid_through_at"], updated["paid_through_at"]);
                    assert_eq!(updated["status"], "active");
                    assert_eq!(observed["status"], "past_due");
                    assert_eq!(updated["cancel_at_period_end"], false);
                    assert_eq!(observed["cancel_at_period_end"], true);
                    assert_ne!(
                        observed["current_period_end"],
                        updated["current_period_end"]
                    );
                    assert!(updated["grace_until"].is_null());
                    assert!(observed["grace_until"].is_string());
                    assert_eq!(after_update.1, after_renewal.1);
                    latest = Some(after_renewal);
                    Ok(vec![old])
                },
                NOW,
            )
            .await
    })
    .await
    .expect("stale hourly observation is accepted without a subscription update");
    assert!(!delayed.explicit_stale && delayed.fail_closed.is_none());
    assert_eq!(delayed.read_plan.as_deref(), Some("plus"));
    assert_eq!(delayed.read_anchor, None);
    assert_eq!(delayed.row.paid_through_unix, Some(OLD_END));
    assert_eq!(delayed.row.paid_invoice_id.as_deref(), Some(OLD_INVOICE));

    let latest = latest.expect("later observations committed during invoice loading");
    assert_eq!(
        snapshot(observer).await,
        latest,
        "late sweep changed committed state"
    );
    let unrelated = |rows: Value| {
        rows.as_array()
            .unwrap()
            .iter()
            .filter(|row| row["stripe_subscription_id"] != SUBSCRIPTION)
            .cloned()
            .collect::<Vec<_>>()
    };
    let unrelated_before = unrelated(before.0);
    assert!(!unrelated_before.is_empty());
    assert_eq!(unrelated(latest.0), unrelated_before);
    let history = latest.1.as_array().unwrap();
    let prior_history = before.1.as_array().unwrap();
    assert_eq!(history.len(), prior_history.len() + 2);
    assert!(prior_history.iter().all(|row| history.contains(row)));
    for (invoice, plan) in [(OLD_INVOICE, "plus"), (NEW_INVOICE, "pro")] {
        let row = history
            .iter()
            .find(|row| row["stripe_invoice_id"] == invoice)
            .unwrap();
        assert_eq!(row["user_id"], USER);
        assert_eq!(row["stripe_subscription_id"], SUBSCRIPTION);
        assert_eq!(row["plan_id"], plan);
    }

    const EQUAL_END_ANCHOR: &str = "in_native_equal_end_anchor";
    const DELAYED_HISTORY: &str = "in_native_delayed_history";
    let mut newer = renewal;
    newer
        .invoices
        .push(invoice(EQUAL_END_ANCHOR, PRO, 1500, NOW + 20, NEW_END));
    let mut delayed_invoices = update.invoices;
    delayed_invoices.push(invoice(DELAYED_HISTORY, PRO, 1500, NOW + 5, NEW_END - 1));
    let mut older = subscription(PRO, "active", NEW_END, false);
    older.items.as_mut().unwrap().data[0].id = "si_native_before_equal_end".into();
    let mut committed = None;
    let delayed = with_store(CALLER_CONNECTION, 2, async |store| {
        store
            .reconcile_owned_subscription(
                &user,
                &target,
                older,
                async |_| {
                    let updated = dispatch_caller_at(&user, &target, &newer, NEW_END + 1)
                        .await
                        .expect("fresh equal-end observation commits");
                    assert_eq!(
                        updated.row.paid_invoice_id.as_deref(),
                        Some(EQUAL_END_ANCHOR)
                    );
                    let state = snapshot(observer).await;
                    let row = subscription_row(&state.0);
                    assert_eq!(row["paid_invoice_id"], EQUAL_END_ANCHOR);
                    assert_eq!(row["status"], "past_due");
                    assert_eq!(row["cancel_at_period_end"], true);
                    assert!(row["grace_until"].is_string());
                    committed = Some(state);
                    Ok(delayed_invoices)
                },
                NOW,
            )
            .await
    })
    .await
    .expect("delayed equal-end observation is accepted for history only");
    assert!(!delayed.explicit_stale && delayed.fail_closed.is_none());
    assert_eq!(delayed.read_anchor.as_deref(), Some(NEW_INVOICE));
    assert_eq!(delayed.row.paid_through_unix, Some(NEW_END));
    let (rows, history) = snapshot(observer).await;
    let (newer_rows, newer_history) = committed.expect("newer equal-end write committed");
    assert_eq!(
        rows, newer_rows,
        "delayed equal-end write changed subscription state"
    );
    let history = history.as_array().unwrap();
    let newer_history = newer_history.as_array().unwrap();
    assert_eq!(history.len(), newer_history.len() + 1);
    assert!(newer_history.iter().all(|row| history.contains(row)));
    assert!(history
        .iter()
        .any(|row| row["stripe_invoice_id"] == DELAYED_HISTORY
            && row["stripe_subscription_id"] == SUBSCRIPTION
            && row["user_id"] == USER));

    const RENEWAL_ANCHOR: &str = "in_native_renewal_with_cancel_race";
    let mut renewed = newer;
    renewed.subscription = subscription(PRO, "active", RENEWAL_END, false);
    renewed
        .invoices
        .push(invoice(RENEWAL_ANCHOR, PRO, 1500, NEW_END, RENEWAL_END));
    let mut cancel = renewed.clone();
    cancel.subscription.cancel_at_period_end = true;
    let mut committed = None;
    let overlap = with_store(CALLER_CONNECTION, 2, async |store| {
        store
            .reconcile_owned_subscription(
                &user,
                &target,
                cancel.subscription.clone(),
                async |_| {
                    let unmodified = dispatch_caller_at(&user, &target, &renewed, NEW_END)
                        .await
                        .expect("renewal commits before the cancellation write");
                    assert_eq!(unmodified.read_anchor.as_deref(), Some(EQUAL_END_ANCHOR));
                    assert_eq!(
                        unmodified.row.paid_invoice_id.as_deref(),
                        Some(RENEWAL_ANCHOR)
                    );
                    let after_renewal = snapshot(observer).await;
                    let row = subscription_row(&after_renewal.0);
                    assert_eq!(row["paid_invoice_id"], RENEWAL_ANCHOR);
                    assert_eq!(row["cancel_at_period_end"], false);
                    // Matching proof and an unchanged flag is an exact replay.
                    dispatch(&user, &unmodified)
                        .await
                        .expect("renewal replay succeeds");
                    assert_eq!(snapshot(observer).await, after_renewal);
                    committed = Some(after_renewal);
                    Ok(cancel.invoices.clone())
                },
                NEW_END,
            )
            .await
    })
    .await;
    assert!(overlap.is_err(), "overlapping cancellation must be retried");
    let committed = committed.expect("renewal committed during the cancellation fold");
    assert_eq!(snapshot(observer).await, committed);
    let fresh = dispatch_caller_at(&user, &target, &cancel, NEW_END)
        .await
        .expect("fresh cancellation commits");
    assert_eq!(fresh.read_anchor.as_deref(), Some(RENEWAL_ANCHOR));
    let after_cancel = snapshot(observer).await;
    assert_eq!(
        subscription_row(&after_cancel.0)["cancel_at_period_end"],
        true
    );
    assert_eq!(after_cancel.1, committed.1);
}

async fn entitlement_of(user: &str, at: i64) -> crate::billing::EffectivePlan {
    let live = with_store(CALLER_CONNECTION, 1, async |store| {
        super::Store::live_subscription(store, user).await
    })
    .await
    .unwrap();
    crate::billing::resolve_effective_plan(live.as_ref().map(|row| row.as_view()), at)
}

async fn assert_no_billing_locks(observer: &Client, users: &[&str]) {
    let sql = "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_locks
        WHERE database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())
          AND pid <> pg_backend_pid()
          AND (relation IN ('public.subscriptions'::regclass, 'public.paid_invoice_history'::regclass, 'public.users'::regclass) OR locktype = 'advisory'))";
    let clear: bool = observer.query_one(sql, &[]).await.unwrap().get(0);
    assert!(clear, "billing locks remained held after transaction end");
    for user in users {
        let q = "SELECT pg_try_advisory_xact_lock(hashtext($1))";
        let free: bool = observer.query_one(q, &[user]).await.unwrap().get(0);
        assert!(free, "per-user advisory lock remained held for {user}");
    }
}

async fn poll_lock_pid(observer: &Client, sql: &str, blocker: Option<i32>) -> Result<i32, ()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let row = match blocker {
                Some(pid) => observer.query_opt(sql, &[&pid]).await.unwrap(),
                None => observer.query_opt(sql, &[]).await.unwrap(),
            };
            if let Some(row) = row {
                return row.get::<_, i32>(0);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| ())
}

fn assert_snapshot_added_one(
    before: &(Value, Value),
    after: &(Value, Value),
    (owner, sub, inv): (&str, &str, &str),
    (gone_sub, gone_inv): (Option<&str>, &str),
) {
    let (bs, bh) = (before.0.as_array().unwrap(), before.1.as_array().unwrap());
    let (subs, hist) = (after.0.as_array().unwrap(), after.1.as_array().unwrap());
    assert_eq!((subs.len(), hist.len()), (bs.len() + 1, bh.len() + 1));
    assert!(bs.iter().all(|r| subs.contains(r)) && bh.iter().all(|r| hist.contains(r)));
    let has_sub = |r: &Value| r["stripe_subscription_id"] == sub && r["user_id"] == owner;
    assert!(subs
        .iter()
        .any(|r| has_sub(r) && r["paid_invoice_id"] == inv));
    assert!(gone_sub.is_none_or(|g| !subs.iter().any(|r| r["stripe_subscription_id"] == g)));
    assert!(hist
        .iter()
        .any(|r| has_sub(r) && r["stripe_invoice_id"] == inv));
    assert!(!hist.iter().any(|r| r["stripe_invoice_id"] == gone_inv));
}

const COLLISION_GATE_KEY: i64 = 781_349_099;

async fn run_gated_collision(
    observer: &Client,
    winner: (&str, &ReconcileOutcome),
    loser: (&str, &ReconcileOutcome),
    table_lock: bool,
    during_check: (&str, i64, crate::billing::EffectivePlan),
) -> (Result<(), StoreError>, Result<(), StoreError>) {
    let q_lock = "SELECT pg_try_advisory_lock($1)";
    let q_unlock = "SELECT pg_advisory_unlock($1)";
    assert!(observer
        .query_one(q_lock, &[&COLLISION_GATE_KEY])
        .await
        .unwrap()
        .get::<_, bool>(0));
    let mut unlocked = false;
    let outcome = async {
        let winner_fut = dispatch(winner.0, winner.1);
        tokio::pin!(winner_fut);
        let gate_sql = "SELECT l.pid FROM pg_catalog.pg_locks l JOIN pg_catalog.pg_locks w ON w.pid = l.pid
            WHERE l.database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())
              AND l.relation = 'public.subscriptions'::regclass AND l.mode = 'ShareRowExclusiveLock' AND l.granted
              AND w.locktype = 'advisory' AND NOT w.granted AND pg_backend_pid() = ANY(pg_blocking_pids(l.pid))";
        let winner_pid = tokio::select! {
            res = &mut winner_fut => return Err(format!("winner finished before gate: {res:?}")),
            pid = poll_lock_pid(observer, gate_sql, None) => pid.map_err(|_| "winner never reached gate".to_string())?,
        };
        let loser_fut = dispatch(loser.0, loser.1);
        tokio::pin!(loser_fut);
        let pred = if table_lock {
            "l.relation = 'public.subscriptions'::regclass AND l.mode = 'ShareRowExclusiveLock'"
        } else {
            "l.locktype = 'advisory'"
        };
        let wait_sql = format!(
            "SELECT l.pid FROM pg_catalog.pg_locks l WHERE l.database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database()) AND {pred} AND NOT l.granted AND $1 = ANY(pg_blocking_pids(l.pid))"
        );
        let loser_pid = tokio::select! {
            res = &mut winner_fut => return Err(format!("winner finished early: {res:?}")),
            res = &mut loser_fut => return Err(format!("contender finished before blocking: {res:?}")),
            pid = poll_lock_pid(observer, &wait_sql, Some(winner_pid)) => pid.map_err(|_| "contender never blocked".to_string())?,
        };
        if loser_pid == winner_pid {
            return Err("contender and winner shared backend pid".to_string());
        }
        let during = entitlement_of(during_check.0, during_check.1).await;
        if during != during_check.2 {
            return Err(format!("entitlement changed before commit: {during:?}"));
        }
        unlocked = observer.query_one(q_unlock, &[&COLLISION_GATE_KEY]).await.unwrap().get(0);
        if !unlocked {
            return Err("failed to unlock collision gate".to_string());
        }
        Ok(tokio::join!(winner_fut, loser_fut))
    }
    .await;
    if !unlocked {
        let _ = observer.query_one(q_unlock, &[&COLLISION_GATE_KEY]).await;
    }
    outcome.expect("gated collision phases must complete cleanly")
}

async fn concurrent_collision_and_target_identity_preserve_entitlement_and_release_locks(
    observer: &Client,
) {
    use crate::billing::EffectivePlan;
    use crate::plan_limits::PlanId;

    const WINNER: &str = "native_billing_race_winner";
    const LOSER: &str = "native_billing_race_loser";
    const SLOT_A: &str = "sub_native_slot_race_a";
    let mk = |sub: &str, pro: bool, inv: &str, end: i64| {
        let (price, plan, amt) = if pro {
            (PRO, "pro", 1500)
        } else {
            (PLUS, "plus", 800)
        };
        ReconcileOutcome {
            row: ReconciledRow {
                stripe_subscription_id: sub.into(),
                stripe_customer_id: format!("cus_{inv}"),
                stripe_item_id: format!("si_{sub}"),
                stripe_price_id: price.into(),
                plan_id: plan.into(),
                status: "active".into(),
                current_period_end_unix: Some(end),
                cancel_at_period_end: false,
                paid_through_unix: Some(end),
                paid_invoice_id: Some(inv.into()),
                grace_until_unix: None,
            },
            history: vec![PaidInvoiceRow {
                stripe_invoice_id: inv.into(),
                stripe_subscription_id: sub.into(),
                plan_id: plan.into(),
                period_start_unix: NOW,
                period_end_unix: end,
                amount_cents: amt,
                currency: "usd".into(),
                paid_at_unix: NOW,
                covered_periods: vec![(NOW, end)],
            }],
            fail_closed: None,
            explicit_stale: false,
            read_plan: None,
            read_anchor: None,
        }
    };
    let ep = |plan, entitled| EffectivePlan { plan, entitled };
    let (pro, plus, closed) = (
        ep(PlanId::Pro, true),
        ep(PlanId::Plus, true),
        ep(PlanId::Free, false),
    );
    let check_base = async || {
        assert_eq!(entitlement_of(USER, NOW + 299).await, pro);
        assert_eq!(entitlement_of(USER, NOW + 300).await, closed);
        assert_eq!(entitlement_of(OTHER_USER, NOW + 60).await, plus);
    };
    check_base().await;
    let before = snapshot(observer).await;

    // 1. Conflicting target identities against existing entitled accounts.
    for (sub, is_pro, inv) in [
        (
            "sub_native_slot_collision",
            false,
            "in_native_slot_collision",
        ),
        ("sub_native_mixed_plan", true, "in_native_foreign_target"),
    ] {
        assert!(dispatch(USER, &mk(sub, is_pro, inv, NOW + 600))
            .await
            .is_err());
        assert_eq!(snapshot(observer).await, before);
        check_base().await;
        assert_no_billing_locks(observer, &[USER, OTHER_USER]).await;
    }

    let mut bad_hist = mk(SUBSCRIPTION, true, "in_native_wrong_history", NOW + 600);
    bad_hist.read_plan = Some("pro".into());
    bad_hist.read_anchor = Some("in_native_renewal_with_cancel_race".into());
    bad_hist.history[0].stripe_subscription_id = "sub_native_wrong_history".into();
    let pre_err = with_store(CONNECTION, 0, async |store| {
        super::Store::store_paid_reconcile_outcome(store, USER, &bad_hist).await
    })
    .await;
    assert!(pre_err.is_err());
    let (mut c, conn) = tokio_postgres::connect(CONNECTION, NoTls).await.unwrap();
    let driver = tokio::spawn(conn);
    super::billing_budget::bootstrap_transaction(&mut c)
        .await
        .unwrap();
    c.batch_execute("SELECT pg_current_xact_id()")
        .await
        .unwrap();
    let tx_err = crate::billing_transaction_sql::apply_reconcile_transaction(
        &mut c,
        USER.into(),
        bad_hist,
        true,
    )
    .await;
    assert!(tx_err.is_err());
    let xid_sql = "SELECT pg_current_xact_id_if_assigned() IS NULL";
    assert!(c.query_one(xid_sql, &[]).await.unwrap().get::<_, bool>(0));
    drop(c);
    driver.await.unwrap().unwrap();
    assert_eq!(snapshot(observer).await, before);
    check_base().await;
    assert_no_billing_locks(observer, &[USER, OTHER_USER]).await;

    // 2. Concurrent cross-user global target-identity and same-user live-slot collisions.
    observer.execute(
        "INSERT INTO public.users (id, email, created_at) VALUES ($1, 'race-winner@example.test', $3), ($2, 'race-loser@example.test', $3)",
        &[&WINNER, &LOSER, &NOW],
    ).await.unwrap();
    observer.batch_execute(
        "CREATE FUNCTION public.native_billing_collision_gate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(781349099); RETURN NEW; END $$;
         CREATE TRIGGER native_billing_collision_gate BEFORE INSERT ON public.paid_invoice_history FOR EACH ROW
         WHEN (NEW.stripe_invoice_id IN ('in_native_concurrent_winner', 'in_native_slot_race_a'))
         EXECUTE FUNCTION public.native_billing_collision_gate();",
    ).await.unwrap();
    let all = [USER, OTHER_USER, WINNER, LOSER];
    let mut prev = before;
    let races = [
        (
            WINNER,
            LOSER,
            "sub_native_concurrent_global",
            "sub_native_concurrent_global",
            "in_native_concurrent_winner",
            "in_native_concurrent_loser",
            true,
            (USER, NOW + 299, pro),
            (plus, closed),
        ),
        (
            LOSER,
            LOSER,
            SLOT_A,
            "sub_native_slot_race_b",
            "in_native_slot_race_a",
            "in_native_slot_race_b",
            false,
            (LOSER, NOW + 499, closed),
            (plus, plus),
        ),
    ];
    for (w_u, l_u, w_s, l_s, w_i, l_i, tlock, during, expected) in races {
        let (w_out, l_out) = (
            mk(w_s, false, w_i, NOW + 500),
            mk(l_s, true, l_i, NOW + 600),
        );
        let (w_res, l_res) =
            run_gated_collision(observer, (w_u, &w_out), (l_u, &l_out), tlock, during).await;
        w_res.expect("first concurrent claimant commits");
        assert!(l_res.is_err(), "concurrent contender must roll back");
        let after = snapshot(observer).await;
        assert_snapshot_added_one(
            &prev,
            &after,
            (w_u, w_s, w_i),
            ((l_s != w_s).then_some(l_s), l_i),
        );
        check_base().await;
        assert_eq!(
            (
                entitlement_of(WINNER, NOW + 499).await,
                entitlement_of(LOSER, NOW + 499).await
            ),
            expected
        );
        assert_no_billing_locks(observer, &all).await;
        prev = after;
    }
    let drop_sql = "DROP TRIGGER IF EXISTS native_billing_collision_gate ON public.paid_invoice_history; DROP FUNCTION IF EXISTS public.native_billing_collision_gate();";
    observer.batch_execute(drop_sql).await.unwrap();

    // 3. Follow-up fold succeeds immediately, proving no pending fold or lock lingers.
    let mut follow_up = mk(SLOT_A, false, "in_native_slot_race_a_2", NOW + 900);
    follow_up.read_plan = Some("plus".into());
    follow_up.read_anchor = Some("in_native_slot_race_a".into());
    dispatch(LOSER, &follow_up)
        .await
        .expect("follow-up fold commits cleanly");
    assert_eq!(entitlement_of(LOSER, NOW + 899).await, plus);
    assert_no_billing_locks(observer, &all).await;
}
