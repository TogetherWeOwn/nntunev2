//! Read-only payment evidence for invite review. Stripe's Acacia schema
//! exposes the invoice's latest Charge as `invoice.charge`. A paid invoice
//! alone (for example, paid with credit) is not evidence of a card payment.

use serde_json::Value;

const EVIDENCE_API_VERSION: &str = "2024-12-18.acacia";

/// A confirmed observation, not an identity claim. Intentionally opaque:
/// no Debug, serialization, raw-value accessors, or equality implementation.
pub(crate) struct ConfirmedCardPayment<'a> {
    customer: &'a str,
    fingerprint: &'a str,
    livemode: bool,
}

/// Verify an authenticated Stripe Invoice response with `charge` expanded.
/// The caller must supply the actual response version and configured mode,
/// the already reconciled paid invoice ID, and the account's uniquely bound
/// customer ID. Never pass client input or a mutable default payment method.
/// Shapes and bindings are checked here; response provenance is the caller's
/// responsibility. Any missing or unsupported evidence is `None`.
pub(crate) fn confirm_card_payment<'a>(
    response_version: &str,
    livemode: bool,
    expected_invoice: &str,
    bound_customer: &str,
    invoice: &'a Value,
) -> Option<ConfirmedCardPayment<'a>> {
    let customer = reference(invoice.get("customer")?, "customer", "cus_")?;
    if response_version != EVIDENCE_API_VERSION
        || invoice.get("object")?.as_str()? != "invoice"
        || stripe_id(invoice.get("id")?.as_str()?, "in_")? != expected_invoice
        || customer != bound_customer
        || invoice.get("livemode")?.as_bool()? != livemode
        || invoice.get("status")?.as_str()? != "paid"
        || invoice.get("paid_out_of_band")?.as_bool()?
        || invoice.get("amount_paid")?.as_u64()? == 0
        || invoice.pointer("/status_transitions/paid_at")?.as_u64()? == 0
    {
        return None;
    }

    let currency = invoice.get("currency")?.as_str()?;
    let charge = invoice.get("charge")?;
    let captured = charge.get("amount_captured")?.as_u64()?;
    stripe_id(charge.get("id")?.as_str()?, "ch_")?;
    if currency.len() != 3
        || !currency.bytes().all(|b| b.is_ascii_lowercase())
        || charge.get("currency")?.as_str()? != currency
        || charge.get("object")?.as_str()? != "charge"
        || reference(charge.get("invoice")?, "invoice", "in_")? != expected_invoice
        || reference(charge.get("customer")?, "customer", "cus_")? != customer
        || charge.get("livemode")?.as_bool()? != livemode
        || charge.get("status")?.as_str()? != "succeeded"
        || !charge.get("paid")?.as_bool()?
        || !charge.get("captured")?.as_bool()?
        || captured == 0
        || captured > charge.get("amount")?.as_u64()?
        || charge.get("amount_refunded")?.as_u64()? != 0
        || charge.get("refunded")?.as_bool()?
        || charge.get("disputed")?.as_bool()?
        || charge.pointer("/payment_method_details/type")?.as_str()? != "card"
    {
        return None;
    }

    let fingerprint = charge
        .pointer("/payment_method_details/card/fingerprint")?
        .as_str()?;
    opaque_token(fingerprint).then_some(ConfirmedCardPayment {
        customer,
        fingerprint,
        livemode,
    })
}

fn reference<'a>(value: &'a Value, object: &str, prefix: &str) -> Option<&'a str> {
    let id = match value.as_str() {
        Some(id) => id,
        None if value.get("object")?.as_str()? == object
            && value
                .get("deleted")
                .is_none_or(|v| v.as_bool() == Some(false)) =>
        {
            value.get("id")?.as_str()?
        }
        None => return None,
    };
    stripe_id(id, prefix)
}

fn stripe_id<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    opaque_token(value.strip_prefix(prefix)?).then_some(value)
}

fn opaque_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PaymentSharing {
    SharedCustomer,
    SharedCardFingerprint,
    NoMatch,
    Unknown,
}

