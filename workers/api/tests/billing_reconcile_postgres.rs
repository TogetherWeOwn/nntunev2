//! Baseline reconciliation selection controls on disposable PostgreSQL.
#![cfg(feature = "postgres-tests")]

use nntune_api::billing_reconcile::{
    insert_paid_invoice_history_statement, reconcile_live_select_statement,
    reconcile_lock_statement, reconcile_subscription, reconcile_target_select_statement,
    store_reconciled_subscription_statement, LiveRow, PriceBinding, ReconcileInput,
    ReconcileOutcome, RemoteSubscription,
};
use nntune_api::stripe::{
    Invoice, InvoiceLine, InvoiceLinePeriod, InvoiceLines, InvoiceStatusTransitions, Subscription,
    SubscriptionItem, SubscriptionItems,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_postgres::types::{ToSql, Type};
use tokio_postgres::{Client, Config, NoTls};

#[path = "support/billing_contention.rs"]
mod billing_contention;
#[path = "support/billing_transaction_fixture.rs"]
mod billing_transaction_fixture;

static SCHEMA: AtomicUsize = AtomicUsize::new(0);
const T: i64 = 1_786_032_000;
const P: i64 = 30 * 24 * 60 * 60;
const CUS: &str = "cus_a";
const PRICE: &str = "price_plus_test";

fn disposable_config(url: &str) -> Result<Config, &'static str> {
    let config: Config = url.parse().map_err(|_| "invalid test database URL")?;
    if config.get_dbname() != Some("nntune_ci") {
        return Err("test database must be disposable");
    }
    if config.get_hosts().is_empty()
        || !config.get_hosts().iter().all(|host| match host {
            tokio_postgres::config::Host::Tcp(host) => {
                host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            }
            _ => false,
        })
        || !config.get_hostaddrs().iter().all(|addr| addr.is_loopback())
    {
        return Err("test database hosts and addresses must be local");
    }
    Ok(config)
}

#[test]
fn disposable_config_rejects_non_local_targets() {
    let base = "postgresql://postgres:throwaway@localhost:5432/nntune_ci";
    assert!(disposable_config(base).is_ok());
    for url in [
        format!("{base}?hostaddr=203.0.113.10"),
        format!("{base}?host=remote.example.com"),
        format!("{base}?dbname=application"),
    ] {
        assert!(disposable_config(&url).is_err(), "must refuse: {url}");
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snap {
    user_id: String,
    customer: String,
    item: String,
    price: String,
    plan: String,
    status: String,
    period_end: Option<i64>,
    cancel: bool,
    paid_through: Option<i64>,
    anchor: Option<String>,
    grace: Option<i64>,
}

struct Fx {
    db: Client,
    schema: String,
}

impl Fx {
    async fn new() -> Self {
        let url = std::env::var("NNTUNE_TEST_DATABASE_URL")
            .expect("NNTUNE_TEST_DATABASE_URL must name CI's disposable database");
        let config = disposable_config(&url).expect("scratch database configuration");
        let schema = format!(
            "billing_test_{}_{}",
            std::process::id(),
            SCHEMA.fetch_add(1, Ordering::Relaxed)
        );
        let (db, connection) = config.connect(NoTls).await.expect("connect scratch DB");
        tokio::spawn(async move { connection.await.expect("scratch connection driver") });
        db.batch_execute(&format!(
            "CREATE SCHEMA {schema}; SET search_path TO {schema}"
        ))
        .await
        .unwrap();
        for migration in [
            include_str!("../../../migrations/0002_auth_vault.sql"),
            include_str!("../../../migrations/0021_billing_schema.sql"),
            include_str!("../../../migrations/0026_plan_reseed_no_trial.sql"),
            include_str!("../../../migrations/0029_paid_invoice_history.sql"),
        ] {
            db.batch_execute(migration).await.unwrap();
        }
        Self { db, schema }
    }

    async fn add_user(&self, id: &str) {
        self.db
            .execute(
                "INSERT INTO users (id,email,created_at) VALUES ($1,$2,$3)",
                &[&id, &format!("{id}@example.test"), &T],
            )
            .await
            .unwrap();
    }

    async fn raw(&self, user: &str, sub: &str, status: &str, paid_through: Option<i64>) {
        self.db
            .execute(
                "INSERT INTO subscriptions (user_id, stripe_subscription_id, stripe_customer_id, stripe_subscription_item_id, stripe_price_id, plan_id, status, paid_through_at, paid_invoice_id) VALUES ($1,$2,$3,'si_raw',$4,'plus',$5,to_timestamp($6::bigint),$7)",
                &[&user, &sub, &CUS, &PRICE, &status, &paid_through,
                    &paid_through.map(|_| format!("inv_raw_{sub}"))],
            )
            .await
            .unwrap();
    }

    async fn snap(&self, sub: &str) -> Option<Snap> {
        self.db
            .query_opt(
                "SELECT user_id, stripe_customer_id, stripe_subscription_item_id, stripe_price_id, plan_id, status, EXTRACT(EPOCH FROM current_period_end)::bigint, cancel_at_period_end, EXTRACT(EPOCH FROM paid_through_at)::bigint, paid_invoice_id, EXTRACT(EPOCH FROM grace_until)::bigint FROM subscriptions WHERE stripe_subscription_id = $1",
                &[&sub],
            )
            .await
            .unwrap()
            .map(|r| Snap {
                user_id: r.get(0), customer: r.get(1), item: r.get(2),
                price: r.get(3), plan: r.get(4), status: r.get(5),
                period_end: r.get(6), cancel: r.get(7), paid_through: r.get(8),
                anchor: r.get(9), grace: r.get(10),
            })
    }

    async fn rows(&self, user: &str) -> i64 {
        self.db
            .query_one(
                "SELECT count(*) FROM subscriptions WHERE user_id = $1",
                &[&user],
            )
            .await
            .unwrap()
            .get(0)
    }

    async fn live(&self, user: &str) -> Option<LiveRow> {
        self.select(user, None).await
    }

    async fn select(&self, user: &str, target: Option<&str>) -> Option<LiveRow> {
        let row = match target {
            Some(target) => {
                self.db
                    .query_opt(reconcile_target_select_statement(), &[&user, &target])
                    .await
            }
            None => {
                self.db
                    .query_opt(reconcile_live_select_statement(), &[&user])
                    .await
            }
        };
        row.unwrap().map(|r| LiveRow {
            stripe_subscription_id: r.get("stripe_subscription_id"),
            stripe_customer_id: r.get("stripe_customer_id"),
            stripe_item_id: r.get("stripe_subscription_item_id"),
            stripe_price_id: r.get("stripe_price_id"),
            plan_id: r.get("plan_id"),
            status: r.get("status"),
            paid_through_unix: r.get("paid_through_unix"),
            paid_invoice_id: r.get("paid_invoice_id"),
            grace_until_unix: r.get("grace_until_unix"),
        })
    }

    async fn history_rows(&self, sub: &str) -> i64 {
        self.db
            .query_one(
                "SELECT count(*) FROM paid_invoice_history WHERE stripe_subscription_id = $1",
                &[&sub],
            )
            .await
            .unwrap()
            .get(0)
    }

    async fn fold(&self, user: &str, remote: &RemoteSubscription) -> ReconcileOutcome {
        let input = ReconcileInput {
            stripe_customer_id: Some(CUS.into()),
            live: self.live(user).await,
            bindings: bindings(),
        };
        // Observe after the second invoice is paid, before its period ends.
        reconcile_subscription(&input, remote, T + P + 1)
    }

    async fn try_put(
        &mut self,
        user: &str,
        outcome: &ReconcileOutcome,
    ) -> Result<(), tokio_postgres::Error> {
        let tx = self.db.transaction().await?;
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
                ],
            )
            .await?;
        for h in &outcome.history {
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
            .await?;
        }
        tx.commit().await
    }

    async fn put(&mut self, user: &str, outcome: &ReconcileOutcome) {
        self.try_put(user, outcome)
            .await
            .expect("reconcile write must commit");
    }

    async fn seed_active(&mut self, user: &str, sub: &str) -> Snap {
        let remote = remote(sub, &[invoice(sub, 1), invoice(sub, 2)]);
        let outcome = self.fold(user, &remote).await;
        self.put(user, &outcome).await;
        let snap = self.snap(sub).await.expect("seeded");
        assert_eq!(snap.status, "active");
        assert_eq!(snap.plan, "plus");
        assert_eq!(snap.paid_through, Some(T + 2 * P));
        assert_eq!(
            snap.anchor.as_deref(),
            Some(format!("inv_{sub}_2").as_str())
        );
        snap
    }

    async fn cleanup(&self) {
        self.db
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .await
            .unwrap();
    }
}

fn bindings() -> Vec<PriceBinding> {
    vec![PriceBinding {
        price_id: PRICE.into(),
        plan_id: "plus".into(),
        selectable: true,
        livemode: false,
    }]
}

