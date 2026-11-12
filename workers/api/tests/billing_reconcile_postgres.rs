//! Baseline reconciliation selection controls on disposable PostgreSQL.
#![cfg(feature = "postgres-tests")]

use nntune_api::billing_reconcile::{
    insert_paid_invoice_history_statement, reconcile_live_select_statement,
    reconcile_lock_statement, reconcile_subscription, store_reconciled_subscription_statement,
    LiveRow, PriceBinding, ReconcileInput, ReconcileOutcome, RemoteSubscription,
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

#[derive(Debug, PartialEq, Eq)]
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
        self.db
            .query_opt(reconcile_live_select_statement(), &[&user])
            .await
            .unwrap()
            .map(|r| LiveRow {
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
        reconcile_subscription(&input, remote, T)
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
                ],
            )
            .await?;
        let r = &outcome.row;
        let params: [&(dyn ToSql + Sync); 14] = [
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
