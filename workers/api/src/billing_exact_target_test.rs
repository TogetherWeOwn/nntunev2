//! Exact-target folds committed through the dormant Worker store on disposable PostgreSQL.
use super::{billing_budget, MirrorBinding, PgStore, ReconcileProbe, StoreError, CONNECTION};
use crate::billing_reconcile::{
    reconcile_exact_target, reconcile_target_select_statement, LiveRow, PriceBinding,
    ReconcileInput, RemoteSubscription,
};
use serde_json::json;

const USER: &str = "worker_fault_user";
const HOLDER: &str = "sub_worker_fault";
const TARGET: &str = "sub_worker_exact";
const PRICE: &str = "price_worker_fault";
const NOW: i64 = 1_786_032_001;
const END: i64 = 1_786_032_100;

#[derive(Debug, PartialEq, Eq)]
struct TargetState {
    row: Option<LiveRow>,
    subscription: Option<String>,
    history: String,
    history_count: i64,
}

async fn state(
    id: &'static str,
    status: Option<&'static str>,
) -> Result<TargetState, &'static str> {
    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |client| Box::pin(async move {
            if let Some(status) = status {
                let changed = client.execute(
                    "UPDATE public.subscriptions SET status = $1 WHERE user_id = $2 AND stripe_subscription_id = $3",
                    &[&status, &USER, &id],
                ).await.map_err(|_| StoreError)?;
                if changed != 1 { return Err(StoreError); }
            }
            let row = client.query_opt(reconcile_target_select_statement(), &[&USER, &id])
                .await.map_err(|_| StoreError)?.map(|row| LiveRow {
                    stripe_subscription_id: row.get("stripe_subscription_id"),
                    stripe_customer_id: row.get("stripe_customer_id"),
                    stripe_item_id: row.get("stripe_subscription_item_id"),
                    stripe_price_id: row.get("stripe_price_id"),
                    plan_id: row.get("plan_id"),
                    status: row.get("status"),
                    paid_through_unix: row.get("paid_through_unix"),
                    paid_invoice_id: row.get("paid_invoice_id"),
                    grace_until_unix: row.get("grace_until_unix"),
                });
            let snapshot = client.query_one(
                "SELECT (SELECT to_jsonb(s)::text FROM public.subscriptions s WHERE stripe_subscription_id = $1),
                        COALESCE(jsonb_agg(to_jsonb(h) ORDER BY stripe_invoice_id), '[]')::text, count(*)
                 FROM public.paid_invoice_history h WHERE stripe_subscription_id = $1",
                &[&id],
            ).await.map_err(|_| StoreError)?;
            Ok(TargetState { row, subscription: snapshot.get(0), history: snapshot.get(1), history_count: snapshot.get(2) })
        }),
        |_| {},
    ).await.map_err(|_| "exact-target fixture query failed")
}

fn remote(id: &str, item: &str, status: &str, invoices: &[(&str, &str)]) -> RemoteSubscription {
    let customer = "cus_worker_fault";
    RemoteSubscription {
        subscription: serde_json::from_value(json!({
            "id": id, "customer": customer, "status": status, "current_period_end": END,
            "items": { "data": [{ "id": item, "price": PRICE }] }
        })).expect("fixed subscription fixture"),
        invoices: invoices.iter().enumerate().map(|(order, (invoice, currency))| {
            serde_json::from_value(json!({
                "id": invoice, "customer": customer, "subscription": id, "status": "paid",
                "currency": currency, "amount_paid": 800, "status_transitions": { "paid_at": NOW + order as i64 },
                "lines": { "data": [{ "id": "il_exact_target", "amount": 800, "price": PRICE,
                    "period": { "start": NOW - 1, "end": END } }] }
            })).expect("fixed invoice fixture")
        }).collect(),
    }
}

fn input(row: Option<LiveRow>) -> ReconcileInput {
    ReconcileInput {
        stripe_customer_id: Some("cus_worker_fault".into()),
        live: row,
        bindings: vec![PriceBinding {
            price_id: PRICE.into(),
            plan_id: "plus".into(),
            selectable: true,
            livemode: false,
        }],
    }
}