fn invoice(sub: &str, n: i64) -> Invoice {
    let (start, end) = (T + (n - 1) * P, T + n * P);
    Invoice {
        id: format!("inv_{sub}_{n}"),
        customer: Some(CUS.into()),
        subscription: Some(sub.into()),
        status: "paid".into(),
        amount_paid: 800,
        currency: Some("usd".into()),
        status_transitions: Some(InvoiceStatusTransitions {
            paid_at: Some(start),
        }),
        lines: Some(InvoiceLines {
            data: vec![InvoiceLine {
                id: format!("inv_{sub}_{n}_li"),
                amount: 800,
                currency: Some("usd".into()),
                price: Some(PRICE.into()),
                period: Some(InvoiceLinePeriod {
                    start: Some(start),
                    end: Some(end),
                }),
            }],
            has_more: false,
        }),
        livemode: false,
    }
}

fn remote(sub: &str, invoices: &[Invoice]) -> RemoteSubscription {
    RemoteSubscription {
        subscription: Subscription {
            id: sub.into(),
            customer: Some(CUS.into()),
            status: "active".into(),
            cancel_at_period_end: false,
            current_period_end: Some(T + 2 * P),
            items: Some(SubscriptionItems {
                data: vec![SubscriptionItem {
                    id: "si_a".into(),
                    price: Some(PRICE.into()),
                }],
            }),
            livemode: false,
        },
        invoices: invoices.to_vec(),
    }
}

#[tokio::test]
async fn equal_end_selection_persists_and_allows_fresh_update() {
    let mut fx = Fx::new().await;
    for (case, tied_payment) in [("paid", false), ("id", true)] {
        for reverse in [false, true] {
            let user = format!("u_{case}_{reverse}");
            let sub = format!("sub_{case}_{reverse}");
            fx.add_user(&user).await;
            let seed = invoice(&sub, 1);
            let initial = fx.fold(&user, &remote(&sub, &[seed.clone()])).await;
            fx.put(&user, &initial).await;

            let mut older = invoice(&sub, 2);
            older.id = format!("inv_{sub}_z");
            let mut newer = older.clone();
            newer.id = format!("inv_{sub}_a");
            if !tied_payment {
                newer.status_transitions.as_mut().unwrap().paid_at = Some(T + P + 1);
            }
            let expected = if tied_payment { &older.id } else { &newer.id };
            let mut candidates = vec![older.clone(), newer.clone()];
            if reverse {
                candidates.reverse();
            }
            let mut invoices = vec![seed];
            invoices.extend(candidates);
            let snapshot = remote(&sub, &invoices);
            let outcome = fx.fold(&user, &snapshot).await;
            assert_eq!(outcome.row.paid_invoice_id.as_ref(), Some(expected));
            fx.put(&user, &outcome).await;
            let stored = fx.snap(&sub).await.unwrap();
            assert_eq!(stored.anchor.as_ref(), Some(expected));
            assert_eq!(stored.paid_through, Some(T + 2 * P));
            assert_eq!(fx.history_rows(&sub).await, 3);

            let mut fresh = snapshot;
            fresh.subscription.status = "past_due".into();
            let outcome = fx.fold(&user, &fresh).await;
            fx.put(&user, &outcome).await;
            let updated = fx.snap(&sub).await.unwrap();
            assert_eq!(updated.status, "past_due");
            assert_eq!(updated.anchor.as_ref(), Some(expected));
            assert_eq!(updated.paid_through, Some(T + 2 * P));
            assert_eq!(fx.history_rows(&sub).await, 3);
        }
    }
    fx.cleanup().await;
}

#[tokio::test]
async fn first_insert_and_replay_preserve_subscription_and_history() {
    let mut fx = Fx::new().await;
    fx.add_user("u1").await;
    let seeded = fx.seed_active("u1", "sub_a").await;
    assert_eq!(fx.rows("u1").await, 1);
    assert_eq!(fx.history_rows("sub_a").await, 2);
    let remote = remote("sub_a", &[invoice("sub_a", 1), invoice("sub_a", 2)]);
    let replay = fx.fold("u1", &remote).await;
    assert_eq!(replay.history.len(), 2);
    fx.put("u1", &replay).await;
    assert_eq!(fx.snap("sub_a").await, Some(seeded));
    assert_eq!(fx.rows("u1").await, 1);
    assert_eq!(fx.history_rows("sub_a").await, 2);
    fx.cleanup().await;
}

#[tokio::test]
async fn fresh_same_subscription_controls_still_apply() {
    let mut fx = Fx::new().await;
    fx.add_user("u1").await;
    fx.raw("u1", "sub_old", "canceled", Some(T)).await;
    fx.seed_active("u1", "sub_a").await;

    let mut renewal_remote = remote(
        "sub_a",
        &[
            invoice("sub_a", 1),
            invoice("sub_a", 2),
            invoice("sub_a", 3),
        ],
    );
    renewal_remote.subscription.current_period_end = Some(T + 3 * P);
    let renewal = fx.fold("u1", &renewal_remote).await;
    fx.put("u1", &renewal).await;
    let s = fx.snap("sub_a").await.unwrap();
    assert_eq!(s.paid_through, Some(T + 3 * P));
    assert_eq!(s.anchor.as_deref(), Some("inv_sub_a_3"));
    assert_eq!((s.status.as_str(), s.grace), ("active", None));
    assert_eq!(fx.history_rows("sub_a").await, 3);

    let mut held_remote = renewal_remote.clone();
    held_remote.subscription.status = "paused".into();
    held_remote.subscription.cancel_at_period_end = true;
    held_remote.subscription.current_period_end = Some(T + 4 * P);
    let held = fx.fold("u1", &held_remote).await;
    fx.put("u1", &held).await;
    let h = fx.snap("sub_a").await.unwrap();
    assert_eq!(h.status, "paused");
    assert!(h.cancel);
    assert_eq!(h.period_end, Some(T + 4 * P));
    assert_eq!(
        (h.paid_through, h.anchor),
        (s.paid_through, s.anchor.clone())
    );

    held_remote.subscription.status = "canceled".into();
    held_remote.subscription.cancel_at_period_end = false;
    let terminal = fx.fold("u1", &held_remote).await;
    fx.put("u1", &terminal).await;
    let t = fx.snap("sub_a").await.unwrap();
    assert_eq!((t.status.as_str(), t.grace), ("canceled", None));
    assert_eq!(t.paid_through, s.paid_through);
    assert!(fx.live("u1").await.is_none());
    fx.cleanup().await;
}

async fn divergent(fx: &Fx, sub: &str, stored: &Snap) -> ReconcileOutcome {
    let mut o = fx
        .fold("u1", &remote(sub, &[invoice(sub, 1), invoice(sub, 2)]))
        .await;
    o.row.stripe_customer_id = "cus_other".into();
    o.row.stripe_item_id = "si_other".into();
    o.row.stripe_price_id = "price_pro_test".into();
    o.row.plan_id = "pro".into();
    o.row.status = "past_due".into();
    o.row.current_period_end_unix = Some(T + 9);
    o.row.cancel_at_period_end = !stored.cancel;
    o.row.grace_until_unix = Some(T + 99);
    o.read_plan = Some(stored.plan.clone());
    o.read_anchor = stored.anchor.clone();
    o
}

#[tokio::test]
async fn null_evidence_updates_status_and_bindings_but_never_coverage() {
    let mut fx = Fx::new().await;
    fx.add_user("u1").await;
    let stored = fx.seed_active("u1", "sub_a").await;
    let mut o = divergent(&fx, "sub_a", &stored).await;
    o.row.paid_through_unix = None;
    o.row.paid_invoice_id = None;
    fx.put("u1", &o).await;
    let s = fx.snap("sub_a").await.unwrap();
    assert_eq!(s.status, "past_due");
    assert_eq!(s.customer, "cus_other");
    assert_eq!(s.item, "si_other");
    assert_eq!(s.price, "price_pro_test");
    assert_eq!(s.period_end, Some(T + 9));
    assert_eq!(s.cancel, !stored.cancel);
    assert_eq!(s.grace, Some(T + 99));
    assert_eq!(s.plan, stored.plan);
    assert_eq!(s.paid_through, stored.paid_through);
    assert_eq!(s.anchor, stored.anchor);
    fx.cleanup().await;
}

#[tokio::test]
async fn predicate_stale_without_the_flag_keeps_bindings_and_mutable_fields() {
    let mut fx = Fx::new().await;
    fx.add_user("u1").await;
    let stored = fx.seed_active("u1", "sub_a").await;
    for (end, anchor) in [(T + P, Some("inv_old")), (T + 2 * P, None)] {
        let mut stale = divergent(&fx, "sub_a", &stored).await;
        stale.row.paid_through_unix = Some(end);
        stale.row.paid_invoice_id = anchor.map(str::to_string);
        assert!(!stale.explicit_stale);
        fx.put("u1", &stale).await;
        assert_eq!(fx.snap("sub_a").await, Some(stored.clone()), "end {end}");
    }
    assert_eq!(fx.history_rows("sub_a").await, 2);
    fx.cleanup().await;
}

