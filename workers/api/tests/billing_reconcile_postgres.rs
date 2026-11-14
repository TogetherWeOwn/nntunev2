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
            expected.status = "canceled".into();
            expected.grace = Some(10);
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