pub(super) async fn check() -> Result<(), &'static str> {
    let store = PgStore {
        env: None,
        trace_id: "worker-exact-target-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: None,
        cancel_token_observer: None,
    };
    for status in ["past_due", "active"] {
        let archive = state(HOLDER, Some("trialing")).await?;
        let Some(row) = archive.row.clone() else {
            return Err("archived target was not selected");
        };
        let mut listing = remote(HOLDER, "si_worker_fault", status, &[]);
        let stale = reconcile_exact_target(input(Some(row.clone())), &listing, NOW);
        if !stale.explicit_stale || stale.read_anchor.as_deref() != Some("in_worker_second") {
            return Err("missing archive anchor did not produce an explicit stale fold");
        }
        store
            .store_reconcile_owned(USER.into(), stale, ReconcileProbe::None)
            .await
            .map_err(|_| "archived target stale no-op failed")?;
        if archive != state(HOLDER, None).await? {
            return Err("missing archive anchor changed committed subscription or history");
        }
        listing.invoices = remote(
            HOLDER,
            "si_worker_fault",
            status,
            &[
                ("in_worker_absent", "usd"),
                ("in_worker_first", "usd"),
                ("in_worker_second", "eur"),
            ],
        )
        .invoices;
        let fresh = reconcile_exact_target(input(Some(row)), &listing, NOW);
        if fresh.explicit_stale
            || fresh.fail_closed.is_some()
            || fresh.read_anchor.as_deref() != Some("in_worker_second")
        {
            return Err("complete equal-end listing did not retain archive provenance");
        }
        store
            .store_reconcile_owned(USER.into(), fresh, ReconcileProbe::None)
            .await
            .map_err(|_| "equal-end archive recovery failed")?;
        let recovered = state(HOLDER, None).await?;
        let Some(row) = recovered.row else {
            return Err("recovered archive missing");
        };
        if row.status != status
            || row.plan_id != "plus"
            || row.paid_through_unix != Some(END)
            || row.paid_invoice_id.as_deref() != Some("in_worker_second")
            || row.grace_until_unix.is_some() != (status == "past_due")
            || recovered.history != archive.history
        {
            return Err(
                "archive recovery changed paid evidence or produced the wrong status/grace",
            );
        }
    }

    let holder = state(HOLDER, None).await?;
    let absent = state(TARGET, None).await?;
    let listing = remote(
        TARGET,
        "si_worker_exact",
        "active",
        &[("in_worker_exact", "usd")],
    );
    let outcome = reconcile_exact_target(input(absent.row.clone()), &listing, NOW);
    if absent.subscription.is_some()
        || absent.history_count != 0
        || holder.row.is_none()
        || outcome != reconcile_exact_target(input(holder.row.clone()), &listing, NOW)
        || outcome.explicit_stale
        || outcome.fail_closed.is_some()
        || outcome.read_anchor.is_some()
        || outcome.read_plan.is_some()
    {
        return Err("exact-target fold borrowed proof from the live slot holder");
    }
    if store
        .store_reconcile_owned(USER.into(), outcome.clone(), ReconcileProbe::None)
        .await
        .is_ok()
        || holder != state(HOLDER, None).await?
        || absent != state(TARGET, None).await?
    {
        return Err("occupied live slot did not refuse the target without writes");
    }
    let released = state(HOLDER, Some("canceled")).await?;
    store
        .store_reconcile_owned(USER.into(), outcome, ReconcileProbe::None)
        .await
        .map_err(|_| "same pre-release fold failed after live slot release")?;
    let committed = state(TARGET, None).await?;
    let Some(row) = committed.row else {
        return Err("target missing after release and commit");
    };
    let history: serde_json::Value =
        serde_json::from_str(&committed.history).map_err(|_| "invalid history snapshot")?;
    if released != state(HOLDER, None).await?
        || committed.history_count != 1
        || row.stripe_subscription_id != TARGET
        || row.stripe_item_id != "si_worker_exact"
        || row.stripe_price_id != PRICE
        || row.status != "active"
        || row.plan_id != "plus"
        || row.grace_until_unix.is_some()
        || row.paid_through_unix != Some(END)
        || row.paid_invoice_id.as_deref() != Some("in_worker_exact")
        || history[0]["user_id"] != USER
        || history[0]["stripe_subscription_id"] != TARGET
        || history[0]["stripe_invoice_id"] != "in_worker_exact"
    {
        return Err(
            "post-release commit changed the holder or failed to persist exact target state",
        );
    }
    Ok(())
}
