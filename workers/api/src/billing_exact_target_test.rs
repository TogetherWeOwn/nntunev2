//! Exact-target folds committed through the dormant Worker store on disposable PostgreSQL.
use super::{billing_budget, MirrorBinding, PgStore, ReconcileProbe, StoreError, CONNECTION};
use crate::billing_reconcile::{
    reconcile_exact_target, reconcile_live_select_statement, reconcile_target_select_statement,
    FailClosedReason, LiveRow, PriceBinding, ReconcileInput, RemoteSubscription,
};
use serde_json::json;

const USER: &str = "worker_fault_user";
const HOLDER: &str = "sub_worker_fault";
const TARGET: &str = "sub_worker_exact";
const TERMINAL: &str = "sub_worker_new_terminal";
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

async fn live_subscription_id() -> Result<String, &'static str> {
    billing_budget::with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        |client| {
            Box::pin(async move {
                client
                    .query_one(reconcile_live_select_statement(), &[&USER])
                    .await
                    .map_err(|_| StoreError)?
                    .try_get("stripe_subscription_id")
                    .map_err(|_| StoreError)
            })
        },
        |_| {},
    )
    .await
    .map_err(|_| "live projection fixture query failed")
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
    // A genuine new terminal target is not an existing-target suppression.
    let terminal_listing = remote(TERMINAL, "si_worker_terminal", "canceled", &[]);
    let mut terminal = reconcile_exact_target(input(holder.row.clone()), &terminal_listing, NOW);
    let terminal_absent = state(TERMINAL, None).await?;
    if terminal_absent.row.is_some()
        || terminal_absent.subscription.is_some()
        || terminal_absent.history_count != 0
        || terminal.explicit_stale
        || terminal.fail_closed.is_some()
        || terminal.read_plan.is_some()
        || terminal.read_anchor.is_some()
        || !terminal.history.is_empty()
    {
        return Err("new terminal fold did not start without target proof");
    }
    // The terminal fold emits no history itself. Supply independently folded,
    // attributed paid evidence to exercise the store's atomic history contract.
    let confirmed = remote(
        TERMINAL,
        "si_worker_terminal",
        "active",
        &[("in_worker_terminal", "usd")],
    );
    terminal.history = reconcile_exact_target(input(None), &confirmed, NOW).history;
    store
        .store_reconcile_owned(USER.into(), terminal.clone(), ReconcileProbe::None)
        .await
        .map_err(|_| "new terminal beside live holder was refused")?;
    let terminal_committed = state(TERMINAL, None).await?;
    let Some(row) = terminal_committed.row.clone() else {
        return Err("accepted new terminal target was not inserted");
    };
    let history: serde_json::Value = serde_json::from_str(&terminal_committed.history)
        .map_err(|_| "invalid terminal history snapshot")?;
    if terminal_committed.subscription.is_none()
        || row.status != "canceled"
        || row.stripe_item_id != "si_worker_terminal"
        || row.stripe_price_id != PRICE
        || row.plan_id != "free"
        || row.paid_through_unix.is_some()
        || row.paid_invoice_id.is_some()
        || row.grace_until_unix.is_some()
        || terminal_committed.history_count != 1
        || history[0]["user_id"] != USER
        || history[0]["stripe_subscription_id"] != TERMINAL
        || history[0]["stripe_invoice_id"] != "in_worker_terminal"
        || holder != state(HOLDER, None).await?
    {
        return Err("new terminal commit changed holder, coverage, or history attribution");
    }
    let mut replay = reconcile_exact_target(input(Some(row)), &terminal_listing, NOW);
    replay.history = terminal.history;
    store
        .store_reconcile_owned(USER.into(), replay, ReconcileProbe::None)
        .await
        .map_err(|_| "new terminal replay with exact read proof was refused")?;
    if state(TERMINAL, None).await? != terminal_committed || state(HOLDER, None).await? != holder {
        return Err("new terminal replay changed committed subscription or history");
    }
    if live_subscription_id().await? != HOLDER {
        return Err("new terminal displaced the live projection");
    }

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
        || outcome.row.paid_invoice_id.as_deref() != Some("in_worker_exact")
        || outcome.history.len() != 1
        || outcome.history[0].stripe_invoice_id != "in_worker_exact"
    {
        return Err("genuine new paid fold did not produce its own invoice and absent proof");
    }
    if store
        .store_reconcile_owned(USER.into(), outcome.clone(), ReconcileProbe::None)
        .await
        .is_ok()
        || holder != state(HOLDER, None).await?
        || absent != state(TARGET, None).await?
        || terminal_committed != state(TERMINAL, None).await?
    {
        return Err("occupied live slot did not refuse the target without writes");
    }
    let released = state(HOLDER, Some("canceled")).await?;
    store
        .store_reconcile_owned(USER.into(), outcome.clone(), ReconcileProbe::None)
        .await
        .map_err(|_| "same pre-release fold failed after live slot release")?;
    let committed = state(TARGET, None).await?;
    let Some(row) = committed.row else {
        return Err("target missing after release and commit");
    };
    let history: serde_json::Value =
        serde_json::from_str(&committed.history).map_err(|_| "invalid history snapshot")?;
    if released != state(HOLDER, None).await?
        || terminal_committed != state(TERMINAL, None).await?
        || live_subscription_id().await? != TARGET
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

    // This pass read absence before the slot release. Another commit has now
    // created the target; a delayed pass without proof must retry, not append.
    let mut delayed = outcome.clone();
    delayed.history[0].stripe_invoice_id = "in_worker_provenance".into();
    delayed.row.cancel_at_period_end = true;
    for status in ["active", "trialing", "canceled"] {
        let before = state(TARGET, Some(status)).await?;
        if store
            .store_reconcile_owned(USER.into(), outcome.clone(), ReconcileProbe::None)
            .await
            .is_ok()
        {
            return Err("unchanged absence fold accepted for an existing target");
        }
        if state(TARGET, None).await? != before || state(HOLDER, None).await? != released {
            return Err("unchanged absence fold mutated committed subscription or history");
        }
        for paid_end in [None, Some(END - 1), Some(END), Some(END + 1)] {
            delayed.row.paid_through_unix = paid_end;
            delayed.row.paid_invoice_id = match paid_end {
                None | Some(END) => None,
                _ => Some("in_worker_provenance".into()),
            };
            if store
                .store_reconcile_owned(USER.into(), delayed.clone(), ReconcileProbe::None)
                .await
                .is_ok()
            {
                return Err("delayed absence fold accepted for an existing target");
            }
            if state(TARGET, None).await? != before || state(HOLDER, None).await? != released {
                return Err("delayed absence fold mutated committed subscription or history");
            }
        }
    }

    let before = state(TARGET, None).await?;
    let listing = remote(
        TARGET,
        "si_worker_exact",
        "active",
        &[("in_worker_exact", "usd"), ("in_worker_provenance", "usd")],
    );
    let fresh = reconcile_exact_target(input(before.row), &listing, NOW);
    if fresh.explicit_stale
        || fresh.read_plan.as_deref() != Some("plus")
        || fresh.read_anchor.as_deref() != Some("in_worker_exact")
    {
        return Err("fresh fold did not carry exact target proof");
    }
    store
        .store_reconcile_owned(USER.into(), fresh, ReconcileProbe::None)
        .await
        .map_err(|_| "fresh exact-target retry failed")?;
    let applied = state(TARGET, None).await?;
    let history: serde_json::Value = serde_json::from_str(&applied.history)
        .map_err(|_| "invalid provenance history snapshot")?;
    let Some(row) = applied.row else {
        return Err("fresh exact-target retry did not recover the target");
    };
    if row.status != "active"
        || row.plan_id != "plus"
        || row.paid_through_unix != Some(END)
        || row.paid_invoice_id.as_deref() != Some("in_worker_provenance")
        || applied.history_count != 2
        || history[1]["stripe_invoice_id"] != "in_worker_provenance"
        || history[1]["stripe_subscription_id"] != TARGET
        || history[1]["user_id"] != USER
        || state(HOLDER, None).await? != released
    {
        return Err("fresh exact-target retry did not recover state and history");
    }
    check_existing_target_suppression(&store).await
}

