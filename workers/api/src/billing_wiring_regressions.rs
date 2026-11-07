// Billing wiring regressions. Included inside the handlers `tests` module,
// so the parent harness (`stripe_harness`, `FakeStripe`, `deliver_first_period`,
// `live_for`, ...) and its private fields are used directly.

const WIRING_DAY: i64 = 24 * 3_600;

fn wiring_event_body(event_id: &str, event_type: &str, object_id: &str) -> Vec<u8> {
    serde_json::json!({
        "id": event_id,
        "type": event_type,
        "livemode": false,
        "data": {"object": {"id": object_id}},
    })
    .to_string()
    .into_bytes()
}

fn wiring_deliver(h: &Harness, event_id: &str, event_type: &str, object_id: &str) -> Response {
    let body = wiring_event_body(event_id, event_type, object_id);
    h.call_webhook(&body, Some(sign(&body, BILLING_NOW)))
}

fn wiring_usage_seed(h: &Harness) -> HashMap<(String, String), u64> {
    h.store
        .stream_seconds
        .borrow_mut()
        .insert(("u1".to_string(), "2023-11".to_string()), 4_321);
    h.store.stream_seconds.borrow().clone()
}

/// Remote Pro upgrade on top of a delivered Plus first period: the
/// subscription now carries the Pro price and a confirmed prorated invoice
/// covers the same period end (the portal must not need a renewal).
fn wiring_remote_pro_upgrade(h: &Harness) -> i64 {
    let start = BILLING_NOW + 3_600;
    let end = BILLING_NOW + BILLING_PERIOD;
    h.stripe.subscriptions.borrow_mut().insert(
        "sub_test_1".to_string(),
        test_subscription("sub_test_1", "cus_test_1", "active", PRICE_PRO),
    );
    h.stripe.invoices.borrow_mut().insert(
        "in_test_2".to_string(),
        test_invoice(
            "in_test_2",
            "cus_test_1",
            "sub_test_1",
            PRICE_PRO,
            start,
            end,
            start,
        ),
    );
    end
}

/// A stored `past_due` row whose paid period and grace are both over at
/// the harness clock. The remote still reads `past_due` with only the old
/// paid invoice, so the pass is a genuine expiration candidate.
fn wiring_seed_past_grace(h: &Harness) -> crate::billing_reconcile::ReconciledRow {
    let paid_end = BILLING_NOW - 20 * WIRING_DAY;
    seed_billing_user(h, "u1");
    h.store
        .billing_customers
        .borrow_mut()
        .insert("u1".to_string(), "cus_test_1".to_string());
    let row = crate::billing_reconcile::ReconciledRow {
        stripe_subscription_id: "sub_test_1".to_string(),
        stripe_customer_id: "cus_test_1".to_string(),
        stripe_item_id: "si_test_1".to_string(),
        stripe_price_id: PRICE_PLUS.to_string(),
        plan_id: PLAN_PLUS.to_string(),
        status: "past_due".to_string(),
        current_period_end_unix: Some(paid_end),
        cancel_at_period_end: false,
        paid_through_unix: Some(paid_end),
        paid_invoice_id: Some("in_old".to_string()),
        grace_until_unix: Some(BILLING_NOW - WIRING_DAY),
    };
    h.store
        .billing_rows
        .borrow_mut()
        .insert("u1".to_string(), row.clone());
    h.stripe.subscriptions.borrow_mut().insert(
        "sub_test_1".to_string(),
        test_subscription("sub_test_1", "cus_test_1", "past_due", PRICE_PLUS),
    );
    h.stripe.invoices.borrow_mut().insert(
        "in_old".to_string(),
        test_invoice(
            "in_old",
            "cus_test_1",
            "sub_test_1",
            PRICE_PLUS,
            paid_end - BILLING_PERIOD,
            paid_end,
            paid_end - BILLING_PERIOD + 60,
        ),
    );
    row
}

fn wiring_ensure(h: &Harness) -> Result<String, EnsureCheckoutError> {
    block_on(ensure_checkout_session(
        &h.ctx(),
        "u1",
        PRICE_PLUS,
        "https://example.com/ok",
        "https://example.com/cancel",
        None,
    ))
}