#[tokio::test]
async fn cancellation_follows_the_fresh_pass_and_binding_is_stable_under_stale() {
    let mut fx = Fx::new().await;
    fx.add_user("u1").await;
    let stored = fx.seed_active("u1", "sub_a").await;
    let mut remote = remote("sub_a", &[invoice("sub_a", 1), invoice("sub_a", 2)]);
    remote.subscription.cancel_at_period_end = true;
    let cancel = fx.fold("u1", &remote).await;
    assert!(!cancel.explicit_stale);
    fx.put("u1", &cancel).await;
    let mut expected = stored.clone();
    expected.cancel = true;
    assert_eq!(fx.snap("sub_a").await, Some(expected.clone()));

    let mut stale = divergent(&fx, "sub_a", &expected).await;
    stale.row.paid_through_unix = Some(T + P);
    assert!(!stale.explicit_stale);
    fx.put("u1", &stale).await;
    assert_eq!(fx.snap("sub_a").await, Some(expected.clone()));

    remote.subscription.cancel_at_period_end = false;
    let clear = fx.fold("u1", &remote).await;
    assert!(!clear.explicit_stale);
    fx.put("u1", &clear).await;
    expected.cancel = false;
    assert_eq!(fx.snap("sub_a").await, Some(expected));
    assert_eq!(fx.history_rows("sub_a").await, 2);
    fx.cleanup().await;
}

#[tokio::test]
async fn explicit_stale_suppresses_every_update_whatever_the_evidence_end() {
    let mut fx = Fx::new().await;
    fx.add_user("u1").await;
    let stored = fx.seed_active("u1", "sub_a").await;
    let mut stale = fx
        .fold("u1", &remote("sub_a", &[invoice("sub_a", 1)]))
        .await;
    assert!(stale.explicit_stale);
    let mut history = stale.history[0].clone();
    history.stripe_invoice_id = "inv_new_history".into();
    stale.history.push(history);
    let different = divergent(&fx, "sub_a", &stored).await;
    stale.row = different.row;
    for end in [Some(T), stored.paid_through, Some(T + 4 * P), None] {
        stale.row.paid_through_unix = end;
        fx.put("u1", &stale).await;
        assert_eq!(fx.snap("sub_a").await, Some(stored.clone()), "{end:?}");
        assert_eq!(fx.history_rows("sub_a").await, 3);
    }
    // Suppression also prevents an insert when the target does not exist.
    stale.row.stripe_subscription_id = "sub_missing".into();
    stale.history.clear();
    fx.put("u1", &stale).await;
    assert!(fx.snap("sub_missing").await.is_none());
    fx.cleanup().await;
}

#[tokio::test]
async fn flagged_null_evidence_suppresses_subscription_write() {
    let mut fx = Fx::new().await;
    fx.add_user("u1").await;
    let stored = fx.seed_active("u1", "sub_a").await;
    let mut stale = fx.fold("u1", &remote("sub_a", &[])).await;
    assert!(stale.explicit_stale);
    stale.row = divergent(&fx, "sub_a", &stored).await.row;
    stale.row.paid_through_unix = None;
    stale.row.paid_invoice_id = None;
    fx.put("u1", &stale).await;
    assert_eq!(fx.snap("sub_a").await, Some(stored));
    fx.cleanup().await;
}

#[tokio::test]
async fn stale_and_fresh_commit_orders_converge() {
    let mut fx = Fx::new().await;
    let mut results = Vec::new();
    for user in ["u1", "u2"] {
        fx.add_user(user).await;
        let sub = format!("sub_{user}");
        fx.seed_active(user, &sub).await;
        let stale = fx.fold(user, &remote(&sub, &[invoice(&sub, 1)])).await;
        let mut renewal = remote(
            &sub,
            &[invoice(&sub, 1), invoice(&sub, 2), invoice(&sub, 3)],
        );
        renewal.subscription.current_period_end = Some(T + 3 * P);
        let fresh = fx.fold(user, &renewal).await;
        assert!(stale.explicit_stale);
        assert!(!fresh.explicit_stale);
        if user == "u1" {
            fx.put(user, &stale).await;
            fx.put(user, &fresh).await;
        } else {
            fx.put(user, &fresh).await;
            fx.put(user, &stale).await;
        }
        let result = fx.snap(&sub).await.unwrap();
        assert_eq!(result.paid_through, Some(T + 3 * P));
        assert_eq!(result.anchor, Some(format!("inv_{sub}_3")));
        assert_eq!(result.period_end, Some(T + 3 * P));
        assert_eq!(fx.history_rows(&sub).await, 3);
        results.push(Snap {
            user_id: String::new(),
            anchor: None,
            ..result
        });
    }
    assert_eq!(results[0], results[1]);
    fx.cleanup().await;
}

#[tokio::test]
async fn insertion_conflicts_only_while_another_subscription_holds_the_slot() {
    let mut fx = Fx::new().await;
    let live = ["incomplete", "active", "past_due", "unpaid", "paused"];
    let outside = [
        "canceled",
        "incomplete_expired",
        "trialing",
        "unknown_state",
    ];
    for (n, status) in live.iter().chain(outside.iter()).enumerate() {
        let user = format!("ux{n}");
        fx.add_user(&user).await;
        let holder = format!("sub_holder_{n}");
        let newcomer = format!("sub_new_{n}");
        fx.raw(&user, &holder, status, None).await;
        let input = ReconcileInput {
            stripe_customer_id: Some(CUS.into()),
            live: None,
            bindings: bindings(),
        };
        let outcome =
            reconcile_subscription(&input, &remote(&newcomer, &[invoice(&newcomer, 1)]), T);
        let result = fx.try_put(&user, &outcome).await;
        if live.contains(status) {
            let error = result.expect_err(status);
            assert_eq!(
                error.code(),
                Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION),
                "holder status {status}"
            );
            assert!(fx.snap(&newcomer).await.is_none(), "holder {status}");
            assert_eq!(fx.rows(&user).await, 1);
            assert_eq!(fx.history_rows(&newcomer).await, 0, "holder {status}");
        } else {
            result.expect(status);
            assert!(fx.snap(&newcomer).await.is_some(), "holder {status}");
            assert_eq!(fx.rows(&user).await, 2);
            assert_eq!(fx.history_rows(&newcomer).await, 1);
        }
    }
    fx.add_user("other").await;
    fx.raw("other", "sub_other", "active", None).await;
    fx.add_user("free_user").await;
    let outcome = fx
        .fold("free_user", &remote("sub_free", &[invoice("sub_free", 1)]))
        .await;
    fx.put("free_user", &outcome).await;
    assert!(fx.snap("sub_free").await.is_some());
    fx.cleanup().await;
}

#[tokio::test]
async fn foreign_remote_without_live_anchor_rolls_back_on_unique_violation() {
    let mut fx = Fx::new().await;
    fx.add_user("u1").await;
    let a = fx.seed_active("u1", "sub_a").await;
    let live = fx.live("u1").await.expect("anchored live A");
    assert_eq!(live.stripe_subscription_id, "sub_a");
    assert_eq!(live.paid_invoice_id, a.anchor);
    let b = remote("sub_b", &[invoice("sub_b", 1), invoice("sub_b", 2)]);
    assert!(b.invoices.iter().all(|i| Some(&i.id) != a.anchor.as_ref()));
    let outcome = fx.fold("u1", &b).await;
    assert_eq!(outcome.row.stripe_subscription_id, "sub_b");
    let error = fx.try_put("u1", &outcome).await.expect_err("foreign B");
    assert_eq!(
        error.code(),
        Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
    );
    assert_eq!(fx.snap("sub_a").await, Some(a));
    assert!(fx.snap("sub_b").await.is_none());
    assert_eq!(fx.rows("u1").await, 1);
    assert_eq!(fx.history_rows("sub_a").await, 2);
    assert_eq!(fx.history_rows("sub_b").await, 0);
    fx.cleanup().await;
}

#[tokio::test]
async fn setup_snapshots_and_live_selection_are_isolated() {
    let fx = Fx::new().await;
    for user in ["u1", "u2", "u3"] {
        fx.add_user(user).await;
    }
    fx.raw("u1", "sub_terminal", "canceled", None).await;
    fx.raw("u1", "sub_live", "active", Some(T)).await;
    fx.raw("u2", "sub_other", "past_due", Some(T + 60)).await;
    fx.raw("u3", "sub_only_terminal", "canceled", None).await;

    assert_eq!(fx.rows("u1").await, 2);
    assert_eq!(fx.rows("u2").await, 1);
    assert_eq!(fx.rows("u3").await, 1);
    assert_eq!(fx.rows("missing").await, 0);
    let terminal = fx.snap("sub_terminal").await.unwrap();
    assert_eq!(terminal.user_id, "u1");
    assert_eq!(terminal.status, "canceled");
    assert_eq!(terminal.paid_through, None);
    let live = fx.snap("sub_live").await.unwrap();
    assert_eq!(live.user_id, "u1");
    assert_eq!(live.status, "active");
    assert_eq!(live.paid_through, Some(T));
    assert_eq!(live.anchor.as_deref(), Some("inv_raw_sub_live"));
    assert!(fx.snap("missing").await.is_none());
    let selected = fx.live("u1").await.unwrap();
    assert_eq!(selected.stripe_subscription_id, "sub_live");
    assert_eq!(selected.status, "active");
    assert_eq!(selected.paid_through_unix, Some(T));
    assert_eq!(
        selected.paid_invoice_id.as_deref(),
        Some("inv_raw_sub_live")
    );
    let other = fx.live("u2").await.unwrap();
    assert_eq!(other.stripe_subscription_id, "sub_other");
    assert_eq!(other.status, "past_due");
    assert_eq!(other.paid_through_unix, Some(T + 60));
    assert!(fx.live("u3").await.is_none());
    assert!(fx.live("missing").await.is_none());
    fx.cleanup().await;
}

