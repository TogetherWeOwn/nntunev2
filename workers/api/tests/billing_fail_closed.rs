use nntune_api::billing_reconcile::{
    reconcile_subscription, FailClosedReason, LiveRow, PriceBinding, ReconcileInput,
    RemoteSubscription,
};
use nntune_api::billing_store_classification::{
    classify_write, Accepted, Identity, Refused, Target,
};
use nntune_api::billing_sweep::{scheduled_hour, CRON};
use nntune_api::stripe::{Invoice, InvoiceList};
use serde_json::json;

#[cfg(not(target_arch = "wasm32"))]
#[path = "../src/billing_cancel_control.rs"]
mod billing_cancel_control;

const NOW: i64 = 1_800_000_000;

fn local() -> ReconcileInput {
    ReconcileInput {
        stripe_customer_id: Some("cus_fixture".into()),
        bindings: vec![PriceBinding {
            price_id: "price_fixture".into(),
            plan_id: "plus".into(),
            selectable: true,
            livemode: false,
        }],
        live: None,
    }
}

fn invoice(id: &str, end: i64) -> Invoice {
    serde_json::from_value(json!({
        "id": id,
        "customer": "cus_fixture",
        "subscription": "sub_fixture",
        "status": "paid",
        "amount_paid": 800,
        "currency": "usd",
        "status_transitions": {"paid_at": NOW - 30},
        "lines": {"has_more": false, "data": [{
            "id": "il_fixture",
            "amount": 800,
            "currency": "usd",
            "price": "price_fixture",
            "period": {"start": NOW - 30, "end": end}
        }]}
    }))
    .unwrap()
}

fn remote(status: &str, invoices: Vec<Invoice>) -> RemoteSubscription {
    RemoteSubscription {
        subscription: serde_json::from_value(json!({
            "id": "sub_fixture",
            "customer": "cus_fixture",
            "status": status,
            "items": {"data": [{"id": "si_fixture", "price": "price_fixture"}]}
        }))
        .unwrap(),
        invoices,
    }
}

fn anchored() -> ReconcileInput {
    let mut local = local();
    local.live = Some(LiveRow {
        stripe_subscription_id: "sub_fixture".into(),
        stripe_customer_id: "cus_fixture".into(),
        stripe_item_id: "si_fixture".into(),
        stripe_price_id: "price_fixture".into(),
        plan_id: "plus".into(),
        status: "past_due".into(),
        paid_through_unix: Some(NOW),
        paid_invoice_id: Some("in_anchor".into()),
        grace_until_unix: Some(NOW + 60),
    });
    local
}

#[test]
fn incomplete_invoice_envelopes_are_not_finished_pages() {
    for incomplete in [
        json!({}),
        json!({"data": [{"id": "in_fixture"}]}),
        json!({"has_more": false}),
    ] {
        assert!(serde_json::from_value::<InvoiceList>(incomplete).is_err());
    }
    let page: InvoiceList = serde_json::from_value(json!({"data": [], "has_more": false})).unwrap();
    assert!(page.data.is_empty() && !page.has_more);
}

#[test]
fn incomplete_lines_refuse_the_whole_snapshot() {
    let mut incomplete = invoice("in_anchor", NOW);
    incomplete.lines.as_mut().unwrap().has_more = true;
    let mut missing = incomplete.clone();
    missing.lines = None;
    for (status, local, expected_end) in [
        ("active", local(), None),
        ("canceled", anchored(), Some(NOW)),
    ] {
        for bad in [incomplete.clone(), missing.clone()] {
            let remote = remote(status, vec![invoice("in_new", NOW + 300), bad]);
            let outcome = reconcile_subscription(&local, &remote, NOW);
            assert_eq!(
                outcome.fail_closed,
                Some(FailClosedReason::IncompleteInvoiceLines)
            );
            assert!(outcome.explicit_stale && outcome.history.is_empty());
            assert_eq!(outcome.row.paid_through_unix, expected_end);
        }
    }
}

