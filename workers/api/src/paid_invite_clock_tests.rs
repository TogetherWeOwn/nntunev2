use super::*;
use serde_json::json;

const CLOCK: &str = "clock_fixture";
const CUSTOMER: &str = "cus_test_1";
const SUBSCRIPTION: &str = "sub_test_clock";

fn clock_checkout(h: &Harness) -> Result<String, EnsureCheckoutError> {
    block_on(ensure_checkout_session(
        &h.ctx(),
        "u1",
        PRICE_PLUS,
        "https://example.test/ok",
        "https://example.test/cancel",
        Some(CLOCK),
    ))
}

#[test]
fn clock_checkout_refuses_existing_and_unverified_customers() {
    let mut h = stripe_harness();
    seed_billing_user(&h, "u1");
    h.config.stripe_secret_key = Some("sk_live_synthetic".into());
    assert_eq!(clock_checkout(&h), Err(EnsureCheckoutError::Unavailable));
    h.config.stripe_secret_key = Some("sk_test_synthetic".into());
    h.config.paid_invite_billing = true;
    assert_eq!(clock_checkout(&h), Err(EnsureCheckoutError::Unavailable));
    h.config.paid_invite_billing = false;
    h.store
        .billing_customers
        .borrow_mut()
        .insert("u1".into(), "cus_existing".into());
    assert_eq!(clock_checkout(&h), Err(EnsureCheckoutError::Unavailable));
    assert!(h.stripe.customers_created.borrow().is_empty());
    assert_eq!(h.store.billing_customers.borrow()["u1"], "cus_existing");
    h.store.billing_customers.borrow_mut().remove("u1");

    for reply in [
        (true, Some(CLOCK)),
        (false, None),
        (false, Some("clock_other")),
    ] {
        h.stripe.customer_reply.set(Some(reply));
        assert_eq!(clock_checkout(&h), Err(EnsureCheckoutError::Unavailable));
        assert!(h.store.billing_customers.borrow().is_empty());
    }
    h.store
        .billing_customers
        .borrow_mut()
        .insert("u2".into(), CUSTOMER.into());
    assert_eq!(clock_checkout(&h), Err(EnsureCheckoutError::Unavailable));
    assert_eq!(h.store.billing_customers.borrow().len(), 1);
    assert_eq!(h.store.billing_customers.borrow()["u2"], CUSTOMER);
    assert!(h.store.checkout_ops.borrow().is_empty());
    assert_eq!(*h.stripe.sessions_created.borrow(), 0);
}

#[test]
fn clock_checkout_does_not_inherit_an_unbound_flight() {
    for wins_during_insert in [false, true] {
        let h = stripe_harness();
        seed_billing_user(&h, "u1");
        let winner = crate::billing_reconcile::StoredCheckoutOp {
            id: "op_winner".into(),
            user_id: "u1".into(),
            state: "submitted".into(),
            price_id: Some(PRICE_PLUS.into()),
            session_id: Some("cs_test_winner".into()),
            subscription_id: None,
            replayable: true,
        };
        h.stripe.sessions.borrow_mut().insert(
            "cs_test_winner".into(),
            test_session(
                "cs_test_winner",
                "open",
                "unpaid",
                None,
                None,
                Some("op_winner"),
            ),
        );
        if wins_during_insert {
            h.store
                .plant_winner_on_next_create
                .replace(Some(winner.clone()));
        } else {
            h.store
                .checkout_ops
                .borrow_mut()
                .insert(winner.id.clone(), winner.clone());
        }

        assert_eq!(clock_checkout(&h), Err(EnsureCheckoutError::Unavailable));
        assert_eq!(h.store.checkout_ops.borrow()["op_winner"], winner);
        assert!(h.store.billing_customers.borrow().is_empty());
        assert_eq!(
            h.stripe.customers_created.borrow().len(),
            usize::from(wins_during_insert)
        );
        assert_eq!(*h.stripe.sessions_created.borrow(), 0);
        assert!(h.stripe.expired.borrow().is_empty());
    }
}