#[tokio::test]
async fn target_selector_reads_archived_rows_without_changing_live_selection() {
    let fx = Fx::new().await;
    fx.add_user("u1").await;
    fx.add_user("u2").await;
    fx.raw("u1", "live", "active", Some(200)).await;
    for status in ["canceled", "incomplete_expired", "trialing", "unknown"] {
        fx.raw("u1", status, status, Some(100)).await;
        fx.db.execute("UPDATE subscriptions SET grace_until = to_timestamp(75) WHERE stripe_subscription_id = $1", &[&status]).await.unwrap();
        let before = fx.snap(status).await;
        let selected = fx.select("u1", Some(status)).await.unwrap();
        assert_eq!(selected.stripe_customer_id, CUS);
        assert_eq!(selected.stripe_item_id, "si_raw");
        assert_eq!(selected.stripe_price_id, PRICE);
        assert_eq!(selected.plan_id, "plus");
        assert_eq!(selected.stripe_subscription_id, status);
        assert_eq!(selected.status, status);
        assert_eq!(selected.paid_through_unix, Some(100));
        assert_eq!(
            selected.paid_invoice_id.as_deref(),
            Some(format!("inv_raw_{status}").as_str())
        );
        assert_eq!(selected.grace_until_unix, Some(75));
        assert_eq!(fx.snap(status).await, before);
        assert!(fx.select("u2", Some(status)).await.is_none());
        assert_eq!(fx.history_rows(status).await, 0);
    }
    assert_eq!(
        fx.select("u1", None).await.unwrap().stripe_subscription_id,
        "live"
    );
    assert_eq!(
        fx.select("u1", Some("live")).await.unwrap(),
        fx.live("u1").await.unwrap()
    );
    assert!(fx.select("u1", Some("missing")).await.is_none());
    assert!(fx.select("missing", Some("live")).await.is_none());
    fx.raw("u2", "terminal", "canceled", None).await;
    assert!(fx.live("u2").await.is_none());
    assert_eq!(fx.rows("u1").await, 5);
    assert_eq!(fx.rows("u2").await, 1);
    for (i, status) in ["incomplete", "active", "past_due", "unpaid", "paused"]
        .iter()
        .enumerate()
    {
        let user = format!("live_user_{i}");
        fx.add_user(&user).await;
        fx.raw(&user, &format!("live_{i}"), status, None).await;
        assert_eq!(fx.live(&user).await.unwrap().status, *status);
    }
    fx.cleanup().await;
}

#[tokio::test]
async fn retained_subscription_rows_follow_existing_upsert_guards() {
    let mut fx = Fx::new().await;
    fx.add_user("u1").await;
    fx.add_user("u2").await;
    // A and B are outside the live-slot index; fake displacement is not SQL selection.
    let mut a = fx.fold("u1", &remote("sub_test_1", &[])).await;
    a.history.clear();
    a.row.stripe_customer_id = "cus_test_1".into();
    a.row.stripe_item_id = "si_test_1".into();
    a.row.stripe_price_id = PRICE.into();
    a.row.plan_id = "plus".into();
    a.read_plan = None;
    a.read_anchor = None;
    a.row.current_period_end_unix = Some(1_700_000_000 + P);
    a.row.cancel_at_period_end = false;
    a.row.status = "canceled".into();
    a.row.paid_through_unix = Some(100);
    a.row.paid_invoice_id = Some("in_100".into());
    a.row.grace_until_unix = Some(10);
    let mut b = a.clone();
    b.row.stripe_subscription_id = "sub_other".into();
    b.row.paid_through_unix = None;
    b.row.paid_invoice_id = None;
    b.row.grace_until_unix = None;
    fx.put("u1", &a).await;
    let mut other = a.clone();
    other.row.stripe_subscription_id = "sub_user_2".into();
    other.row.paid_through_unix = Some(999);
    fx.put("u2", &other).await;
    let other = fx.snap("sub_user_2").await.unwrap();
    fx.put("u1", &b).await;
    let mut expected = Snap {
        user_id: "u1".into(),
        customer: "cus_test_1".into(),
        item: "si_test_1".into(),
        price: PRICE.into(),
        plan: "plus".into(),
        status: "canceled".into(),
        period_end: Some(1_700_000_000 + P),
        cancel: false,
        paid_through: Some(100),
        anchor: Some("in_100".into()),
        grace: Some(10),
    };
    let expected_b = Snap {
        paid_through: None,
        anchor: None,
        grace: None,
        ..expected.clone()
    };
    assert_eq!(fx.snap("sub_test_1").await.as_ref(), Some(&expected));
    assert_eq!(fx.snap("sub_other").await.as_ref(), Some(&expected_b));
    let mut stale = a.clone();
    stale.row.paid_through_unix = Some(50);
    stale.row.status = "incomplete_expired".into();
    stale.row.stripe_customer_id = "cus_stale".into();
    stale.row.stripe_item_id = "si_stale".into();
    stale.row.stripe_price_id = "price_stale".into();
    stale.row.current_period_end_unix = Some(50);
    stale.row.cancel_at_period_end = true;
    for _ in 0..2 {
        fx.put("u1", &stale).await;
        assert_eq!(fx.snap("sub_test_1").await.as_ref(), Some(&expected));
        fx.put("u1", &b).await;
        assert_eq!(fx.snap("sub_test_1").await.as_ref(), Some(&expected));
        assert_eq!(fx.snap("sub_other").await.as_ref(), Some(&expected_b));
    }
    let mut advanced = a.clone();
    advanced.row.plan_id = "pro".into();
    for end in [150, 200] {
        advanced.row.paid_through_unix = Some(end);
        advanced.row.paid_invoice_id = Some(format!("in_{end}"));
        fx.put("u1", &advanced).await;
        expected.plan = "pro".into();
        expected.paid_through = Some(end);
        expected.anchor = Some(format!("in_{end}"));
        assert_eq!(fx.snap("sub_test_1").await.as_ref(), Some(&expected));
    }
    fx.put("u1", &b).await;
    fx.put("u1", &a).await;
    assert_eq!(fx.snap("sub_test_1").await.as_ref(), Some(&expected));
    assert_eq!(fx.snap("sub_other").await.as_ref(), Some(&expected_b));
    assert_eq!(fx.history_rows("sub_test_1").await, 0);
    assert_eq!(fx.rows("u1").await, 2);

    // The same named write-boundary cases are exercised by the handler test.
    for case in [
        "valid",
        "shared",
        "read-plan",
        "read-anchor",
        "no-anchor",
        "null-end",
        "first-end",
        "advance-null-anchor",
    ] {
        let sub = format!("sub_{case}");
        let mut stored = a.clone();
        stored.row.stripe_subscription_id = sub.clone();
        stored.row.paid_through_unix = if case == "first-end" { None } else { Some(100) };
        stored.row.paid_invoice_id = Some("in_old".into());
        stored.row.grace_until_unix = Some(10);
        fx.put("u1", &stored).await;
        let mut replacement = stored.clone();
        replacement.row.stripe_subscription_id = format!("replacement_{case}");
        fx.put("u1", &replacement).await;
        let replacement_before = fx.snap(&replacement.row.stripe_subscription_id).await;

        let mut incoming = stored.clone();
        incoming.row.status = "incomplete_expired".into();
        incoming.row.plan_id = "pro".into();
        incoming.row.paid_through_unix = Some(100);
        incoming.row.paid_invoice_id = Some("in_new".into());
        incoming.row.grace_until_unix = Some(20);
        incoming.row.stripe_customer_id = "cus_new".into();
        incoming.row.stripe_item_id = "si_new".into();
        incoming.row.stripe_price_id = "price_new".into();
        incoming.row.current_period_end_unix = Some(200);
        incoming.row.cancel_at_period_end = true;
        incoming.read_plan = Some("plus".into());
        incoming.read_anchor = Some("in_old".into());
        match case {
            "shared" => incoming.row.paid_invoice_id = Some("in_old".into()),
            "read-plan" => incoming.read_plan = Some("pro".into()),
            "read-anchor" => incoming.read_anchor = Some("in_wrong".into()),
            "no-anchor" | "advance-null-anchor" => incoming.row.paid_invoice_id = None,
            "null-end" => incoming.row.paid_through_unix = None,
            _ => {}
        }
        if case == "advance-null-anchor" {
            incoming.row.paid_through_unix = Some(150);
        }
        let stored_before = fx.snap(&sub).await.unwrap();
        fx.put("u1", &incoming).await;
        let mut expected = Snap {
            user_id: "u1".into(),
            customer: "cus_new".into(),
            item: "si_new".into(),
            price: "price_new".into(),
            plan: "pro".into(),
            status: "incomplete_expired".into(),
            period_end: Some(200),
            cancel: true,
            paid_through: Some(100),
            anchor: Some("in_new".into()),
            grace: Some(20),
        };
        if matches!(case, "read-plan" | "read-anchor" | "no-anchor" | "null-end") {
            expected.plan = "plus".into();
            expected.anchor = Some("in_old".into());
        } else if case == "shared" || case == "advance-null-anchor" {
            expected.anchor = Some("in_old".into());
        }
        if case == "no-anchor" {
            expected = stored_before;
        }
        if case == "advance-null-anchor" {
            expected.paid_through = Some(150);
        }
        assert_eq!(fx.snap(&sub).await, Some(expected), "{case}");
        assert_eq!(
            fx.snap(&replacement.row.stripe_subscription_id).await,
            replacement_before,
            "{case} replacement"
        );
        assert_eq!(fx.history_rows(&sub).await, 0, "{case}");
    }
    assert_eq!(fx.snap("sub_user_2").await, Some(other));
    assert_eq!(fx.rows("u2").await, 1);
    fx.cleanup().await;
}

