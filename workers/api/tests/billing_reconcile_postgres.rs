//! Production subscription-store statement on CI's disposable PostgreSQL.
//! Run with --features postgres-tests --test billing_reconcile_postgres.
//! Missing test configuration fails; ordinary unit tests do not require a
//! database. Fixtures come from the public reconcile fold and the real
//! migrations; the statement under test is the production one, bound with
//! the Worker's positional parameter types, never a copy of its SQL.
#![cfg(feature = "postgres-tests")]

use nntune_api::billing::{resolve_effective_plan, SubscriptionView};
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
const P: i64 = 2_592_000;
const GRACE: i64 = 7 * 24 * 3_600;
const CUS: &str = "cus_a";
const PRICE_PLUS: &str = "price_plus_test";
const PRICE_PRO: &str = "price_pro_test";

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

/// The stored columns a reconcile write can move, epochs as seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        let fx = Self { db };
        fx.add_user("u1").await;
        fx
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

    /// A row written straight to the table, status verbatim.
    async fn raw(&self, user: &str, sub: &str, status: &str, paid_through: Option<i64>) {
        self.db
            .execute(
                "INSERT INTO subscriptions (user_id, stripe_subscription_id, stripe_customer_id, stripe_subscription_item_id, stripe_price_id, plan_id, status, paid_through_at, paid_invoice_id) VALUES ($1,$2,$3,'si_raw',$4,'plus',$5,to_timestamp($6::bigint),$7)",
                &[
                    &user,
                    &sub,
                    &CUS,
                    &PRICE_PLUS,
                    &status,
                    &paid_through,
                    &paid_through.map(|_| format!("inv_raw_{sub}")),
                ],
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
                user_id: r.get(0),
                customer: r.get(1),
                item: r.get(2),
                price: r.get(3),
                plan: r.get(4),
                status: r.get(5),
                period_end: r.get(6),
                cancel: r.get(7),
                paid_through: r.get(8),
                anchor: r.get(9),
                grace: r.get(10),
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

    /// The stored live row as the production read path loads it.
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

    /// Fold against what the store holds now.
    async fn fold(&self, user: &str, remote: &RemoteSubscription) -> ReconcileOutcome {
        fold_with(self.live(user).await, remote)
    }

    /// The Worker's transaction: per-user lock, positional typed write,
    /// then history. A failing statement rolls the whole transaction back
    /// and is returned, so tests can assert the exact SQLSTATE.
    async fn try_put(
        &mut self,
        user: &str,
        outcome: &ReconcileOutcome,
    ) -> Result<(), tokio_postgres::Error> {
        let tx = self.db.transaction().await?;
        let written: Result<(), tokio_postgres::Error> = async {
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
                &outcome.stale_snapshot,
            ];
            tx.execute(&stmt, &params).await?;
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
            Ok(())
        }
        .await;
        match written {
            Ok(()) => tx.commit().await,
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    /// `try_put` for writes that must succeed.
    async fn put(&mut self, user: &str, outcome: &ReconcileOutcome) {
        self.try_put(user, outcome)
            .await
            .expect("reconcile write must commit");
    }

    /// Stored active subscription `sub` with invoices 1 and 2 folded
    /// through the public reconcile: plus, covered to T+2P, anchored inv2.
    async fn seed_active(&mut self, user: &str, sub: &str) -> Snap {
        let remote = remote(
            sub,
            "active",
            PRICE_PLUS,
            false,
            T + 2 * P,
            &invoices(sub, 2),
        );
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
}

fn bindings() -> Vec<PriceBinding> {
    [(PRICE_PLUS, "plus"), (PRICE_PRO, "pro")]
        .into_iter()
        .map(|(price, plan)| PriceBinding {
            price_id: price.into(),
            plan_id: plan.into(),
            selectable: true,
            livemode: false,
        })
        .collect()
}

fn fold_with(live: Option<LiveRow>, remote: &RemoteSubscription) -> ReconcileOutcome {
    let input = ReconcileInput {
        stripe_customer_id: Some(CUS.into()),
        live,
        bindings: bindings(),
    };
    reconcile_subscription(&input, remote, T)
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
                price: Some(PRICE_PLUS.into()),
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

/// Invoices 1..=count, oldest first.
fn invoices(sub: &str, count: i64) -> Vec<Invoice> {
    (1..=count).map(|n| invoice(sub, n)).collect()
}

fn remote(
    sub: &str,
    status: &str,
    price: &str,
    cancel: bool,
    period_end: i64,
    invoices: &[Invoice],
) -> RemoteSubscription {
    RemoteSubscription {
        subscription: Subscription {
            id: sub.into(),
            customer: Some(CUS.into()),
            status: status.into(),
            cancel_at_period_end: cancel,
            current_period_end: Some(period_end),
            items: Some(SubscriptionItems {
                data: vec![SubscriptionItem {
                    id: "si_a".into(),
                    price: Some(price.into()),
                }],
            }),
            livemode: false,
        },
        invoices: invoices.to_vec(),
    }
}

/// An outcome whose every mutable column differs from the seeded row.
fn divergent(sub: &str, stored: &Snap) -> ReconcileOutcome {
    let mut o = fold_with(
        Some(LiveRow {
            stripe_subscription_id: sub.into(),
            stripe_customer_id: stored.customer.clone(),
            stripe_item_id: stored.item.clone(),
            stripe_price_id: stored.price.clone(),
            plan_id: stored.plan.clone(),
            status: stored.status.clone(),
            paid_through_unix: stored.paid_through,
            paid_invoice_id: stored.anchor.clone(),
            grace_until_unix: stored.grace,
        }),
        &remote(sub, "active", PRICE_PLUS, false, T, &invoices(sub, 2)),
    );
    o.row.stripe_customer_id = "cus_other".into();
    o.row.stripe_item_id = "si_other".into();
    o.row.stripe_price_id = PRICE_PRO.into();
    o.row.plan_id = "pro".into();
    o.row.status = "past_due".into();
    o.row.current_period_end_unix = Some(T + 9);
    o.row.cancel_at_period_end = !stored.cancel;
    o.row.grace_until_unix = Some(T + 99);
    o.read_plan = Some(stored.plan.clone());
    o.read_anchor = stored.anchor.clone();
    o.stale_snapshot = false;
    o
}

#[tokio::test]
async fn stale_and_fresh_commit_orders_converge() {
    // Both passes read the same stored state; only the commit order differs.
    let mut finals = Vec::new();
    for stale_first in [true, false] {
        let mut fx = Fx::new().await;
        fx.seed_active("u1", "sub_a").await;
        let fresh_remote = remote(
            "sub_a",
            "past_due",
            PRICE_PLUS,
            true,
            T + 3 * P,
            &invoices("sub_a", 2),
        );
        // An older retrieval that predates the stored anchor.
        let stale_remote = remote(
            "sub_a",
            "active",
            PRICE_PLUS,
            false,
            T + P,
            &invoices("sub_a", 1),
        );
        let fresh = fx.fold("u1", &fresh_remote).await;
        let stale = fx.fold("u1", &stale_remote).await;
        assert!(!fresh.stale_snapshot, "fresh snapshot");
        assert!(stale.stale_snapshot, "snapshot missing the stored anchor");
        if stale_first {
            fx.put("u1", &stale).await;
            fx.put("u1", &fresh).await;
        } else {
            fx.put("u1", &fresh).await;
            fx.put("u1", &stale).await;
        }
        let snap = fx.snap("sub_a").await.unwrap();
        assert_eq!(snap.status, "past_due");
        assert!(snap.cancel, "fresh cancellation survives a stale commit");
        assert_eq!(snap.period_end, Some(T + 3 * P));
        assert_eq!(snap.grace, Some(T + 2 * P + GRACE));
        assert_eq!(snap.paid_through, Some(T + 2 * P));
        assert_eq!(snap.anchor.as_deref(), Some("inv_sub_a_2"));
        finals.push(snap);
    }
    assert_eq!(finals[0], finals[1], "commit order must not matter");
}

#[tokio::test]
async fn explicit_stale_suppresses_every_update_whatever_the_evidence_end() {
    let mut fx = Fx::new().await;
    let stored = fx.seed_active("u1", "sub_a").await;
    let ends = [
        Some(T + P),
        Some(stored.paid_through.unwrap()),
        Some(T + 5 * P),
        None,
    ];
    for end in ends {
        for anchor in [None, Some("inv_new".to_string())] {
            let mut o = divergent("sub_a", &stored);
            o.row.paid_through_unix = end;
            o.row.paid_invoice_id = anchor.clone();
            o.stale_snapshot = true;
            fx.put("u1", &o).await;
            assert_eq!(
                fx.snap("sub_a").await.unwrap(),
                stored,
                "end {end:?} anchor {anchor:?}"
            );
        }
    }
}

#[tokio::test]
async fn predicate_stale_without_the_flag_keeps_bindings_and_mutable_fields() {
    let mut fx = Fx::new().await;
    let stored = fx.seed_active("u1", "sub_a").await;
    // Strictly lower incoming end, and anchor-less equal end against the
    // anchored row, both read as stale at commit time.
    for (end, anchor) in [(T + P, Some("inv_old")), (T + 2 * P, None)] {
        let mut o = divergent("sub_a", &stored);
        o.row.paid_through_unix = Some(end);
        o.row.paid_invoice_id = anchor.map(str::to_string);
        fx.put("u1", &o).await;
        assert_eq!(fx.snap("sub_a").await.unwrap(), stored, "end {end}");
    }
}

#[tokio::test]
async fn cancellation_follows_the_fresh_pass_and_binding_is_stable_under_stale() {
    let mut fx = Fx::new().await;
    let stored = fx.seed_active("u1", "sub_a").await;
    let cancel = fx
        .fold(
            "u1",
            &remote(
                "sub_a",
                "active",
                PRICE_PLUS,
                true,
                T + 2 * P,
                &invoices("sub_a", 2),
            ),
        )
        .await;
    assert!(!cancel.stale_snapshot);
    fx.put("u1", &cancel).await;
    let cancelled = fx.snap("sub_a").await.unwrap();
    assert!(cancelled.cancel);

    // A stale pass carrying cancel=false and other bindings cannot undo it.
    let mut stale = divergent("sub_a", &cancelled);
    stale.row.cancel_at_period_end = false;
    stale.row.paid_through_unix = Some(T + P);
    stale.stale_snapshot = true;
    fx.put("u1", &stale).await;
    assert_eq!(fx.snap("sub_a").await.unwrap(), cancelled);
    assert_eq!(cancelled.item, stored.item);
    assert_eq!(cancelled.price, stored.price);

    // A later fresh pass still clears it.
    let clear = fx
        .fold(
            "u1",
            &remote(
                "sub_a",
                "active",
                PRICE_PLUS,
                false,
                T + 2 * P,
                &invoices("sub_a", 2),
            ),
        )
        .await;
    fx.put("u1", &clear).await;
    assert!(!fx.snap("sub_a").await.unwrap().cancel);
}

#[tokio::test]
async fn fresh_same_subscription_controls_still_apply() {
    // Ordinary paid renewal, with an unrelated terminal row in the history.
    let mut fx = Fx::new().await;
    fx.raw("u1", "sub_old", "canceled", Some(T)).await;
    fx.seed_active("u1", "sub_a").await;
    let renewal = fx
        .fold(
            "u1",
            &remote(
                "sub_a",
                "active",
                PRICE_PLUS,
                false,
                T + 3 * P,
                &invoices("sub_a", 3),
            ),
        )
        .await;
    assert!(!renewal.stale_snapshot);
    fx.put("u1", &renewal).await;
    let s = fx.snap("sub_a").await.unwrap();
    assert_eq!(s.paid_through, Some(T + 3 * P));
    assert_eq!(s.anchor.as_deref(), Some("inv_sub_a_3"));
    assert_eq!((s.status.as_str(), s.grace), ("active", None));
    assert_eq!(fx.history_rows("sub_a").await, 3);

    // Fresh held status: recorded, coverage and anchor untouched.
    let held = fx
        .fold(
            "u1",
            &remote(
                "sub_a",
                "paused",
                PRICE_PLUS,
                true,
                T + 4 * P,
                &invoices("sub_a", 3),
            ),
        )
        .await;
    assert!(!held.stale_snapshot);
    fx.put("u1", &held).await;
    let h = fx.snap("sub_a").await.unwrap();
    assert_eq!(h.status, "paused");
    assert!(h.cancel);
    assert_eq!(h.period_end, Some(T + 4 * P));
    assert_eq!((h.paid_through, h.anchor), (s.paid_through, s.anchor));

    // Fresh terminal status: recorded, grace cleared, slot released.
    let terminal = fx
        .fold(
            "u1",
            &remote(
                "sub_a",
                "canceled",
                PRICE_PLUS,
                false,
                T + 4 * P,
                &invoices("sub_a", 3),
            ),
        )
        .await;
    assert!(!terminal.stale_snapshot);
    fx.put("u1", &terminal).await;
    let t = fx.snap("sub_a").await.unwrap();
    assert_eq!((t.status.as_str(), t.grace), ("canceled", None));
    assert_eq!(t.paid_through, s.paid_through);
    assert!(fx.live("u1").await.is_none());
}

#[tokio::test]
async fn null_evidence_updates_status_and_bindings_but_never_coverage() {
    let mut fx = Fx::new().await;
    let stored = fx.seed_active("u1", "sub_a").await;
    let mut o = divergent("sub_a", &stored);
    o.row.paid_through_unix = None;
    o.row.paid_invoice_id = None;
    fx.put("u1", &o).await;
    let s = fx.snap("sub_a").await.unwrap();
    assert_eq!(s.status, "past_due");
    assert_eq!(s.customer, "cus_other");
    assert_eq!(s.item, "si_other");
    assert_eq!(s.price, PRICE_PRO);
    assert_eq!(s.period_end, Some(T + 9));
    assert_eq!(s.cancel, !stored.cancel);
    assert_eq!(s.grace, Some(T + 99));
    assert_eq!(s.plan, stored.plan);
    assert_eq!(s.paid_through, stored.paid_through);
    assert_eq!(s.anchor, stored.anchor);

    // The same NULL-evidence shape with the explicit flag changes nothing.
    let mut fx = Fx::new().await;
    let stored = fx.seed_active("u1", "sub_a").await;
    let mut o = divergent("sub_a", &stored);
    o.row.paid_through_unix = None;
    o.row.paid_invoice_id = None;
    o.stale_snapshot = true;
    fx.put("u1", &o).await;
    assert_eq!(fx.snap("sub_a").await.unwrap(), stored);
}

#[tokio::test]
async fn terminal_subscription_is_not_resurrected_beside_a_live_one() {
    let mut fx = Fx::new().await;
    fx.seed_active("u1", "sub_a").await;
    let end_a = fx
        .fold(
            "u1",
            &remote(
                "sub_a",
                "canceled",
                PRICE_PLUS,
                false,
                T + 2 * P,
                &invoices("sub_a", 2),
            ),
        )
        .await;
    fx.put("u1", &end_a).await;
    let terminal_a = fx.snap("sub_a").await.unwrap();
    assert_eq!(terminal_a.status, "canceled");

    // Replacement B takes the slot.
    let b = fx
        .fold(
            "u1",
            &remote(
                "sub_b",
                "active",
                PRICE_PLUS,
                false,
                T + 9 * P,
                &invoices("sub_b", 1),
            ),
        )
        .await;
    fx.put("u1", &b).await;
    let live_b = fx.snap("sub_b").await.unwrap();
    assert_eq!(live_b.status, "active");

    // A late pass for A carries evidence beyond A's stored end. It folded
    // before B existed (no live row), so it advances A's fields; the
    // commit must neither revive A nor trip the live-slot unique index.
    let late_a = fold_with(
        None,
        &remote(
            "sub_a",
            "active",
            PRICE_PLUS,
            false,
            T + 9 * P,
            &invoices("sub_a", 6),
        ),
    );
    assert!(late_a.row.paid_through_unix.unwrap() > terminal_a.paid_through.unwrap());
    fx.put("u1", &late_a).await;
    assert_eq!(fx.snap("sub_a").await.unwrap(), terminal_a);
    assert_eq!(fx.snap("sub_b").await.unwrap(), live_b);
    // History is separate: the paid invoices still land.
    assert_eq!(fx.history_rows("sub_a").await, 6);
    assert_eq!(fx.live("u1").await.unwrap().stripe_subscription_id, "sub_b");

    // Control: with no other live holder the same pass is a legitimate update.
    let end_b = fx
        .fold(
            "u1",
            &remote(
                "sub_b",
                "canceled",
                PRICE_PLUS,
                false,
                T + 9 * P,
                &invoices("sub_b", 1),
            ),
        )
        .await;
    fx.put("u1", &end_b).await;
    fx.put("u1", &late_a).await;
    assert_eq!(fx.snap("sub_a").await.unwrap().status, "active");
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
        let outcome = fold_with(
            None,
            &remote(
                &newcomer,
                "active",
                PRICE_PLUS,
                false,
                T + P,
                &invoices(&newcomer, 1),
            ),
        );
        let held = live.contains(status);
        let result = fx.try_put(&user, &outcome).await;
        if held {
            // A competing live holder surfaces as a retryable unique
            // violation; the transaction (history included) rolls back.
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

    // Another user's live slot never blocks this user.
    fx.add_user("other").await;
    fx.raw("other", "sub_other", "active", None).await;
    fx.add_user("free_user").await;
    let outcome = fold_with(
        None,
        &remote(
            "sub_free",
            "active",
            PRICE_PLUS,
            false,
            T + P,
            &invoices("sub_free", 1),
        ),
    );
    fx.put("free_user", &outcome).await;
    assert!(fx.snap("sub_free").await.is_some());
}

#[tokio::test]
async fn equal_end_plan_switch_converges_in_both_commit_orders() {
    let mut finals = Vec::new();
    for held_first in [true, false] {
        let mut fx = Fx::new().await;
        let seed = fx.seed_active("u1", "sub_a").await;
        // Both passes read the same seed: Plus, end E, anchor I1.
        let held = fx
            .fold(
                "u1",
                &remote(
                    "sub_a",
                    "paused",
                    PRICE_PLUS,
                    false,
                    T + 2 * P + 1,
                    &invoices("sub_a", 2),
                ),
            )
            .await;
        // Newer snapshot: I1 plus a same-end Pro invoice paid later.
        let mut pro = invoice("sub_a", 2);
        pro.id = "inv_sub_a_pro".into();
        pro.status_transitions = Some(InvoiceStatusTransitions {
            paid_at: Some(T + P + 10),
        });
        pro.lines.as_mut().unwrap().data[0].price = Some(PRICE_PRO.into());
        let mut newer_invoices = invoices("sub_a", 2);
        newer_invoices.push(pro);
        let active = fx
            .fold(
                "u1",
                &remote(
                    "sub_a",
                    "active",
                    PRICE_PRO,
                    true,
                    T + 3 * P,
                    &newer_invoices,
                ),
            )
            .await;
        assert_eq!(held.read_anchor, seed.anchor);
        assert_eq!(held.read_plan.as_deref(), Some("plus"));
        assert_eq!(active.read_anchor, seed.anchor);
        assert_eq!(active.row.plan_id, "pro");
        assert_eq!(active.row.paid_invoice_id.as_deref(), Some("inv_sub_a_pro"));
        assert!(!held.stale_snapshot && !active.stale_snapshot);
        if held_first {
            fx.put("u1", &held).await;
            fx.put("u1", &active).await;
        } else {
            fx.put("u1", &active).await;
            // Same end, but the stored plan/anchor moved since this read.
            fx.put("u1", &held).await;
        }
        let snap = fx.snap("sub_a").await.unwrap();
        assert_eq!(snap.plan, "pro");
        assert_eq!(snap.anchor.as_deref(), Some("inv_sub_a_pro"));
        assert_eq!(snap.price, PRICE_PRO);
        assert_eq!(snap.status, "active");
        assert!(snap.cancel);
        assert_eq!(snap.period_end, Some(T + 3 * P));
        assert_eq!(snap.paid_through, Some(T + 2 * P));
        assert_eq!(snap.grace, None);
        // Exact paid cutoff: entitled strictly before the end, and with a
        // scheduled cancellation closed at the end.
        let view = |now| {
            resolve_effective_plan(
                Some(SubscriptionView {
                    plan_id: &snap.plan,
                    status: &snap.status,
                    cancel_at_period_end: snap.cancel,
                    paid_through_at_unix: snap.paid_through,
                    grace_until_unix: snap.grace,
                    current_period_end_unix: snap.period_end,
                }),
                now,
            )
        };
        let end = T + 2 * P;
        assert!(view(end - 1).entitled);
        assert!(!view(end).entitled);
        finals.push(snap);
    }
    assert_eq!(finals[0], finals[1], "commit order must not matter");
}

#[tokio::test]
async fn trialing_row_recovers_to_active_and_past_due_on_the_same_paid_evidence() {
    // `trialing` is fail-closed and sits outside the live select, so the
    // recovery pass folds with no live row: read_plan/read_anchor are NULL
    // while the stored row still holds the same paid end and anchor.
    for status in ["active", "past_due"] {
        let mut fx = Fx::new().await;
        let seed = fx.seed_active("u1", "sub_a").await;
        if status == "past_due" {
            // Stored grace comes from a first failed renewal on the paid end.
            let due = fx
                .fold(
                    "u1",
                    &remote(
                        "sub_a",
                        status,
                        PRICE_PLUS,
                        false,
                        T + 2 * P,
                        &invoices("sub_a", 2),
                    ),
                )
                .await;
            fx.put("u1", &due).await;
            assert!(fx.snap("sub_a").await.unwrap().grace.is_some());
        }
        let trial = fx
            .fold(
                "u1",
                &remote(
                    "sub_a",
                    "trialing",
                    PRICE_PRO,
                    false,
                    T + P + 1,
                    &invoices("sub_a", 2),
                ),
            )
            .await;
        assert!(trial.fail_closed.is_some());
        fx.put("u1", &trial).await;
        let trialing = fx.snap("sub_a").await.unwrap();
        assert_eq!(trialing.status, "trialing");
        assert_eq!(trialing.price, PRICE_PRO);
        assert_eq!(trialing.paid_through, seed.paid_through);
        assert_eq!(trialing.anchor, seed.anchor);
        assert!(
            fx.live("u1").await.is_none(),
            "trialing is outside the live select"
        );

        let back = fx
            .fold(
                "u1",
                &remote(
                    "sub_a",
                    status,
                    PRICE_PLUS,
                    true,
                    T + 3 * P,
                    &invoices("sub_a", 2),
                ),
            )
            .await;
        assert_eq!(
            (back.read_plan.as_deref(), back.read_anchor.as_deref()),
            (None, None)
        );
        assert_eq!(back.row.paid_through_unix, seed.paid_through);
        assert_eq!(back.row.paid_invoice_id, seed.anchor);
        fx.put("u1", &back).await;

        // Static red expectation: the equal-end read-mismatch guard treats
        // NULL read_plan as "moved since read" and keeps the trialing fields.
        let snap = fx.snap("sub_a").await.unwrap();
        assert_eq!(snap.status, status, "{status}");
        assert_eq!(snap.price, PRICE_PLUS, "{status}");
        assert_eq!(snap.item, seed.item, "{status}");
        assert_eq!(snap.customer, seed.customer, "{status}");
        assert_eq!(snap.period_end, Some(T + 3 * P), "{status}");
        assert!(snap.cancel, "{status}");
        assert_eq!(snap.grace, back.row.grace_until_unix, "{status}");
        assert_eq!(snap.plan, "plus");
        assert_eq!(snap.paid_through, seed.paid_through);
        assert_eq!(snap.anchor, seed.anchor);
        let live = fx.live("u1").await.expect("recovered row is live again");
        assert_eq!(live.stripe_subscription_id, "sub_a");
        assert_eq!(live.status, status);
        assert_eq!(live.paid_invoice_id, seed.anchor);
    }
}

#[tokio::test]
async fn no_read_equal_end_outcome_preserves_live_and_terminal_state() {
    for status in ["active", "canceled", "incomplete_expired"] {
        let mut fx = Fx::new().await;
        fx.seed_active("u1", "sub_a").await;
        let delayed = fold_with(
            None,
            &remote(
                "sub_a",
                "active",
                PRICE_PLUS,
                false,
                T + 2 * P,
                &invoices("sub_a", 2),
            ),
        );
        let current = fx
            .fold(
                "u1",
                &remote(
                    "sub_a",
                    status,
                    PRICE_PLUS,
                    true,
                    T + 3 * P,
                    &invoices("sub_a", 2),
                ),
            )
            .await;
        fx.put("u1", &current).await;
        let stored = fx.snap("sub_a").await.unwrap();
        assert_eq!(stored.status, status);
        fx.put("u1", &delayed).await;
        assert_eq!(fx.snap("sub_a").await.unwrap(), stored, "{status}");
    }
}

#[tokio::test]
async fn no_read_equal_end_outcome_cannot_undo_a_committed_plan_switch() {
    // Control for the trialing recovery: an equal-end outcome that read no
    // live row (NULL read_plan/read_anchor) but whose stored row has since
    // switched plan/anchor must still be suppressed. Relaxing the guard for
    // a NULL read_plan alone would relabel the row back to Plus.
    let mut fx = Fx::new().await;
    fx.seed_active("u1", "sub_a").await;
    let delayed = fold_with(
        None,
        &remote(
            "sub_a",
            "active",
            PRICE_PLUS,
            true,
            T + 2 * P + 5,
            &invoices("sub_a", 2),
        ),
    );
    assert_eq!(
        (delayed.read_plan.as_deref(), delayed.read_anchor.as_deref()),
        (None, None)
    );
    assert_eq!(delayed.row.paid_through_unix, Some(T + 2 * P));

    let mut pro = invoice("sub_a", 2);
    pro.id = "inv_sub_a_pro".into();
    pro.status_transitions = Some(InvoiceStatusTransitions {
        paid_at: Some(T + P + 10),
    });
    pro.lines.as_mut().unwrap().data[0].price = Some(PRICE_PRO.into());
    let mut switched_invoices = invoices("sub_a", 2);
    switched_invoices.push(pro);
    let switched = fx
        .fold(
            "u1",
            &remote(
                "sub_a",
                "active",
                PRICE_PRO,
                false,
                T + 3 * P,
                &switched_invoices,
            ),
        )
        .await;
    fx.put("u1", &switched).await;
    let stored = fx.snap("sub_a").await.unwrap();
    assert_eq!(stored.plan, "pro");
    assert_eq!(stored.anchor.as_deref(), Some("inv_sub_a_pro"));
    assert_eq!(stored.paid_through, Some(T + 2 * P));

    fx.put("u1", &delayed).await;
    assert_eq!(fx.snap("sub_a").await.unwrap(), stored);
}

#[tokio::test]
async fn new_terminal_row_beside_a_live_one_commits_without_disturbing_it() {
    let mut fx = Fx::new().await;
    let live_b = fx.seed_active("u1", "sub_b").await;

    // Terminal A is not a live-slot holder: the row and its confirmed
    // history are accepted and B keeps the slot unchanged.
    let mut terminal_a = fold_with(
        None,
        &remote(
            "sub_a",
            "canceled",
            PRICE_PLUS,
            false,
            T + P,
            &invoices("sub_a", 1),
        ),
    );
    terminal_a.history = fold_with(
        None,
        &remote(
            "sub_a",
            "active",
            PRICE_PLUS,
            false,
            T + P,
            &invoices("sub_a", 2),
        ),
    )
    .history;
    assert_eq!(terminal_a.history.len(), 2);
    fx.put("u1", &terminal_a).await;
    assert_eq!(fx.snap("sub_a").await.unwrap().status, "canceled");
    assert_eq!(fx.history_rows("sub_a").await, 2);
    assert_eq!(fx.snap("sub_b").await.unwrap(), live_b);
    assert_eq!(fx.live("u1").await.unwrap().stripe_subscription_id, "sub_b");

    // Control: a genuinely new live C conflicts with B, retryably, and
    // rolls back its row and history.
    let live_c = fold_with(
        None,
        &remote(
            "sub_c",
            "active",
            PRICE_PLUS,
            false,
            T + P,
            &invoices("sub_c", 1),
        ),
    );
    let error = fx
        .try_put("u1", &live_c)
        .await
        .expect_err("live slot is held");
    assert_eq!(
        error.code(),
        Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
    );
    assert!(fx.snap("sub_c").await.is_none());
    assert_eq!(fx.history_rows("sub_c").await, 0);
    assert_eq!(fx.snap("sub_b").await.unwrap(), live_b);
    assert_eq!(fx.rows("u1").await, 2);
}