#[test]
fn billing_wiring_paid_plan_change_event_folds_promptly_and_converges() {
    let h = stripe_harness();
    seed_billing_user(&h, "u1");
    deliver_first_period(&h, "evt_test_1");
    let usage = wiring_usage_seed(&h);
    let new_end = wiring_remote_pro_upgrade(&h);

    // The paid invoice event is a wake hint: the fresh invoice and
    // subscription are re-read and the plan change lands in one delivery.
    webhook_ok(&wiring_deliver(
        &h,
        "evt_test_2",
        "invoice.paid",
        "in_test_2",
    ));
    let live = live_for(&h, "u1").expect("live row stored");
    assert_eq!(live.plan_id, PLAN_PRO);
    assert_eq!(live.status, "active");
    assert_eq!(live.paid_through_unix, Some(new_end));
    assert_eq!(live.paid_invoice_id.as_deref(), Some("in_test_2"));
    assert_eq!(h.store.invoice_history.borrow().len(), 2);

    // Duplicate and reordered deliveries (an older invoice event, the
    // subscription hints, a payment_succeeded twin) converge on the same
    // state and never regress coverage or the plan.
    webhook_ok(&wiring_deliver(
        &h,
        "evt_test_2",
        "invoice.paid",
        "in_test_2",
    ));
    webhook_ok(&wiring_deliver(
        &h,
        "evt_test_3",
        "invoice.payment_succeeded",
        "in_test_1",
    ));
    webhook_ok(&wiring_deliver(
        &h,
        "evt_test_4",
        "customer.subscription.updated",
        "sub_test_1",
    ));
    webhook_ok(&wiring_deliver(
        &h,
        "evt_test_5",
        "invoice.payment_succeeded",
        "in_test_2",
    ));
    let again = live_for(&h, "u1").expect("live row stored");
    assert_eq!(again, live);
    assert_eq!(h.store.invoice_history.borrow().len(), 2);
    assert!(h.store.billing_operation_users.borrow().is_empty());
    // Billing never touches the usage ledger.
    assert_eq!(*h.store.stream_seconds.borrow(), usage);
}

#[test]
fn billing_wiring_unconfirmed_invoice_never_upgrades_plan() {
    let h = stripe_harness();
    seed_billing_user(&h, "u1");
    deliver_first_period(&h, "evt_test_1");
    let usage = wiring_usage_seed(&h);
    let before = live_for(&h, "u1").expect("live row stored");
    wiring_remote_pro_upgrade(&h);
    // The Pro invoice exists remotely but is not paid.
    h.stripe
        .invoices
        .borrow_mut()
        .get_mut("in_test_2")
        .expect("invoice seeded")
        .status = "open".to_string();

    webhook_ok(&wiring_deliver(
        &h,
        "evt_test_2",
        "invoice.paid",
        "in_test_2",
    ));
    assert_eq!(live_for(&h, "u1").expect("live row"), before);

    // A subscription hint with the Pro price but no paid Pro money grants
    // nothing either: coverage and plan stay on the confirmed Plus period.
    webhook_ok(&wiring_deliver(
        &h,
        "evt_test_3",
        "customer.subscription.updated",
        "sub_test_1",
    ));
    let live = live_for(&h, "u1").expect("live row");
    assert_eq!(live.plan_id, PLAN_PLUS);
    assert_eq!(live.paid_through_unix, before.paid_through_unix);
    assert_eq!(live.paid_invoice_id, before.paid_invoice_id);
    assert_eq!(h.store.invoice_history.borrow().len(), 1);
    assert_eq!(*h.store.stream_seconds.borrow(), usage);
}