// The fixture relation is migrated in a private, generated schema. These
// clients never lock or write the public production-named relation.
#[tokio::test]
async fn table_exclusion_blocks_raw_writer_but_not_plain_reader() {
    use std::time::Duration;
    use tokio::time::timeout;

    let url = std::env::var("NNTUNE_TEST_DATABASE_URL").unwrap();
    let config = disposable_config(&url).unwrap();
    let schema = format!(
        "billing_test_{}_{}",
        std::process::id(),
        SCHEMA.fetch_add(1, Ordering::Relaxed)
    );
    let role = format!("{schema}_dml");
    let read_role = format!("{schema}_read");
    let (db, connection) = timeout(Duration::from_secs(5), config.connect(NoTls))
        .await
        .expect("fixture connection timed out")
        .expect("fixture connection failed");
    let driver = tokio::spawn(connection);
    let setup = timeout(Duration::from_secs(5), async {
        db.batch_execute(&format!("BEGIN; CREATE SCHEMA {schema}; SET LOCAL search_path TO {schema}; SET LOCAL statement_timeout = '4s'; SET LOCAL transaction_timeout = '10s'")).await?;
        for migration in [
            include_str!("../../../migrations/0002_auth_vault.sql"),
            include_str!("../../../migrations/0021_billing_schema.sql"),
            include_str!("../../../migrations/0026_plan_reseed_no_trial.sql"),
            include_str!("../../../migrations/0029_paid_invoice_history.sql"),
        ] {
            db.batch_execute(migration).await?;
        }
        db.execute("INSERT INTO users (id,email,created_at) VALUES ('lock_owner','lock_owner@example.test',$1)", &[&T]).await?;
        db.execute("INSERT INTO subscriptions (user_id, stripe_subscription_id, stripe_customer_id, stripe_subscription_item_id, stripe_price_id, plan_id, status) VALUES ('lock_owner','sub_lock',$1,'si_raw',$2,'plus','active')", &[&CUS, &PRICE]).await?;
        db.batch_execute(&format!("CREATE ROLE {role} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT; CREATE ROLE {read_role} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT; GRANT USAGE ON SCHEMA {schema} TO {role}, {read_role}; GRANT SELECT, INSERT, UPDATE, DELETE ON {schema}.subscriptions TO {role}; GRANT SELECT ON {schema}.subscriptions TO {read_role}; SET search_path TO {schema}; COMMIT")).await
    }).await;
    if !matches!(setup, Ok(Ok(()))) {
        // Close an uncertain transaction before trying to remove a possibly committed schema.
        drop(db);
        driver.abort();
        let cleanup = timeout(Duration::from_secs(5), async {
            let (cleanup_db, cleanup_connection) = config.connect(NoTls).await?;
            let cleanup_driver = tokio::spawn(cleanup_connection);
            let result = cleanup_db
                .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE; DROP ROLE IF EXISTS {role}; DROP ROLE IF EXISTS {read_role}"))
                .await;
            cleanup_driver.abort();
            result
        })
        .await;
        panic!("fixture setup failed: {setup:?}; cleanup: {cleanup:?}");
    }
    let fx = Fx { db, schema };
    let schema = &fx.schema;
    let mut pending = None;
    let mut drivers = vec![driver];

    // Keep all phases below the holder's five-second idle timeout.
    let result: Result<(), String> = timeout(Duration::from_secs(4), async {
        let (writer, writer_connection) = timeout(Duration::from_secs(5), config.connect(NoTls)).await.map_err(|_| "writer connection timed out".to_string())?.map_err(|e| e.to_string())?;
        drivers.push(tokio::spawn(writer_connection));
        let (reader, reader_connection) = timeout(Duration::from_secs(5), config.connect(NoTls)).await.map_err(|_| "reader connection timed out".to_string())?.map_err(|e| e.to_string())?;
        drivers.push(tokio::spawn(reader_connection));
        timeout(Duration::from_secs(5), async {
            writer.batch_execute(&format!("BEGIN; SET LOCAL ROLE {read_role}; SET LOCAL statement_timeout = '4s'")).await?;
            writer.query_one(&format!("SELECT count(*) FROM {schema}.subscriptions"), &[]).await?;
            let denied = writer.batch_execute(&format!("LOCK TABLE {schema}.subscriptions IN SHARE ROW EXCLUSIVE MODE")).await;
            writer.batch_execute("ROLLBACK").await?;
            Ok::<_, tokio_postgres::Error>(denied)
        }).await.map_err(|_| "permission check timed out".to_string())?.map_err(|e| e.to_string())?
            .err().and_then(|e| e.code().cloned())
            .filter(|code| code == &tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
            .ok_or_else(|| "lock denial was not insufficient_privilege".to_string())?;
        // Commit the DML probe first so its row and table locks cannot mask exclusion.
        fx.db.batch_execute(&format!("BEGIN; SET LOCAL ROLE {role}; SET LOCAL statement_timeout = '3s'")).await.map_err(|e| e.to_string())?;
        let updated = fx.db.execute(&format!("UPDATE {schema}.subscriptions SET cancel_at_period_end = false WHERE stripe_subscription_id = 'sub_lock'"), &[]).await.map_err(|e| e.to_string())?;
        if updated != 1 { return Err(format!("restricted update affected {updated} rows")); }
        fx.db.batch_execute("COMMIT").await.map_err(|e| e.to_string())?;
        let (holder_pid, writer_pid, holder) = timeout(Duration::from_secs(5), async {
            fx.db.batch_execute(&format!("BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL ROLE {role}; SET LOCAL lock_timeout = '2s'; SET LOCAL idle_in_transaction_session_timeout = '5s'; SET LOCAL transaction_timeout = '15s'; SET LOCAL statement_timeout = '4s'")).await?;
            fx.db.batch_execute(&format!("LOCK TABLE {schema}.subscriptions IN SHARE ROW EXCLUSIVE MODE")).await?;
            fx.db.query_one(&format!("SELECT count(*) FROM {schema}.subscriptions"), &[]).await?;
            writer.batch_execute("SET statement_timeout = '10s'").await?;
            let holder = fx.db.query_one("SELECT pg_backend_pid(), current_user::text, (SELECT rolsuper FROM pg_roles WHERE rolname = current_user), current_setting('lock_timeout'), current_setting('statement_timeout'), current_setting('idle_in_transaction_session_timeout'), current_setting('transaction_timeout')", &[]).await?;
            let holder_pid: i32 = holder.get(0);
            let writer_pid: i32 = writer.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
            Ok::<_, tokio_postgres::Error>((holder_pid, writer_pid, holder))
        }).await.map_err(|_| "lock setup timed out".to_string())?.map_err(|e| e.to_string())?;
        if holder.get::<_, String>(1) != role || holder.get::<_, bool>(2)
            || holder.get::<_, String>(3) != "2s" || holder.get::<_, String>(4) != "4s"
            || holder.get::<_, String>(5) != "5s" || holder.get::<_, String>(6) != "15s" {
            return Err("restricted backend role or transaction-local settings differ".into());
        }
        let (baseline, after, timeout_error, same_backend) = timeout(Duration::from_secs(4), async {
            let baseline: String = writer.query_one("SHOW lock_timeout", &[]).await?.get(0);
            writer.batch_execute(&format!("BEGIN; SET LOCAL ROLE {role}; SET LOCAL lock_timeout = '250ms'; SET LOCAL statement_timeout = '3s'; SET LOCAL transaction_timeout = '4s'")).await?;
            let pid: i32 = writer.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
            let error = writer.batch_execute(&format!("LOCK TABLE {schema}.subscriptions IN SHARE ROW EXCLUSIVE MODE")).await;
            writer.batch_execute("ROLLBACK").await?;
            let after: String = writer.query_one("SHOW lock_timeout", &[]).await?.get(0);
            let after_pid: i32 = writer.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
            Ok::<_, tokio_postgres::Error>((baseline, after, error, pid == writer_pid && pid == after_pid))
        }).await.map_err(|_| "bounded lock wait timed out externally".to_string())?.map_err(|e| e.to_string())?;
        if !same_backend || baseline != after { return Err("transaction settings or backend were not restored".into()); }
        let timeout_error = timeout_error.err().ok_or_else(|| "contended lock unexpectedly succeeded".to_string())?;
        if timeout_error.code() != Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE) { return Err(format!("unexpected lock error: {timeout_error}")); }
        let sql = format!("UPDATE {schema}.subscriptions SET cancel_at_period_end = true WHERE stripe_subscription_id = 'sub_lock'");
        pending = Some(tokio::spawn(async move { writer.execute(&sql, &[]).await }));
        let task = pending.as_mut().unwrap();
        timeout(Duration::from_secs(5), async {
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 2] = [&holder_pid, &writer_pid];
            loop {
                tokio::select! {
                    result = &mut *task => return Err(format!("writer finished before observed block: {result:?}")),
                    result = reader.query_one("SELECT $1 = ANY(pg_blocking_pids($2))", &params) => {
                        if result.map_err(|e| e.to_string())?.get::<_, bool>(0) { break; }
                    }
                }
                tokio::task::yield_now().await;
            }
            Ok(())
        }).await.map_err(|_| "writer blocking observation timed out".to_string())??;
        let row = timeout(Duration::from_secs(5), reader.query_one(&format!("SELECT cancel_at_period_end FROM {schema}.subscriptions WHERE stripe_subscription_id = 'sub_lock'"), &[])).await.map_err(|_| "plain read timed out".to_string())?.map_err(|e| e.to_string())?;
        if row.get::<_, bool>(0) { return Err("writer changed row before lock release".into()); }
        timeout(Duration::from_secs(5), fx.db.batch_execute("COMMIT")).await.map_err(|_| "commit timed out".to_string())?.map_err(|e| e.to_string())?;
        let count = timeout(Duration::from_secs(5), &mut *task).await.map_err(|_| "writer completion timed out".to_string())?.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        if count != 1 { return Err(format!("writer affected {count} rows")); }
        Ok(())
    }).await.unwrap_or_else(|_| Err("contention phases timed out".into()));

    if let Some(task) = pending {
        task.abort();
    }
    let _ = timeout(Duration::from_secs(5), fx.db.batch_execute("ROLLBACK")).await;
    let Fx { db, schema } = fx;
    drop(db);
    for driver in drivers {
        driver.abort();
    }
    // The holder can have been terminated by a server timeout; do not reuse it.
    let cleanup = timeout(Duration::from_secs(5), async {
        let (cleanup_db, cleanup_connection) = config.connect(NoTls).await?;
        let cleanup_driver = tokio::spawn(cleanup_connection);
        let result = cleanup_db.batch_execute(&format!("SET statement_timeout = '4s'; DROP SCHEMA IF EXISTS {schema} CASCADE; DROP ROLE IF EXISTS {role}; DROP ROLE IF EXISTS {read_role}")).await;
        cleanup_driver.abort();
        result
    }).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "fixture cleanup failed: {cleanup:?}; contention phases: {result:?}"
    );
    result.expect("contention phases failed");
}

