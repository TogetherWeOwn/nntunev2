//! Exact-target folds through the owned Worker store on disposable PostgreSQL.
use super::{
    billing_budget, MirrorBinding, PgStore, ReconcileProbe, Store, StoreError, CONNECTION,
};
use crate::billing_reconcile::{
    reconcile_exact_target, reconcile_live_select_statement, reconcile_target_select_statement,
    FailClosedReason, LiveRow, PaidInvoiceRow, PriceBinding, ReconcileInput, ReconcileOutcome,
    RemoteSubscription,
};
use serde_json::{json, Value};

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
        invoices: invoices.iter().map(|(invoice, currency)| {
            serde_json::from_value(json!({
                "id": invoice, "customer": customer, "subscription": id, "status": "paid",
                "currency": currency, "amount_paid": 800, "status_transitions": { "paid_at": NOW - 1 },
                "lines": { "has_more": false, "data": [{ "id": "il_exact_target", "amount": 800, "price": PRICE, "currency": currency,
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

fn fold(
    requested_id: &str,
    row: Option<LiveRow>,
    remote: &RemoteSubscription,
) -> Result<ReconcileOutcome, &'static str> {
    reconcile_exact_target(requested_id, input(row), remote, NOW)
        .ok_or("provider returned the wrong subscription")
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
        let stale = fold(HOLDER, Some(row.clone()), &listing)?;
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
        let fresh = fold(HOLDER, Some(row), &listing)?;
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
    let mut terminal = fold(TERMINAL, holder.row.clone(), &terminal_listing)?;
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
    terminal.history = fold(TERMINAL, None, &confirmed)?.history;
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
    let mut delayed = terminal.clone();
    delayed.history[0].stripe_invoice_id = "in_worker_delayed_terminal".into();
    for _ in 0..2 {
        if store
            .store_reconcile_owned(USER.into(), delayed.clone(), ReconcileProbe::None)
            .await
            .is_ok()
            || state(TERMINAL, None).await? != terminal_committed
            || state(HOLDER, None).await? != holder
        {
            return Err("terminal fold without read proof was not refused without writes");
        }
    }
    let mut replay = fold(TERMINAL, Some(row), &terminal_listing)?;
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
    if fold(HOLDER, holder.row.clone(), &listing).is_ok() {
        return Err("fold accepted a provider subscription different from the requested target");
    }
    let outcome = fold(TARGET, absent.row.clone(), &listing)?;
    if absent.subscription.is_some()
        || absent.history_count != 0
        || holder.row.is_none()
        || outcome != fold(TARGET, holder.row.clone(), &listing)?
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
    let fresh = fold(TARGET, before.row, &listing)?;
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
    check_existing_target_suppression(&store).await?;
    check_terminal_provenance(&store).await?;
    check_archive_evidence(&store).await
}

async fn check_terminal_provenance(store: &PgStore) -> Result<(), &'static str> {
    const OLD_ANCHOR: &str = "in_worker_terminal_recovery";
    const NEW_ANCHOR: &str = "in_worker_terminal_new_anchor";
    const NEW_PRICE: &str = "price_worker_terminal_relabel";
    const OLD_ITEM: &str = "si_worker_terminal";
    const NEW_ITEM: &str = "si_worker_terminal_relabel";

    let mut listing = remote(TERMINAL, OLD_ITEM, "active", &[(OLD_ANCHOR, "usd")]);
    let mut local = input(state(TERMINAL, None).await?.row);
    local.bindings.push(PriceBinding {
        price_id: NEW_PRICE.into(),
        plan_id: "max".into(),
        selectable: true,
        livemode: false,
    });
    for (status, read_plan, read_anchor, plan) in [
        ("canceled", "plus", OLD_ANCHOR, "plus"),
        ("incomplete_expired", "plus", NEW_ANCHOR, "max"),
    ] {
        let mut held = listing.clone();
        held.subscription.status = "paused".into();
        held.subscription.current_period_end = Some(END - 1);
        held.subscription.cancel_at_period_end = false;
        let delayed_held = reconcile_exact_target(TERMINAL, local.clone(), &held, NOW)
            .ok_or("delayed held fold missing")?;
        if delayed_held.explicit_stale
            || delayed_held.fail_closed.is_some()
            || delayed_held.row.paid_through_unix != Some(END)
        {
            return Err("held fold did not carry equal-end proof from its original read");
        }
        let mut terminal = listing.clone();
        terminal.subscription.status = status.into();
        terminal.subscription.current_period_end = Some(END - 1);
        terminal.subscription.cancel_at_period_end = false;
        let mut delayed = reconcile_exact_target(TERMINAL, local.clone(), &terminal, NOW)
            .ok_or("delayed terminal fold missing")?;
        if delayed.explicit_stale || delayed.fail_closed.is_some() || !delayed.history.is_empty() {
            return Err("terminal fold was not fresh for its original read");
        }
        // Terminal folds emit no history. Inject independently confirmed, new
        // history to prove that a refused transaction does not append it.
        delayed.history = fold(
            TERMINAL,
            None,
            &remote(
                TERMINAL,
                OLD_ITEM,
                "active",
                &[("in_worker_refused_terminal", "usd")],
            ),
        )?
        .history;
        if plan == "plus" {
            listing = remote(
                TERMINAL,
                OLD_ITEM,
                "active",
                &[(OLD_ANCHOR, "usd"), (NEW_ANCHOR, "usd")],
            );
            listing.invoices[1]
                .status_transitions
                .as_mut()
                .unwrap()
                .paid_at = Some(NOW);
        } else {
            // Same shared anchor and paid end, with a qualifying line for the new price.
            let item = &mut listing.subscription.items.as_mut().unwrap().data[0];
            item.id = NEW_ITEM.into();
            item.price = Some(NEW_PRICE.into());
            let lines = &mut listing.invoices[1].lines.as_mut().unwrap().data;
            let mut relabel_line = lines[0].clone();
            relabel_line.id = "il_terminal_relabel".into();
            relabel_line.price = Some(NEW_PRICE.into());
            lines.push(relabel_line);
        }
        listing.subscription.cancel_at_period_end = true;
        let newer = reconcile_exact_target(TERMINAL, local.clone(), &listing, NOW)
            .ok_or("newer active fold missing")?;
        store
            .store_reconcile_owned(USER.into(), newer, ReconcileProbe::None)
            .await
            .map_err(|_| "newer equal-end active fold was refused")?;
        let committed = state(TERMINAL, None).await?;
        let row = committed.row.as_ref().ok_or("newer row missing")?;
        if row.plan_id != plan
            || row.paid_invoice_id.as_deref() != Some(NEW_ANCHOR)
            || row.paid_through_unix != Some(END)
            || delayed.row.paid_through_unix != Some(END)
            || delayed.read_plan.as_deref() != Some(read_plan)
            || delayed.read_anchor.as_deref() != Some(read_anchor)
        {
            return Err("fixture did not isolate one changed proof field at equal paid end");
        }
        store
            .store_reconcile_owned(USER.into(), delayed_held, ReconcileProbe::None)
            .await
            .map_err(|_| "changed held proof did not resolve as a stale write")?;
        if state(TERMINAL, None).await? != committed {
            return Err("delayed held fold changed the newer row or history");
        }
        let refused = std::rc::Rc::new(std::cell::Cell::new(false));
        if store
            .store_reconcile_owned(
                USER.into(),
                delayed,
                ReconcileProbe::ObserveTerminalProvenanceRefusal(refused.clone()),
            )
            .await
            .is_ok()
            || !refused.get()
            || state(TERMINAL, None).await? != committed
        {
            return Err("changed terminal proof did not refuse without row/history writes");
        }
        if !entitled_at(END - 1).await? || entitled_at(END).await? {
            return Err("scheduled cancellation did not preserve the exact paid-end cutoff");
        }
        local.live = committed.row;
    }

    listing.subscription.cancel_at_period_end = false;
    let uncancel = reconcile_exact_target(TERMINAL, local.clone(), &listing, NOW)
        .ok_or("uncancel fold missing")?;
    store
        .store_reconcile_owned(USER.into(), uncancel, ReconcileProbe::None)
        .await
        .map_err(|_| "same-proof uncancel was refused")?;
    if !entitled_at(END).await? {
        return Err("fresh uncancel did not restore the ordinary settlement allowance");
    }

    let before = state(TERMINAL, None).await?;
    local.live = before.row.clone();
    listing.subscription.status = "incomplete_expired".into();
    let fresh = reconcile_exact_target(TERMINAL, local, &listing, NOW)
        .ok_or("fresh terminal refold missing")?;
    if fresh.explicit_stale
        || fresh.fail_closed.is_some()
        || fresh.read_plan.as_deref() != Some("max")
        || fresh.read_anchor.as_deref() != Some(NEW_ANCHOR)
    {
        return Err("fresh terminal refold lacked matching provenance");
    }
    store
        .store_reconcile_owned(USER.into(), fresh, ReconcileProbe::None)
        .await
        .map_err(|_| "fresh terminal refold was refused")?;
    let after = state(TERMINAL, None).await?;
    let mut expected = before.row.ok_or("active row missing before fresh refold")?;
    expected.status = "incomplete_expired".into();
    if after.row != Some(expected)
        || after.history != before.history
        || entitled_at(END - 1).await?
    {
        return Err("fresh terminal refold failed to release entitlement or changed paid history");
    }
    Ok(())
}

async fn archive_fixture_sql(sql: &'static str) -> Result<(), &'static str> {
    billing_budget::with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |client| {
            Box::pin(async move { client.batch_execute(sql).await.map_err(|_| StoreError) })
        },
        |_| {},
    )
    .await
    .map_err(|_| "archive fixture setup failed")
}

async fn check_archive_evidence(store: &PgStore) -> Result<(), &'static str> {
    let archive = state(HOLDER, Some("trialing")).await?;
    let replacement = state(TARGET, Some("active")).await?;
    let mut listing = remote(
        HOLDER,
        "si_worker_fault",
        "active",
        &[
            ("in_worker_first", "usd"),
            ("in_worker_second", "eur"),
            ("in_worker_absent", "usd"),
        ],
    );
    let replay = fold(HOLDER, archive.row.clone(), &listing)?;
    store
        .store_reconcile_outcome(USER, &replay)
        .await
        .map_err(|_| "exact invoice replay failed")?;
    if state(HOLDER, None).await? != archive || state(TARGET, None).await? != replacement {
        return Err("exact invoice replay changed archive or replacement");
    }

    archive_fixture_sql(
        "UPDATE public.paid_invoice_history SET covered_periods = NULL
                         WHERE stripe_invoice_id = 'in_worker_first'",
    )
    .await?;
    let legacy = state(HOLDER, None).await?;
    let mut unknown = replay.clone();
    unknown.history[0].covered_periods.clear();
    store
        .store_reconcile_outcome(USER, &unknown)
        .await
        .map_err(|_| "null coverage replay failed")?;
    if legacy == archive
        || state(HOLDER, None).await? != legacy
        || state(TARGET, None).await? != replacement
    {
        return Err("null coverage replay changed archive or replacement");
    }
    let mut changed = replay.clone();
    changed.history[0].amount_cents += 1;
    if store.store_reconcile_outcome(USER, &changed).await.is_ok()
        || state(HOLDER, None).await? != legacy
        || state(TARGET, None).await? != replacement
    {
        return Err("conflicting evidence hydrated legacy coverage");
    }
    let mut hydrated = replay;
    // Adjacent components are the same multirange as the original interval.
    hydrated.history[0].covered_periods = vec![(NOW - 1, NOW + 1), (NOW + 1, END)];
    for _ in 0..2 {
        store
            .store_reconcile_outcome(USER, &hydrated)
            .await
            .map_err(|_| "exact legacy hydration or replay failed")?;
        if state(HOLDER, None).await? != archive || state(TARGET, None).await? != replacement {
            return Err(
                "exact hydration changed fields beyond coverage or touched the replacement",
            );
        }
    }

    // Release the slot so the next fold reaches the subscription write as
    // well as the history write. Keep the entire replacement as a witness.
    let released = state(TARGET, Some("canceled")).await?;
    let mut advancing = remote(
        HOLDER,
        "si_worker_fault",
        "active",
        &[("in_worker_archive_fresh", "usd")],
    );
    advancing.invoices[0].lines.as_mut().unwrap().data[0]
        .period
        .as_mut()
        .unwrap()
        .end = Some(END + 10);
    listing.invoices.insert(0, advancing.invoices.remove(0));
    let fresh = fold(HOLDER, archive.row.clone(), &listing)?;
    // The fresh invoice precedes the conflict, so both its insert and the
    // subscription recovery must disappear on refusal.
    if fresh.explicit_stale
        || fresh.fail_closed.is_some()
        || fresh.row.paid_through_unix != Some(END + 10)
        || fresh
            .history
            .get(1)
            .map(|row| row.stripe_invoice_id.as_str())
            != Some("in_worker_first")
    {
        return Err("archive conflict fixture did not produce advancing paid evidence");
    }
    let conflicts: [fn(&mut PaidInvoiceRow); 8] = [
        |row| row.amount_cents += 1,
        |row| row.currency = "eur".into(),
        |row| row.plan_id = "max".into(),
        |row| row.period_start_unix -= 1,
        |row| row.period_end_unix += 1,
        |row| row.paid_at_unix += 1,
        |row| row.covered_periods = vec![(NOW - 1, NOW + 1), (NOW + 2, END)],
        |row| row.stripe_invoice_id = "in_worker_exact".into(),
    ];
    for conflict in conflicts {
        let mut changed = fresh.clone();
        conflict(&mut changed.history[1]);
        if store.store_reconcile_outcome(USER, &changed).await.is_ok()
            || state(HOLDER, None).await? != archive
            || state(TARGET, None).await? != released
        {
            return Err("changed paid evidence did not refuse and roll back the archive recovery");
        }
    }

    archive_fixture_sql(
        "INSERT INTO public.users (id, email, created_at)
                         VALUES ('worker_archive_other', 'archive-other@example.test', 1786032000);
                         UPDATE public.paid_invoice_history SET user_id = 'worker_archive_other'
                         WHERE stripe_invoice_id = 'in_worker_first'",
    )
    .await?;
    let foreign = state(HOLDER, None).await?;
    if foreign == archive
        || store.store_reconcile_outcome(USER, &fresh).await.is_ok()
        || state(HOLDER, None).await? != foreign
        || state(TARGET, None).await? != released
    {
        return Err("foreign history attribution did not refuse and roll back");
    }
    archive_fixture_sql(
        "UPDATE public.paid_invoice_history SET user_id = 'worker_fault_user'
                         WHERE stripe_invoice_id = 'in_worker_first'",
    )
    .await?;
    let retry = fold(HOLDER, state(HOLDER, None).await?.row, &listing)?;
    store
        .store_reconcile_outcome(USER, &retry)
        .await
        .map_err(|_| "fresh archive refold failed")?;
    let committed = state(HOLDER, None).await?;
    let invoices = |snapshot: &TargetState| {
        serde_json::from_str::<Vec<Value>>(&snapshot.history).map_err(|_| "invalid archive history")
    };
    let mut history = invoices(&committed)?;
    history.retain(|row| row["stripe_invoice_id"] != "in_worker_archive_fresh");
    if committed.history_count != archive.history_count + 1
        || history != invoices(&archive)?
        || committed
            .row
            .as_ref()
            .and_then(|row| row.paid_invoice_id.as_deref())
            != Some("in_worker_archive_fresh")
        || live_subscription_id().await? != HOLDER
        || state(TARGET, None).await? != released
    {
        return Err("fresh paid evidence did not recover the archive while preserving prior state");
    }
    let retry = fold(HOLDER, committed.row.clone(), &listing)?;
    store
        .store_reconcile_outcome(USER, &retry)
        .await
        .map_err(|_| "fresh evidence replay failed")?;
    if state(HOLDER, None).await? != committed || state(TARGET, None).await? != released {
        return Err("fresh evidence replay changed committed snapshots");
    }
    Ok(())
}

async fn entitled_at(now: i64) -> Result<bool, &'static str> {
    use crate::billing::{
        resolve_effective_plan, subscription_live_select_statement, SubscriptionView,
    };
    billing_budget::with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |client| {
            Box::pin(async move {
                let row = client
                    .query_opt(subscription_live_select_statement(), &[&USER])
                    .await
                    .map_err(|_| StoreError)?;
                let view = row.as_ref().map(|row| SubscriptionView {
                    plan_id: row.get("plan_id"),
                    status: row.get("status"),
                    cancel_at_period_end: row.get("cancel_at_period_end"),
                    paid_through_at_unix: row.get("paid_through_at_unix"),
                    grace_until_unix: row.get("grace_until_unix"),
                    current_period_end_unix: row.get("current_period_end_unix"),
                });
                Ok(resolve_effective_plan(view, now).entitled)
            })
        },
        |_| {},
    )
    .await
    .map_err(|_| "live entitlement fixture query failed")
}

async fn check_existing_target_suppression(store: &PgStore) -> Result<(), &'static str> {
    let holder = state(TARGET, None).await?;
    let old = state(TERMINAL, None).await?;
    let terminal = remote(TERMINAL, "si_worker_terminal", "incomplete_expired", &[]);
    let close = fold(TERMINAL, old.row.clone(), &terminal)?;
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
    let delayed = fold(TERMINAL, holder.row.clone(), &listing)?;
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

    let fresh = fold(TERMINAL, archived.row.clone(), &listing)?;
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
    let observed = fold(TERMINAL, suppressed.row.clone(), &unknown)?;
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