#[test]
fn billing_wiring_lost_webhook_is_folded_by_sweep_even_when_active_far_future() {
    let h = stripe_harness();
    seed_billing_user(&h, "u1");
    deliver_first_period(&h, "evt_test_1");
    let usage = wiring_usage_seed(&h);
    let new_end = wiring_remote_pro_upgrade(&h);
    // No event is delivered: the stored row is `active` with coverage far
    // in the future, and the polling pass alone must still pick it up.
    let report = block_on(run_billing_reconcile_sweep(&h.ctx()));
    assert_eq!(report.subscriptions_attempted, 1);
    assert_eq!(report.subscriptions_failed, 0);
    let live = live_for(&h, "u1").expect("live row stored");
    assert_eq!(live.plan_id, PLAN_PRO);
    assert_eq!(live.paid_through_unix, Some(new_end));
    assert_eq!(h.store.invoice_history.borrow().len(), 2);
    assert!(h.stripe.canceled.borrow().is_empty());
    assert_eq!(*h.store.stream_seconds.borrow(), usage);
}

#[test]
fn billing_wiring_past_grace_recovered_payment_is_folded_before_any_cancel() {
    let h = stripe_harness();
    wiring_seed_past_grace(&h);
    let usage = wiring_usage_seed(&h);
    let recovered_end = BILLING_NOW + 10 * WIRING_DAY;
    // The payer recovered: a new paid invoice exists although the remote
    // still reports `past_due` and the stored grace is exhausted.
    h.stripe.invoices.borrow_mut().insert(
        "in_new".to_string(),
        test_invoice(
            "in_new",
            "cus_test_1",
            "sub_test_1",
            PRICE_PLUS,
            BILLING_NOW - 20 * WIRING_DAY,
            recovered_end,
            BILLING_NOW - 3_600,
        ),
    );
    let outcome = block_on(reconcile_remote_subscription(&h.ctx(), "u1", "sub_test_1"))
        .expect("recovered payer reconciles");
    assert!(outcome.fail_closed.is_none());
    assert!(
        h.stripe.canceled.borrow().is_empty(),
        "a recovered payer must never be canceled"
    );
    let live = live_for(&h, "u1").expect("slot kept");
    assert_eq!(live.paid_through_unix, Some(recovered_end));
    assert_eq!(live.grace_until_unix, None);
    assert_eq!(live.paid_invoice_id.as_deref(), Some("in_new"));
    assert!(h.store.invoice_history.borrow().contains_key("in_new"));
    assert_eq!(*h.store.stream_seconds.borrow(), usage);
}

#[test]
fn billing_wiring_grace_still_open_is_not_canceled() {
    let h = stripe_harness();
    let mut row = wiring_seed_past_grace(&h);
    row.grace_until_unix = Some(BILLING_NOW + 1);
    h.store
        .billing_rows
        .borrow_mut()
        .insert("u1".to_string(), row);
    block_on(reconcile_remote_subscription(&h.ctx(), "u1", "sub_test_1")).expect("pass reconciles");
    assert!(h.stripe.canceled.borrow().is_empty());
    let live = live_for(&h, "u1").expect("slot kept");
    assert_eq!(live.status, "past_due");
    assert_eq!(live.grace_until_unix, Some(BILLING_NOW + 1));
}

#[test]
fn billing_wiring_expired_grace_cancels_and_stores_terminal_keeping_history() {
    let h = stripe_harness();
    let seeded = wiring_seed_past_grace(&h);
    let usage = wiring_usage_seed(&h);
    let outcome = block_on(reconcile_remote_subscription(&h.ctx(), "u1", "sub_test_1"))
        .expect("expiration settles");
    assert_eq!(outcome.row.status, "canceled");
    assert_eq!(*h.stripe.canceled.borrow(), vec!["sub_test_1".to_string()]);
    // The terminal row is stored: the slot is released (no live row) while
    // coverage, plan and the paid-invoice history stay as the record.
    assert!(live_for(&h, "u1").is_none());
    let stored = h
        .store
        .billing_rows
        .borrow()
        .get("u1")
        .cloned()
        .expect("terminal row kept");
    assert_eq!(stored.status, "canceled");
    assert_eq!(stored.grace_until_unix, None);
    assert_eq!(stored.paid_through_unix, seeded.paid_through_unix);
    assert_eq!(stored.plan_id, seeded.plan_id);
    assert!(h.store.invoice_history.borrow().contains_key("in_old"));
    assert!(h.store.billing_operation_users.borrow().is_empty());
    assert_eq!(*h.store.stream_seconds.borrow(), usage);
}