#[test]
fn cancel_and_replay_require_the_bound_paid_anchor() {
    let mut local = anchored();
    let anchor = invoice("in_anchor", NOW);
    let mut foreign = anchor.clone();
    foreign.subscription = Some("sub_other".into());
    let mut unpaid = anchor.clone();
    unpaid.status = "open".into();
    let mut unconfirmed = anchor.clone();
    unconfirmed.status_transitions = None;
    for invalid in [foreign, unpaid, unconfirmed] {
        let stale = reconcile_subscription(&local, &remote("canceled", vec![invalid]), NOW);
        assert!(stale.explicit_stale);
        assert_eq!(stale.row.status, "past_due");
        assert_eq!(stale.row.grace_until_unix, Some(NOW + 60));
        assert!(stale.history.is_empty());
    }

    let complete = remote("canceled", vec![anchor]);
    let canceled = reconcile_subscription(&local, &complete, NOW);
    assert!(!canceled.explicit_stale && canceled.fail_closed.is_none());
    assert_eq!(canceled.row.status, "canceled");
    assert_eq!(canceled.row.grace_until_unix, None);
    let mut malformed = complete.clone();
    malformed.subscription.items.as_mut().unwrap().data[0]
        .id
        .clear();
    let refused = reconcile_subscription(&local, &malformed, NOW);
    assert_eq!(refused.fail_closed, Some(FailClosedReason::AmbiguousItems));
    assert!(refused.explicit_stale && refused.history.is_empty());

    let committed = local.live.as_mut().unwrap();
    committed.status = canceled.row.status.clone();
    committed.grace_until_unix = canceled.row.grace_until_unix;
    assert_eq!(canceled, reconcile_subscription(&local, &complete, NOW + 1));
}

#[test]
fn reversed_period_and_blank_invoice_id_confer_nothing() {
    for bad in [invoice("in_reversed", NOW - 31), invoice(" ", NOW + 300)] {
        let folded = reconcile_subscription(&local(), &remote("active", vec![bad]), NOW);
        assert_eq!(folded.row.paid_through_unix, None);
        assert!(folded.history.is_empty());
    }
}

#[test]
fn sweep_rejects_non_integer_and_out_of_range_milliseconds() {
    for input in [f64::MAX, 3_599_999.5, 8_640_000_000_000_001.0] {
        assert_eq!(scheduled_hour(CRON, input), None);
    }
    assert_eq!(scheduled_hour(CRON, 3_599_999.0), Some(0));
    assert_eq!(scheduled_hour(CRON, 3_600_000.0), Some(1));
}

#[test]
fn contradictory_write_receipts_are_opaque_refusals() {
    let requested = Identity {
        user_id: "private_fixture_owner",
        subscription_id: "private_fixture_subscription",
    };
    for (predicate_stale, other_live_holder, accepted) in [
        (true, false, Accepted::PredicateStaleOwned),
        (false, true, Accepted::LiveSlotSuppressedOwned),
    ] {
        let identity = || Identity {
            user_id: requested.user_id,
            subscription_id: requested.subscription_id,
        };
        let owned = Target::Owned {
            predicate_stale,
            other_live_holder,
            equal_end_provenance_changed: false,
            matching_proof_cancel_conflict: false,
        };
        assert_eq!(classify_write(&requested, &owned, &[], true), Ok(accepted));
        let err = classify_write(&requested, &owned, &[identity()], true).unwrap_err();
        assert_eq!(err, Refused);
        assert_eq!(format!("{err:?}"), "Refused");
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn cancellation_observation_requires_a_decoded_true() {
    use billing_cancel_control::require_blocked;

    assert_eq!(require_blocked(Ok::<_, &str>(true)), Ok(()));
    assert_eq!(
        require_blocked(Ok::<_, &str>(false)),
        Err("negative control backend did not remain blocked")
    );
    assert_eq!(
        require_blocked(Err("synthetic database diagnostic")),
        Err("negative control observation failed")
    );
}