async fn check_existing_target_suppression(store: &PgStore) -> Result<(), &'static str> {
    let holder = state(TARGET, None).await?;
    let old = state(TERMINAL, None).await?;
    let terminal = remote(TERMINAL, "si_worker_terminal", "incomplete_expired", &[]);
    let close = reconcile_exact_target(input(old.row.clone()), &terminal, NOW);
    store
        .store_reconcile_owned(USER.into(), close, ReconcileProbe::None)
        .await
        .map_err(|_| "existing terminal update beside a live holder failed")?;
    let archived = state(TERMINAL, None).await?;
    if archived.row.as_ref().map(|row| row.status.as_str()) != Some("incomplete_expired")
        || archived.history != old.history
        || state(TARGET, None).await? != holder
    {
        return Err("existing terminal update was suppressed or displaced the holder");
    }

    let listing = remote(
        TERMINAL,
        "si_worker_terminal",
        "active",
        &[("in_worker_terminal_recovery", "usd")],
    );
    let delayed = reconcile_exact_target(input(holder.row.clone()), &listing, NOW);
    if delayed.read_plan.is_some()
        || delayed.read_anchor.is_some()
        || store
            .store_reconcile_owned(USER.into(), delayed, ReconcileProbe::None)
            .await
            .is_ok()
        || state(TERMINAL, None).await? != archived
        || state(TARGET, None).await? != holder
    {
        return Err("replacement-only read did not refuse the old target without writes");
    }

    let fresh = reconcile_exact_target(input(archived.row.clone()), &listing, NOW);
    if fresh.explicit_stale
        || fresh.fail_closed.is_some()
        || fresh.read_plan.as_deref() != Some("free")
        || fresh.read_anchor.is_some()
        || fresh.row.paid_through_unix != Some(END)
        || fresh.history.len() != 1
    {
        return Err("old target fold did not produce advancing evidence with its own proof");
    }
    store
        .store_reconcile_owned(USER.into(), fresh.clone(), ReconcileProbe::None)
        .await
        .map_err(|_| "old target live-slot suppression refused attributed history")?;
    let suppressed = state(TERMINAL, None).await?;
    let history: serde_json::Value = serde_json::from_str(&suppressed.history)
        .map_err(|_| "invalid suppression history snapshot")?;
    if suppressed.subscription != archived.subscription
        || suppressed.history_count != archived.history_count + 1
        || history[1]["stripe_invoice_id"] != "in_worker_terminal_recovery"
        || history[1]["stripe_subscription_id"] != TERMINAL
        || history[1]["user_id"] != USER
        || state(TARGET, None).await? != holder
        || live_subscription_id().await? != TARGET
    {
        return Err("suppression did not preserve both rows and commit only attributed history");
    }
    store
        .store_reconcile_owned(USER.into(), fresh.clone(), ReconcileProbe::None)
        .await
        .map_err(|_| "old target suppression replay failed")?;
    if state(TERMINAL, None).await? != suppressed || state(TARGET, None).await? != holder {
        return Err("old target suppression replay changed committed state");
    }

    let unknown = remote(TERMINAL, "si_worker_terminal", "trialing", &[]);
    let observed = reconcile_exact_target(input(suppressed.row.clone()), &unknown, NOW);
    if observed.explicit_stale
        || observed.read_plan.as_deref() != Some("free")
        || observed.fail_closed != Some(FailClosedReason::UnknownStatus)
    {
        return Err("unknown status did not produce a fresh fail-closed fold with its own proof");
    }
    store
        .store_reconcile_owned(USER.into(), observed, ReconcileProbe::None)
        .await
        .map_err(|_| "unknown status update beside a live holder failed")?;
    let recorded = state(TERMINAL, None).await?;
    if recorded.row.as_ref().map(|row| row.status.as_str()) != Some("trialing")
        || recorded.history != suppressed.history
        || state(TARGET, None).await? != holder
        || live_subscription_id().await? != TARGET
    {
        return Err("unknown status was not recorded verbatim or changed the live slot or history");
    }

    let released = state(TARGET, Some("canceled")).await?;
    store
        .store_reconcile_owned(USER.into(), fresh, ReconcileProbe::None)
        .await
        .map_err(|_| "same owned fold failed after the replacement released its slot")?;
    let recovered = state(TERMINAL, None).await?;
    let row = recovered.row.ok_or("old target missing after release")?;
    if row.status != "active"
        || row.paid_through_unix != Some(END)
        || row.paid_invoice_id.as_deref() != Some("in_worker_terminal_recovery")
        || recovered.history != suppressed.history
        || state(TARGET, None).await? != released
        || live_subscription_id().await? != TERMINAL
    {
        return Err("released slot did not accept the same fold without duplicating history");
    }
    Ok(())
}
