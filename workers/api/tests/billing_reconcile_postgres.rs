//! Baseline reconciliation selection controls on disposable PostgreSQL.
#![cfg(feature = "postgres-tests")]

use nntune_api::billing_reconcile::{reconcile_live_select_statement, LiveRow};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_postgres::{Client, Config, NoTls};

static SCHEMA: AtomicUsize = AtomicUsize::new(0);
const T: i64 = 1_786_032_000;
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

    async fn cleanup(&self) {
        self.db
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .await
            .unwrap();
    }
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