/// Compare only observations from the same Stripe account. `SharedCustomer`
/// describes equal verified customer IDs, not two users sharing a current
/// billing binding: the unique binding rules that out. For distinct users
/// with current bindings, only the card fingerprint can match.
/// A match is a review hint, not grounds for enforcement; a non-match does
/// not rule out a shared funding card (wallets can use different tokens).
pub(crate) fn compare(
    issuer: Option<&ConfirmedCardPayment<'_>>,
    invitee: Option<&ConfirmedCardPayment<'_>>,
) -> PaymentSharing {
    match (issuer, invitee) {
        (Some(a), Some(b)) if a.livemode == b.livemode => {
            if a.customer == b.customer {
                PaymentSharing::SharedCustomer
            } else if a.fingerprint == b.fingerprint {
                PaymentSharing::SharedCardFingerprint
            } else {
                PaymentSharing::NoMatch
            }
        }
        _ => PaymentSharing::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stripe::STRIPE_API_VERSION;
    use serde_json::json;

    fn paid_invoice(customer: &str, id: &str, fingerprint: &str, livemode: bool) -> Value {
        json!({
            "object": "invoice", "id": id, "customer": customer, "livemode": livemode,
            "status": "paid", "paid_out_of_band": false, "amount_paid": 1200, "currency": "usd",
            "status_transitions": {"paid_at": 1735000000},
            "charge": {
                "object": "charge", "id": "ch_Synthetic", "invoice": id,
                "customer": {"object": "customer", "id": customer}, "livemode": livemode,
                "status": "succeeded", "paid": true, "captured": true,
                "amount": 1200, "currency": "usd", "amount_captured": 1200,
                "amount_refunded": 0, "refunded": false, "disputed": false,
                "payment_method_details": {"type": "card", "card": {"fingerprint": fingerprint}}
            }
        })
    }

    fn confirm(invoice: &Value) -> Option<ConfirmedCardPayment<'_>> {
        confirm_card_payment(
            STRIPE_API_VERSION,
            false,
            "in_Issuer",
            "cus_Issuer",
            invoice,
        )
    }

    #[test]
    fn confirmed_observations_compare_without_exposing_identifiers() {
        let issuer = paid_invoice("cus_Issuer", "in_Issuer", "SyntheticCardA", false);
        let issuer = confirm(&issuer).expect("confirmed issuer");
        for (customer, fingerprint, live, expected) in [
            (
                "cus_Issuer",
                "SyntheticCardB",
                false,
                PaymentSharing::SharedCustomer,
            ),
            (
                "cus_Invitee",
                "SyntheticCardA",
                false,
                PaymentSharing::SharedCardFingerprint,
            ),
            (
                "cus_Invitee",
                "SyntheticCarda",
                false,
                PaymentSharing::NoMatch,
            ),
            (
                "cus_Invitee",
                "SyntheticCardA",
                true,
                PaymentSharing::Unknown,
            ),
        ] {
            let invoice = paid_invoice(customer, "in_Invitee", fingerprint, live);
            let invitee =
                confirm_card_payment(STRIPE_API_VERSION, live, "in_Invitee", customer, &invoice)
                    .expect("confirmed invitee");
            assert_eq!(compare(Some(&issuer), Some(&invitee)), expected);
        }
        assert_eq!(compare(Some(&issuer), None), PaymentSharing::Unknown);
        assert_eq!(compare(None, Some(&issuer)), PaymentSharing::Unknown);
        assert_eq!(compare(None, None), PaymentSharing::Unknown);
    }

    #[test]
    fn rejects_missing_malformed_unsupported_or_unconfirmed_evidence() {
        let fixture = paid_invoice("cus_Issuer", "in_Issuer", "SyntheticCardA", false);
        for (path, replacement) in [
            ("/charge", Value::Null),
            ("/charge", json!("ch_Unexpanded")),
            ("/charge/object", json!("payment_method")),
            ("/charge/id", json!("ch_")),
            ("/status", json!("open")),
            ("/status_transitions", json!({})),
            ("/amount_paid", json!(0)),
            ("/paid_out_of_band", json!(true)),
            ("/charge/status", json!("pending")),
            ("/charge/status", json!("failed")),
            ("/charge/paid", json!(false)),
            ("/charge/captured", json!(false)),
            ("/charge/amount_captured", json!(0)),
            ("/charge/amount_captured", json!(1201)),
            ("/charge/currency", json!("eur")),
            ("/currency", json!("USD")),
            ("/charge/amount_refunded", json!(1)),
            ("/charge/refunded", json!(true)),
            ("/charge/disputed", json!(true)),
            (
                "/charge/payment_method_details/type",
                json!("us_bank_account"),
            ),
            ("/charge/payment_method_details/card", json!({})),
            ("/charge/payment_method_details/card/fingerprint", json!(42)),
            ("/charge/payment_method_details/card/fingerprint", json!("")),
            (
                "/charge/payment_method_details/card/fingerprint",
                json!(" SyntheticCardA"),
            ),
        ] {
            let mut invoice = fixture.clone();
            *invoice.pointer_mut(path).unwrap() = replacement;
            assert!(confirm(&invoice).is_none(), "accepted {path}");
        }
        let mut invoice = fixture;
        invoice["default_payment_method"] = json!({"card": {"fingerprint": "SyntheticCardA"}});
        invoice["charge"]["payment_method_details"] = Value::Null;
        assert!(confirm(&invoice).is_none());
    }

    #[test]
    fn rejects_mismatched_customer_invoice_version_or_mode() {
        let fixture = paid_invoice("cus_Issuer", "in_Issuer", "SyntheticCardA", false);
        for (path, replacement) in [
            ("/customer", json!("cus_Other")),
            ("/customer", Value::Null),
            ("/charge/customer/id", json!("cus_Other")),
            ("/charge/customer/object", json!("charge")),
            ("/charge/customer", Value::Null),
            (
                "/customer",
                json!({"object": "customer", "id": "cus_Issuer", "deleted": true}),
            ),
            ("/id", json!("in_Other")),
            ("/charge/invoice", json!("in_Other")),
            ("/livemode", json!(true)),
            ("/charge/livemode", json!(true)),
        ] {
            let mut invoice = fixture.clone();
            *invoice.pointer_mut(path).unwrap() = replacement;
            assert!(confirm(&invoice).is_none(), "accepted {path}");
        }
        assert!(
            confirm_card_payment("other-version", false, "in_Issuer", "cus_Issuer", &fixture)
                .is_none()
        );
        assert!(confirm_card_payment(
            STRIPE_API_VERSION,
            true,
            "in_Issuer",
            "cus_Issuer",
            &fixture
        )
        .is_none());
    }
}