#[test]
fn clock_checkout_does_not_use_a_binding_that_lands_before_insert() {
    let h = stripe_harness();
    seed_billing_user(&h, "u1");
    h.store
        .plant_customer_on_next_create
        .set(Some("cus_unclocked"));
    assert_eq!(clock_checkout(&h), Err(EnsureCheckoutError::Unavailable));
    assert_eq!(h.store.billing_customers.borrow()["u1"], "cus_unclocked");
    assert_eq!(h.stripe.customers_created.borrow().len(), 1);
    assert_eq!(*h.stripe.sessions_created.borrow(), 0);
}

#[test]
fn clock_checkout_reconciles_the_selected_monthly_subscription() {
    let h = stripe_harness();
    seed_billing_user(&h, "u1");
    assert!(!h.config.paid_invite_billing);
    assert_eq!(
        h.call(
            "POST",
            paid_invite_billing::CHECKOUT,
            json!({"planId": PLAN_PLUS})
        )
        .status,
        404
    );
    let url = clock_checkout(&h).expect("fresh clock-bound checkout");
    assert_eq!(
        h.stripe.customers_created.borrow().as_slice(),
        &[(Some("u1".into()), Some(CLOCK.into()))]
    );
    assert_eq!(h.store.billing_customers.borrow()["u1"], CUSTOMER);
    let op = block_on(h.store.open_checkout_operation("u1"))
        .unwrap()
        .unwrap();
    let session_id = op.session_id.as_deref().unwrap();
    let session = h.stripe.sessions.borrow()[session_id].clone();
    assert_eq!(session.url.as_deref(), Some(url.as_str()));
    assert_eq!(session.customer.as_deref(), Some(CUSTOMER));
    assert_eq!(session.client_reference_id.as_deref(), Some(op.id.as_str()));
    assert_eq!(clock_checkout(&h), Err(EnsureCheckoutError::Unavailable));
    assert_eq!(*h.stripe.sessions_created.borrow(), 1);

    let period = |invoice: &str, start| {
        let end = start + BILLING_PERIOD;
        let mut subscription = test_subscription(SUBSCRIPTION, CUSTOMER, "active", PRICE_PLUS);
        subscription.current_period_end = Some(end);
        h.stripe
            .subscriptions
            .borrow_mut()
            .insert(SUBSCRIPTION.into(), subscription);
        h.stripe.invoices.borrow_mut().insert(
            invoice.into(),
            test_invoice(
                invoice,
                CUSTOMER,
                SUBSCRIPTION,
                PRICE_PLUS,
                start,
                end,
                start,
            ),
        );
        end
    };
    let first_end = period("in_test_initial", BILLING_NOW);
    let mut completed = session;
    completed.status = Some("complete".into());
    completed.payment_status = Some("paid".into());
    completed.subscription = Some(SUBSCRIPTION.into());
    h.stripe
        .sessions
        .borrow_mut()
        .insert(session_id.into(), completed);
    let completed = paid_invite_event_body(
        "evt_test_clock_checkout",
        "checkout.session.completed",
        BILLING_NOW,
        json!({"id": session_id, "customer": CUSTOMER, "subscription": SUBSCRIPTION}),
    );
    // The fixture invokes the signed handler directly with paid dispatch off.
    webhook_ok(&h.call_webhook(&completed, Some(sign(&completed, BILLING_NOW))));
    assert_eq!(
        live_for(&h, "u1").unwrap().paid_through_unix,
        Some(first_end)
    );

    let renewal_end = period("in_test_renewal", first_end);
    let mut ctx = h.ctx();
    ctx.now = first_end.try_into().unwrap();
    block_on(reconcile_remote_subscription(&ctx, "u1", SUBSCRIPTION)).unwrap();
    let live = live_for(&h, "u1").unwrap();
    assert_eq!(live.stripe_subscription_id, SUBSCRIPTION);
    assert_eq!(live.paid_through_unix, Some(renewal_end));
    let history = h.store.invoice_history.borrow();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history["in_test_renewal"].stripe_subscription_id,
        SUBSCRIPTION
    );
    assert_eq!(history["in_test_renewal"].period_end_unix, renewal_end);
    assert_eq!(
        h.store.checkout_ops.borrow()[&op.id].session_id.as_deref(),
        Some(session_id)
    );
    assert_eq!(
        h.stripe.sessions.borrow()[session_id]
            .subscription
            .as_deref(),
        Some(SUBSCRIPTION)
    );
}