#[test]
fn billing_wiring_cancel_failure_keeps_live_slot_and_mints_no_session() {
    let h = stripe_harness();
    let seeded = wiring_seed_past_grace(&h);
    h.stripe
        .fail
        .borrow_mut()
        .insert("cancel_subscription".to_string());
    assert!(block_on(reconcile_remote_subscription(&h.ctx(), "u1", "sub_test_1")).is_err());
    assert!(h.stripe.canceled.borrow().is_empty());
    let live = live_for(&h, "u1").expect("live slot preserved");
    assert_eq!(live.status, "past_due");
    assert_eq!(live.paid_through_unix, seeded.paid_through_unix);
    assert_eq!(live.grace_until_unix, seeded.grace_until_unix);
    // The still-held slot refuses a new purchase and mints nothing.
    assert_eq!(wiring_ensure(&h), Err(EnsureCheckoutError::Unavailable));
    assert_eq!(*h.stripe.sessions_created.borrow(), 0);
    assert!(h.store.billing_operation_users.borrow().is_empty());

    // The sweep reports the failure and retries later without a cancel.
    let report = block_on(run_billing_reconcile_sweep(&h.ctx()));
    assert_eq!(report.subscriptions_attempted, 1);
    assert_eq!(report.subscriptions_failed, 1);
    assert!(live_for(&h, "u1").is_some());
}

#[test]
fn billing_wiring_busy_guard_blocks_sweep_reconcile_event_and_purchase() {
    let h = stripe_harness();
    let seeded = wiring_seed_past_grace(&h);
    // An open paid checkout for the same user, so the sweep has both an
    // operation and a subscription target to attempt.
    h.store.checkout_ops.borrow_mut().insert(
        "op-busy-1".to_string(),
        crate::billing_reconcile::StoredCheckoutOp {
            id: "op-busy-1".to_string(),
            user_id: "u1".to_string(),
            state: "submitted".to_string(),
            price_id: Some(PRICE_PLUS.to_string()),
            session_id: Some("cs_test_busy".to_string()),
            subscription_id: None,
        },
    );
    h.stripe.sessions.borrow_mut().insert(
        "cs_test_busy".to_string(),
        test_session(
            "cs_test_busy",
            "complete",
            "paid",
            Some("cus_test_1"),
            Some("sub_test_1"),
            Some("op-busy-1"),
        ),
    );
    // Another worker holds the per-user operation.
    h.store
        .billing_operation_users
        .borrow_mut()
        .insert("u1".to_string());

    assert!(block_on(reconcile_remote_subscription(&h.ctx(), "u1", "sub_test_1")).is_err());
    assert_eq!(wiring_ensure(&h), Err(EnsureCheckoutError::Failed));
    let report = block_on(run_billing_reconcile_sweep(&h.ctx()));
    assert_eq!(report.operations_attempted, 1);
    assert_eq!(report.operations_failed, 1);
    assert_eq!(report.subscriptions_attempted, 1);
    assert_eq!(report.subscriptions_failed, 1);
    let paid = test_invoice(
        "in_busy",
        "cus_test_1",
        "sub_test_1",
        PRICE_PLUS,
        BILLING_NOW,
        BILLING_NOW + BILLING_PERIOD,
        BILLING_NOW,
    );
    h.stripe
        .invoices
        .borrow_mut()
        .insert("in_busy".to_string(), paid);
    assert_eq!(
        wiring_deliver(&h, "evt_busy", "invoice.paid", "in_busy").status,
        500
    );
    assert!(
        h.store.webhook_events.borrow().is_empty(),
        "a busy delivery releases its claim so the retry reprocesses"
    );

    // Nothing remote mutated and nothing stored while busy.
    assert!(h.stripe.canceled.borrow().is_empty());
    assert_eq!(*h.stripe.sessions_created.borrow(), 0);
    assert_eq!(
        h.store.billing_rows.borrow().get("u1").cloned(),
        Some(seeded)
    );
    // The foreign hold is untouched by the refused callers.
    assert!(h.store.billing_operation_users.borrow().contains("u1"));
    let op_state = h
        .store
        .checkout_ops
        .borrow()
        .get("op-busy-1")
        .map(|op| op.state.clone());
    assert_eq!(op_state.as_deref(), Some("submitted"));
}