// A writer that does not take the advisory lock may commit or roll back while
// the table lock is waiting. The subsequent READ COMMITTED statement must see
// the resulting state; a pre-lock observation is not ownership evidence.
#[tokio::test]
async fn table_exclusion_classifies_after_writer_commit_or_rollback() {
    snapshot_fixture(false)
        .await
        .expect("snapshot phases failed");
}

#[tokio::test]
async fn snapshot_fixture_cleans_up_after_early_lock_completion() {
    let result = snapshot_fixture(true).await;
    assert_eq!(result.unwrap_err(), "lock finished before wait barrier");
}

async fn snapshot_fixture(force_early_completion: bool) -> Result<(), String> {
    use std::time::Duration;
    use tokio::time::timeout;

    let config = disposable_config(&std::env::var("NNTUNE_TEST_DATABASE_URL").unwrap()).unwrap();
    let schema = format!(
        "billing_snapshot_{}_{}",
        std::process::id(),
        SCHEMA.fetch_add(1, Ordering::Relaxed)
    );
    let mut drivers = Vec::new();
    let mut pending = Vec::new();
    let result: Result<(), String> = timeout(Duration::from_secs(25), async {
        let (observer, connection) = config.connect(NoTls).await.map_err(|e| e.to_string())?;
        drivers.push(tokio::spawn(connection));
        observer.batch_execute(&format!("SET statement_timeout = '4s'; CREATE SCHEMA {schema}; CREATE TABLE {schema}.subscriptions (stripe_subscription_id text PRIMARY KEY, user_id text NOT NULL)")).await.map_err(|e| e.to_string())?;
        for commit in [true, false] {
            let (writer, connection) = config.connect(NoTls).await.map_err(|e| e.to_string())?;
            drivers.push(tokio::spawn(connection));
            let (reader, connection) = config.connect(NoTls).await.map_err(|e| e.to_string())?;
            drivers.push(tokio::spawn(connection));
            let writer_pid: i32 = writer.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
            let reader_pid: i32 = reader.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
            writer.batch_execute(&format!("BEGIN; SET LOCAL statement_timeout = '4s'; INSERT INTO {schema}.subscriptions VALUES ('sub', 'other')")).await.map_err(|e| e.to_string())?;
            reader.batch_execute("BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL lock_timeout = '4s'; SET LOCAL statement_timeout = '4s'; SET LOCAL transaction_timeout = '10s'").await.map_err(|e| e.to_string())?;
            let before = reader.query_opt(&format!("SELECT user_id FROM {schema}.subscriptions WHERE stripe_subscription_id = 'sub'"), &[]).await.map_err(|e| e.to_string())?;
            if before.is_some() { return Err("uncommitted writer was visible".into()); }
            let sql = if force_early_completion { "INVALID LOCK".to_string() } else { format!("LOCK TABLE {schema}.subscriptions IN SHARE ROW EXCLUSIVE MODE") };
            let (phase_tx, phase_rx) = billing_contention::phase();
            pending.push(tokio::spawn(async move {
                let _ = phase_tx.send(());
                let result = reader.batch_execute(&sql).await;
                (reader, result)
            }));
            billing_contention::reached(phase_rx).await?;
            let task = pending.last_mut().unwrap();
            if force_early_completion {
                timeout(Duration::from_secs(3), async {
                    while !task.is_finished() { tokio::task::yield_now().await; }
                }).await.map_err(|_| "early completion timed out".to_string())?;
            }
            let relation = format!("{schema}.subscriptions");
            let wait = billing_contention::relation_wait(
                &observer, task, writer_pid, reader_pid, &relation, "ShareRowExclusiveLock",
            ).await;
            if let Err(error) = wait {
                if error == "lock finished before wait barrier" {
                    pending.pop(); // The helper consumed this handle's output.
                }
                return Err(error);
            }
            writer.batch_execute(if commit { "COMMIT" } else { "ROLLBACK" }).await.map_err(|e| e.to_string())?;
            let completed = timeout(Duration::from_secs(5), pending.last_mut().unwrap()).await.map_err(|_| "lock did not resume".to_string())?;
            pending.pop(); // The handle has been consumed, even if the task failed.
            let (reader, lock) = completed.map_err(|e| e.to_string())?;
            lock.map_err(|e| e.to_string())?;
            let after = reader.query_opt(&format!("SELECT user_id FROM {schema}.subscriptions WHERE stripe_subscription_id = 'sub'"), &[]).await.map_err(|e| e.to_string())?;
            let owner = after.map(|row| row.get::<_, String>(0));
            if owner.as_deref() != if commit { Some("other") } else { None } {
                return Err(format!("unexpected post-lock owner after commit={commit}: {owner:?}"));
            }
            reader.batch_execute("ROLLBACK").await.map_err(|e| e.to_string())?;
            observer.batch_execute(&format!("DELETE FROM {schema}.subscriptions")).await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }).await.unwrap_or_else(|_| Err("snapshot phases timed out".into()));
    for task in pending {
        task.abort();
        let _ = task.await;
    }
    for driver in drivers {
        driver.abort();
        let _ = driver.await;
    }
    let cleanup = timeout(Duration::from_secs(5), async {
        let (db, connection) = config.connect(NoTls).await?;
        let driver = tokio::spawn(connection);
        let result = async {
            db.batch_execute(&format!("SET lock_timeout = '3s'; SET statement_timeout = '4s'; DROP SCHEMA IF EXISTS {schema} CASCADE")).await?;
            let exists: bool = db.query_one("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)", &[&schema]).await?.get(0);
            Ok::<_, tokio_postgres::Error>(exists)
        }.await;
        drop(db);
        driver.abort();
        let _ = driver.await;
        result
    }).await;
    assert!(
        matches!(cleanup, Ok(Ok(false))),
        "fixture cleanup failed: {cleanup:?}; snapshot phases: {result:?}"
    );
    result
}

// These are server-side expiry controls on a disposable relation, not proof of
// production pooling, role privileges, version support, or driver cancellation.
#[tokio::test]
async fn abandoned_table_lock_expires_and_rollback_cleans_local_settings() {
    use std::time::Duration;
    use tokio::time::timeout;

    let config = disposable_config(&std::env::var("NNTUNE_TEST_DATABASE_URL").unwrap()).unwrap();
    let schema = format!(
        "billing_expiry_{}_{}",
        std::process::id(),
        SCHEMA.fetch_add(1, Ordering::Relaxed)
    );
    let mut drivers = Vec::new();
    let mut pending = Vec::new();
    let result: Result<(), String> = timeout(Duration::from_secs(30), async {
        let (observer, connection) = config.connect(NoTls).await.map_err(|e| e.to_string())?;
        drivers.push(tokio::spawn(connection));
        observer.batch_execute(&format!("SET statement_timeout = '4s'; CREATE SCHEMA {schema}; CREATE TABLE {schema}.subscriptions (id integer PRIMARY KEY, value integer NOT NULL); INSERT INTO {schema}.subscriptions VALUES (1, 0)")).await.map_err(|e| e.to_string())?;

        // Acknowledged rollback on a surviving connection is distinct from expiry.
        let baseline: String = observer.query_one("SHOW lock_timeout", &[]).await.map_err(|e| e.to_string())?.get(0);
        let pid: i32 = observer.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
        observer.batch_execute(&format!("BEGIN; SET LOCAL lock_timeout = '1s'; LOCK TABLE {schema}.subscriptions IN SHARE ROW EXCLUSIVE MODE; UPDATE {schema}.subscriptions SET value = 99; ROLLBACK")).await.map_err(|e| e.to_string())?;
        let row = observer.query_one(&format!("SELECT pg_backend_pid(), current_setting('lock_timeout'), (SELECT value FROM {schema}.subscriptions WHERE id = 1), EXISTS (SELECT 1 FROM pg_locks WHERE pid = pg_backend_pid() AND relation = '{schema}.subscriptions'::regclass AND mode = 'ShareRowExclusiveLock')"), &[]).await.map_err(|e| e.to_string())?;
        if row.get::<_, i32>(0) != pid || row.get::<_, String>(1) != baseline || row.get::<_, i32>(2) != 0 || row.get::<_, bool>(3) {
            return Err("acknowledged rollback did not restore settings, data, and lock state".into());
        }

        for expiry in ["transaction", "idle"] {
            let (holder, connection) = config.connect(NoTls).await.map_err(|e| e.to_string())?;
            drivers.push(tokio::spawn(connection));
            let (writer, connection) = config.connect(NoTls).await.map_err(|e| e.to_string())?;
            drivers.push(tokio::spawn(connection));
            writer.batch_execute("SET statement_timeout = '8s'").await.map_err(|e| e.to_string())?;
            let holder_pid: i32 = holder.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
            let writer_pid: i32 = writer.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
            let settings = if expiry == "transaction" {
                "SET LOCAL transaction_timeout = '3s'; SET LOCAL idle_in_transaction_session_timeout = '0'"
            } else {
                "SET LOCAL transaction_timeout = '10s'; SET LOCAL idle_in_transaction_session_timeout = '3s'"
            };
            holder.batch_execute(&format!("BEGIN ISOLATION LEVEL READ COMMITTED; {settings}; SET LOCAL statement_timeout = '8s'; LOCK TABLE {schema}.subscriptions IN SHARE ROW EXCLUSIVE MODE; UPDATE {schema}.subscriptions SET value = 10 WHERE id = 1")).await.map_err(|e| e.to_string())?;
            let sql = format!("UPDATE {schema}.subscriptions SET value = value + 1 WHERE id = 1");
            pending.push(tokio::spawn(async move { writer.execute(&sql, &[]).await }));
            let task = pending.last_mut().unwrap();
            // Observe the actual wait before allowing the server timeout to end it.
            let relation = format!("{schema}.subscriptions");
            let params: [&(dyn ToSql + Sync); 3] = [&holder_pid, &writer_pid, &relation];
            timeout(Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        result = &mut *task => return Err(format!("{expiry} writer finished before wait barrier: {result:?}")),
                        row = observer.query_one("SELECT $1 = ANY(pg_blocking_pids($2)) AND EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1 AND relation = $3::text::regclass AND mode = 'ShareRowExclusiveLock' AND granted)", &params) => {
                            if row.map_err(|e| e.to_string())?.get::<_, bool>(0) { break; }
                        }
                    }
                    tokio::task::yield_now().await;
                }
                Ok::<(), String>(())
            }).await.map_err(|_| format!("{expiry} wait barrier timed out"))??;
            let expiry_error = if expiry == "transaction" {
                // Keep the client alive even if the statement unexpectedly succeeds.
                let (send, receive) = tokio::sync::oneshot::channel();
                pending.push(tokio::spawn(async move {
                    let result = holder.batch_execute("SELECT pg_sleep(6)").await;
                    let code = result.err().and_then(|e| e.code().map(|c| c.code().to_owned()));
                    let _ = send.send(code);
                    std::future::pending::<()>().await;
                    drop(holder);
                    Ok::<u64, tokio_postgres::Error>(0)
                }));
                Some(receive)
            } else {
                // Keep the client alive but issue no command while the idle timer runs.
                pending.push(tokio::spawn(async move { std::future::pending::<()>().await; drop(holder); Ok::<u64, tokio_postgres::Error>(0) }));
                None
            };
            let writer_index = pending.len() - 2;
            let task = &mut pending[writer_index];
            let count = timeout(Duration::from_secs(8), task).await.map_err(|_| format!("{expiry} writer did not resume"))?.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
            if count != 1 { return Err(format!("{expiry} writer affected {count} rows")); }
            if let Some(receive) = expiry_error {
                let code = timeout(Duration::from_secs(5), receive).await.map_err(|_| "transaction expiry error timed out".to_string())?.map_err(|e| e.to_string())?;
                if code.as_deref() != Some("25P04") { return Err(format!("unexpected transaction expiry SQLSTATE: {code:?}")); }
            }
            // Backend exit can trail lock release. Observe it separately, within a bound.
            timeout(Duration::from_secs(5), async {
                loop {
                    let row = observer.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1), EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1)", &[&holder_pid]).await.map_err(|e| e.to_string())?;
                    if !row.get::<_, bool>(0) && !row.get::<_, bool>(1) { break; }
                    tokio::task::yield_now().await;
                }
                Ok::<(), String>(())
            }).await.map_err(|_| format!("{expiry} backend exit timed out"))??;
            let value: i32 = observer.query_one(&format!("SELECT value FROM {schema}.subscriptions WHERE id = 1"), &[]).await.map_err(|e| e.to_string())?.get(0);
            let expected = if expiry == "transaction" { 1 } else { 2 };
            if value != expected { return Err(format!("{expiry} rollback state incorrect")); }
        }
        Ok(())
    }).await.unwrap_or_else(|_| Err("expiry phases timed out".into()));

    for task in pending {
        if !task.is_finished() {
            task.abort();
            let _ = task.await;
        }
    }
    for driver in drivers {
        driver.abort();
        let _ = driver.await;
    }
    let cleanup = timeout(Duration::from_secs(5), async {
        let (db, connection) = config.connect(NoTls).await?;
        let driver = tokio::spawn(connection);
        let result = db.batch_execute(&format!("SET lock_timeout = '3s'; SET statement_timeout = '4s'; DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
        drop(db);
        driver.abort();
        let _ = driver.await;
        result
    }).await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "fixture cleanup failed: {cleanup:?}; phases: {result:?}"
    );
    result.expect("expiry phases failed");
}