#[test]
fn billing_wiring_operation_guard_is_released_on_completion_and_on_drop() {
    let h = stripe_harness();
    let ran = block_on(h.store.with_billing_operation("u1", async { 7 }));
    assert_eq!(ran.ok(), Some(7));
    assert!(h.store.billing_operation_users.borrow().is_empty());

    // A cancelled callback (future dropped while suspended) must free the
    // guard, or the user is locked out of billing for good.
    let waker = Waker::noop();
    let mut cx = TaskContext::from_waker(waker);
    let mut pending = Box::pin(
        h.store
            .with_billing_operation("u1", std::future::pending::<()>()),
    );
    assert!(pending.as_mut().poll(&mut cx).is_pending());
    assert!(h.store.billing_operation_users.borrow().contains("u1"));
    assert!(block_on(h.store.with_billing_operation("u1", async {})).is_err());
    drop(pending);
    assert!(h.store.billing_operation_users.borrow().is_empty());
    assert!(block_on(h.store.with_billing_operation("u1", async {})).is_ok());
}

#[test]
fn billing_wiring_uncertain_terminal_confirmation_or_persist_keeps_slot_until_retry() {
    for failure in ["retrieve", "persist", "nonterminal"] {
        let h = stripe_harness();
        let seeded = wiring_seed_past_grace(&h);
        match failure {
            "retrieve" => *h.stripe.fail_subscription_retrieve_at.borrow_mut() = Some(2),
            "persist" => *h.store.fail_terminal_reconcile_write.borrow_mut() = true,
            _ => *h.stripe.cancel_response_status.borrow_mut() = Some("past_due".to_string()),
        }
        assert!(block_on(reconcile_remote_subscription(&h.ctx(), "u1", "sub_test_1")).is_err());
        let live = live_for(&h, "u1").expect("uncertain terminal must keep the live slot");
        assert_eq!(live.status, "past_due");
        assert_eq!(live.paid_through_unix, seeded.paid_through_unix);
        assert_eq!(wiring_ensure(&h), Err(EnsureCheckoutError::Unavailable));
        assert_eq!(*h.stripe.sessions_created.borrow(), 0);
        assert_eq!(h.stripe.canceled.borrow().len(), 1);
        assert!(h.store.billing_operation_users.borrow().is_empty());
        *h.stripe.fail_subscription_retrieve_at.borrow_mut() = None;
        *h.store.fail_terminal_reconcile_write.borrow_mut() = false;
        *h.stripe.cancel_response_status.borrow_mut() = None;
        block_on(reconcile_remote_subscription(&h.ctx(), "u1", "sub_test_1"))
            .expect("fresh confirmed terminal observation repairs the slot");
        assert!(live_for(&h, "u1").is_none());
        assert!(h.store.invoice_history.borrow().contains_key("in_old"));
        assert_eq!(*h.stripe.sessions_created.borrow(), 0);
    }
}

#[test]
fn billing_wiring_confirmed_terminal_allows_one_reused_replacement_session() {
    let h = stripe_harness();
    wiring_seed_past_grace(&h);
    block_on(reconcile_remote_subscription(&h.ctx(), "u1", "sub_test_1"))
        .expect("confirmed cancellation settles the slot");
    let first = wiring_ensure(&h).expect("terminal slot admits replacement");
    let second = wiring_ensure(&h).expect("same operation reuses replacement");
    assert_eq!(first, second);
    assert_eq!(*h.stripe.sessions_created.borrow(), 1);
    assert_eq!(h.stripe.canceled.borrow().len(), 1);
    assert!(h.store.billing_operation_users.borrow().is_empty());
}