#[test]
fn coupled_sql_relation_is_transformed_only_for_the_trusted_schema() {
    let [lock, lookup, write] = billing_transaction_fixture::fixed_sql("billing_test_1_2");
    assert!(lock.contains("billing_test_1_2.subscriptions IN SHARE ROW EXCLUSIVE"));
    assert!(lookup.contains("FROM billing_test_1_2.subscriptions WHERE"));
    assert!(write.starts_with("INSERT INTO billing_test_1_2.subscriptions "));
    assert!(
        std::panic::catch_unwind(|| billing_transaction_fixture::fixed_sql("x; DROP")).is_err()
    );
}

// A raw writer does not take the fixture's advisory lock. Its uncommitted
// insert must finish before the fixture can classify the global target.
#[tokio::test]
async fn coupled_fixture_waits_for_raw_insert_commit_or_rollback() {
    use std::time::Duration;
    use tokio::time::timeout;

    let fx = Fx::new().await;
    fx.add_user("u1").await;
    fx.add_user("u2").await;
    let config = disposable_config(&std::env::var("NNTUNE_TEST_DATABASE_URL").unwrap()).unwrap();
    let mut outcome = fx
        .fold("u1", &remote("sub_raw", &[invoice("sub_raw", 1)]))
        .await;
    fx.db
        .batch_execute("SET lock_timeout = '4s'; SET statement_timeout = '5s'")
        .await
        .unwrap();
    outcome.explicit_stale = true;
    outcome.history.clear();
    let mut drivers = Vec::new();
    let mut pending = None;
    let result: Result<(), String> = timeout(Duration::from_secs(30), async {
        let (writer, connection) = config.connect(NoTls).await.map_err(|e| e.to_string())?;
        drivers.push(tokio::spawn(connection));
        let (mut runner, connection) = config.connect(NoTls).await.map_err(|e| e.to_string())?;
        drivers.push(tokio::spawn(connection));
        let writer_pid: i32 = writer.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
        let runner_pid: i32 = runner.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
        runner.batch_execute(&format!("SET search_path TO {}; SET lock_timeout = '4s'; SET statement_timeout = '5s'; SET transaction_timeout = '10s'", fx.schema)).await.map_err(|e| e.to_string())?;
        writer.batch_execute(&format!("SET search_path TO {}; SET statement_timeout = '5s'", fx.schema)).await.map_err(|e| e.to_string())?;
        for commit in [false, true] {
            writer.batch_execute("BEGIN; INSERT INTO subscriptions (user_id, stripe_subscription_id, stripe_customer_id, stripe_subscription_item_id, stripe_price_id, plan_id, status) VALUES ('u2', 'sub_raw', 'cus_a', 'si_raw', 'price_plus_test', 'plus', 'canceled')").await.map_err(|e| e.to_string())?;
            let (phase_tx, phase_rx) = billing_contention::phase();
            let schema = fx.schema.clone();
            let input = outcome.clone();
            pending = Some(tokio::spawn(async move {
                let _ = phase_tx.send(());
                let result = billing_transaction_fixture::run(&mut runner, &schema, "u1", &input).await;
                (runner, result)
            }));
            billing_contention::reached(phase_rx).await?;
            let wait = billing_contention::relation_wait(&fx.db, pending.as_mut().unwrap(), writer_pid, runner_pid, &format!("{}.subscriptions", fx.schema), "ShareRowExclusiveLock").await;
            if let Err(error) = wait {
                if error == "lock finished before wait barrier" { pending = None; }
                return Err(error);
            }
            writer.batch_execute(if commit { "COMMIT" } else { "ROLLBACK" }).await.map_err(|e| e.to_string())?;
            let completed = timeout(Duration::from_secs(5), pending.as_mut().unwrap()).await.map_err(|_| "fixture did not resume".to_string())?;
            pending = None;
            let (next_runner, classification) = completed.map_err(|e| e.to_string())?;
            runner = next_runner;
            if commit {
                if !matches!(&classification, Err(billing_transaction_fixture::FixtureError::Refusal(error)) if error == "foreign or mismatched target") { return Err(format!("committed foreign insert: {classification:?}")); }
                if fx.snap("sub_raw").await.map(|row| row.user_id).as_deref() != Some("u2") { return Err("committed foreign row changed".into()); }
            } else if !matches!(&classification, Ok(billing_transaction_fixture::Class::AbsentEmpty)) || fx.snap("sub_raw").await.is_some() {
                return Err(format!("rolled-back insert: {classification:?}"));
            }
            if fx.history_rows("sub_raw").await != 0 { return Err("unexpected history".into()); }
        }
        Ok(())
    }).await.unwrap_or_else(|_| Err("contention phases timed out".into()));
    if let Some(task) = pending {
        task.abort();
        let _ = task.await;
    }
    for driver in drivers {
        driver.abort();
        let _ = driver.await;
    }
    fx.cleanup().await;
    result.expect("raw writer exclusion failed");
}

#[tokio::test]
async fn coupled_transaction_matrix_and_rollback_on_disposable_schema() {
    use billing_transaction_fixture::{run, Class};
    let mut fx = Fx::new().await;
    fx.add_user("u1").await;
    fx.add_user("u2").await;
    let schema = fx.schema.clone();
    // Non-explicit guarded upsert: exact RETURNING, then an equal-end replay.
    let first = fx
        .fold(
            "u1",
            &remote("sub_a", &[invoice("sub_a", 1), invoice("sub_a", 2)]),
        )
        .await;
    assert!(!first.explicit_stale);
    assert!(matches!(
        run(&mut fx.db, &schema, "u1", &first).await,
        Ok(Class::Wrote)
    ));
    let stored = fx.snap("sub_a").await.unwrap();
    assert_eq!(fx.history_rows("sub_a").await, 2);
    assert!(matches!(
        run(&mut fx.db, &schema, "u1", &first).await,
        Ok(Class::Wrote)
    ));
    assert_eq!(fx.snap("sub_a").await, Some(stored.clone()));

    let mut stale = fx
        .fold("u1", &remote("sub_a", &[invoice("sub_a", 1)]))
        .await;
    assert!(stale.explicit_stale);
    let mut extra = stale.history[0].clone();
    extra.stripe_invoice_id = "inv_extra".into();
    stale.history.push(extra);
    // Explicit stale: own succeeds and keeps history; the row never changes.
    assert!(matches!(
        run(&mut fx.db, &schema, "u1", &stale).await,
        Ok(Class::OwnStale)
    ));
    assert_eq!(fx.snap("sub_a").await, Some(stored.clone()));
    assert_eq!(fx.history_rows("sub_a").await, 3);
    // Foreign owner rejects whatever the history, and leaves no history.
    let mut foreign = stale.clone();
    foreign.history[1].stripe_invoice_id = "inv_foreign".into();
    for history_empty in [false, true] {
        if history_empty {
            foreign.history.clear();
        }
        assert!(matches!(run(&mut fx.db, &schema, "u2", &foreign).await,
            Err(billing_transaction_fixture::FixtureError::Refusal(message)) if message == "foreign or mismatched target"));
        assert_eq!(fx.history_rows("sub_a").await, 3);
        assert_eq!(fx.snap("sub_a").await, Some(stored.clone()));
    }
    let mut unflagged_foreign = stale.clone();
    unflagged_foreign.explicit_stale = false;
    assert!(
        matches!(run(&mut fx.db, &schema, "u2", &unflagged_foreign).await,
        Err(billing_transaction_fixture::FixtureError::Refusal(message)) if message == "foreign or mismatched target")
    );
    assert_eq!(fx.history_rows("sub_a").await, 3);
    // Absent: empty explicit stale succeeds without a row; with history rejects.
    let mut absent = stale.clone();
    absent.row.stripe_subscription_id = "sub_missing".into();
    for h in &mut absent.history {
        h.stripe_subscription_id = "sub_missing".into();
    }
    absent.history[1].stripe_invoice_id = "inv_absent".into();
    assert!(matches!(run(&mut fx.db, &schema, "u1", &absent).await,
        Err(billing_transaction_fixture::FixtureError::Refusal(message)) if message == "absent row with history rejected"));
    assert_eq!(fx.history_rows("sub_a").await, 3);
    absent.history.clear();
    assert!(matches!(
        run(&mut fx.db, &schema, "u1", &absent).await,
        Ok(Class::AbsentEmpty)
    ));
    assert!(fx.snap("sub_missing").await.is_none());
    // Zero rows from a non-explicit write are recognized only as own PG-stale.
    let mut unflagged = stale.clone();
    unflagged.explicit_stale = false;
    assert!(matches!(
        run(&mut fx.db, &schema, "u1", &unflagged).await,
        Ok(Class::OwnStale)
    ));
    assert_eq!(fx.snap("sub_a").await, Some(stored.clone()));
    // A history SQL failure after a successful write rolls the write back.
    let mut failing = fx
        .fold("u2", &remote("sub_r", &[invoice("sub_r", 1)]))
        .await;
    failing.history[0].plan_id = "missing_plan".into();
    let failure = run(&mut fx.db, &schema, "u2", &failing).await.unwrap_err();
    match failure {
        billing_transaction_fixture::FixtureError::Postgres(error) => {
            let db_error = error.as_db_error().expect("database error");
            assert_eq!(
                db_error.code(),
                &tokio_postgres::error::SqlState::FOREIGN_KEY_VIOLATION
            );
            assert!(db_error.message().contains("foreign key"));
        }
        other => panic!("expected PostgreSQL error, got {other:?}"),
    }
    assert!(fx.snap("sub_r").await.is_none());
    assert_eq!(fx.history_rows("sub_r").await, 0);
    assert_eq!(fx.snap("sub_a").await, Some(stored));
    fx.cleanup().await;
}
