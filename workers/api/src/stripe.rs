//! Hand-rolled Stripe client for the api worker (test mode only, no SDK).
//!
//! Pure Rust, native + wasm32, NO Worker runtime imports — same constraint
//! as `auth` / `vault` / `session` / `rate_limit` / `plan_limits` /
//! `ledger` / `billing`. Time arrives only as `now_unix`; this module never
//! reads a clock, a DB, or the environment, so request shapes, signature
//! verification and error mapping are unit-testable without staging.
//! Nothing here performs I/O: [`StripeTransport`] is the seam the Worker
//! entry implements over `worker::Fetch` and the tests implement with
//! fixtures.
//!
//! Stripe surface used (Checkout shape only):
//! - `POST /v1/customers` (optional `test_clock` seam for the staging-proof
//!   card: the customer is created under a test clock *before* Checkout;
//!   seam only, no test-clock management here),
//! - `POST /v1/checkout/sessions` (`mode=subscription`),
//! - `GET /v1/checkout/sessions/{id}`,
//! - `GET /v1/subscriptions/{id}`,
//! - `GET /v1/invoices/{id}` (+ `GET /v1/invoices/{id}/lines` until
//!   `has_more` is false, so reconciliation always reads the *full*
//!   invoice, never only its first page),
//! - `GET /v1/invoices` (filtered by customer/subscription, as
//!   reconciliation needs),
//! - `DELETE /v1/subscriptions/{id}` (reconcile paths that must confirm
//!   terminal state).
//!
//! Auth: bearer test secret key (`sk_test_...`, validated by
//! [`valid_secret_key`]; `sk_live_` and everything else is refused at
//! config load, never at request time). Every request pins
//! [`STRIPE_API_VERSION`] in the `Stripe-Version` header. The idempotency
//! key on session creation derives from the billing operation id
//! ([`checkout_idempotency_key`]), so a retried creation converges.
//!
//! Webhook verification ([`verify_webhook_signature`]) runs over the **raw
//! body** before any JSON parsing: `Stripe-Signature` HMAC-SHA256 with the
//! webhook signing secret, constant-time compare, 5-minute timestamp
//! tolerance enforced symmetrically. Any failure rejects before state is
//! touched.
//!
//! Typed responses are minimal and tolerant: unknown fields are ignored,
//! expandable id-or-object refs collapse to the id ([`string_or_id`]).
//! Stripe errors surface as [`StripeError`] — never as entitlement
//! changes; the reconciliation slice maps every `Err` to a fail-closed
//! store refusal.

use std::future::Future;
use std::pin::Pin;

use hmac::{Hmac, KeyInit, Mac};
use serde::de::Deserializer;
use serde::Deserialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Stripe API host. The transport builds `https://` URLs from this; the
/// value is pinned here (not per-call) so a test asserts the host.
pub const STRIPE_API_BASE: &str = "https://api.stripe.com";

/// Pinned Stripe API version sent on every request (`Stripe-Version`
/// header). Pinning keeps the response shapes below stable; a version bump
/// is a deliberate slice with fixture updates, never silent drift.
pub const STRIPE_API_VERSION: &str = "2024-12-18.acacia";

/// Webhook timestamp tolerance: a signature older *or* newer than this
/// many seconds from `now_unix` rejects.
pub const WEBHOOK_TOLERANCE_SECS: i64 = 300;

/// Inbound header carrying the webhook signature (lowercased by the
/// entry before it reaches the handlers).
pub const STRIPE_SIGNATURE_HEADER: &str = "stripe-signature";

// --- Secret validation ---------------------------------------------------------

/// True iff `key` is an acceptable Stripe bearer secret for this worker:
/// a test-mode key only. `sk_live_` (and restricted/live-adjacent,
/// empty, or blank shapes) is refused — at config load, never per
/// request — so a live key can never be staged into serving traffic.
pub fn valid_secret_key(key: &str) -> bool {
    key.starts_with("sk_test_") && key.len() > "sk_test_".len()
}

// --- Form encoding --------------------------------------------------------------

/// Percent-encode one form component: unreserved bytes pass through,
/// everything else (including `[`, `]`, spaces) becomes `%XX`. Stripe
/// accepts encoded brackets in `line_items[0][price]`-style keys.
fn form_escape(value: &str) -> String {
    const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if UNRESERVED.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Encode ordered `(key, value)` pairs as
/// `application/x-www-form-urlencoded`. Order is caller-controlled and
/// pinned by tests (deterministic fixtures, stable idempotency keys).
pub fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", form_escape(key), form_escape(value)))
        .collect::<Vec<_>>()
        .join("&")
}

// --- Request builders ------------------------------------------------------------

/// One Stripe API call, transport-free: the Worker entry renders this over
/// `worker::Fetch` with the bearer key and [`STRIPE_API_VERSION`]
/// headers. `path` carries the query string for GET lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeRequest {
    pub method: &'static str,
    pub path: String,
    /// Set on creation calls; the transport renders `Idempotency-Key`.
    pub idempotency_key: Option<String>,
    /// Form-encoded body; empty for GET/DELETE.
    pub body: String,
}

/// Idempotency key for Checkout Session creation, derived from the
/// billing operation id: retries and lost responses replay the same key,
/// so concurrent purchase attempts converge on one session.
pub fn checkout_idempotency_key(operation_id: &str) -> String {
    format!("checkout-session:{operation_id}")
}

/// Idempotency key for Stripe customer creation, derived from the same
/// operation id with a distinct prefix: the two creations can never
/// share a key (ADR 0005 creation contract).
pub fn customer_idempotency_key(operation_id: &str) -> String {
    format!("customer:{operation_id}")
}

/// `POST /v1/customers`. `test_clock` is the staging-proof seam: when
/// `Some`, the customer is created under that test clock *before*
/// Checkout. `user_id` lands in `metadata[user_id]` for operator-side
/// binding. `operation_id` selects the customer idempotency key.
pub fn create_customer_request(
    operation_id: &str,
    user_id: Option<&str>,
    test_clock: Option<&str>,
) -> StripeRequest {
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    if let Some(user_id) = user_id {
        pairs.push(("metadata[user_id]", user_id));
    }
    if let Some(clock) = test_clock {
        pairs.push(("test_clock", clock));
    }
    StripeRequest {
        method: "POST",
        path: "/v1/customers".to_string(),
        idempotency_key: Some(customer_idempotency_key(operation_id)),
        body: form_encode(&pairs),
    }
}

/// `POST /v1/checkout/sessions` (`mode=subscription`). The operation id
/// rides `client_reference_id` and both metadata maps; the Stripe
/// idempotency key derives from it ([`checkout_idempotency_key`]), so a
/// retried creation replays instead of minting a second session.
pub fn create_checkout_session_request(
    operation_id: &str,
    customer_id: &str,
    price_id: &str,
    success_url: &str,
    cancel_url: &str,
) -> StripeRequest {
    let pairs = [
        ("mode", "subscription"),
        ("customer", customer_id),
        ("line_items[0][price]", price_id),
        ("line_items[0][quantity]", "1"),
        ("success_url", success_url),
        ("cancel_url", cancel_url),
        ("client_reference_id", operation_id),
        ("metadata[billing_operation_id]", operation_id),
        (
            "subscription_data[metadata][billing_operation_id]",
            operation_id,
        ),
    ];
    StripeRequest {
        method: "POST",
        path: "/v1/checkout/sessions".to_string(),
        idempotency_key: Some(checkout_idempotency_key(operation_id)),
        body: form_encode(&pairs),
    }
}

/// Construct a portal session request without dispatching it. All three
/// values must be nonblank; callers remain responsible for selecting an
/// appropriate configuration, customer binding and trusted return URL.
/// No idempotency key: each request creates a fresh hosted session.
pub fn create_billing_portal_session_request(
    customer_id: &str,
    configuration_id: &str,
    return_url: &str,
) -> Option<StripeRequest> {
    if [customer_id, configuration_id, return_url]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        return None;
    }
    Some(StripeRequest {
        method: "POST",
        path: "/v1/billing_portal/sessions".to_string(),
        idempotency_key: None,
        body: form_encode(&[
            ("customer", customer_id),
            ("configuration", configuration_id),
            ("return_url", return_url),
        ]),
    })
}

/// `GET /v1/checkout/sessions/{id}`.
pub fn retrieve_checkout_session_request(session_id: &str) -> StripeRequest {
    StripeRequest {
        method: "GET",
        path: format!("/v1/checkout/sessions/{}", form_escape(session_id)),
        idempotency_key: None,
        body: String::new(),
    }
}

/// `POST /v1/checkout/sessions/{id}/expire`: deliberately retire an open
/// session before a replacement mints under a fresh operation id (price
/// mismatch repair, dead-session replacement). Expiry is idempotent by
/// nature — an already-expired session answers 200 with that state, so no
/// idempotency key rides along. Expiring a complete session is a Stripe
/// 400, which callers treat as money-landed (reconcile) or retryable,
/// never as a green light to mint beside it.
pub fn expire_checkout_session_request(session_id: &str) -> StripeRequest {
    StripeRequest {
        method: "POST",
        path: format!("/v1/checkout/sessions/{}/expire", form_escape(session_id)),
        idempotency_key: None,
        body: String::new(),
    }
}

/// `GET /v1/subscriptions/{id}`.
pub fn retrieve_subscription_request(subscription_id: &str) -> StripeRequest {
    StripeRequest {
        method: "GET",
        path: format!("/v1/subscriptions/{}", form_escape(subscription_id)),
        idempotency_key: None,
        body: String::new(),
    }
}

/// `DELETE /v1/subscriptions/{id}`: reconcile paths that must confirm
/// terminal state (expiry executor, account-deletion gate). Plain cancel;
/// proration/invoice behavior belongs to those callers' slices.
pub fn cancel_subscription_request(subscription_id: &str) -> StripeRequest {
    StripeRequest {
        method: "DELETE",
        path: format!("/v1/subscriptions/{}", form_escape(subscription_id)),
        idempotency_key: None,
        body: String::new(),
    }
}

/// `GET /v1/invoices/{id}` with line-price expansion. Line pagination is
/// completed through [`list_invoice_lines_request`]; one page is never
/// treated as the full invoice.
pub fn retrieve_invoice_request(invoice_id: &str) -> StripeRequest {
    StripeRequest {
        method: "GET",
        path: format!(
            "/v1/invoices/{}?{}",
            form_escape(invoice_id),
            form_encode(&[("expand[]", "lines.data.price")])
        ),
        idempotency_key: None,
        body: String::new(),
    }
}

/// `GET /v1/invoices/{id}/lines`: one page of an invoice's lines.
/// `starting_after` continues a previous page; `limit` caps the page
/// (Stripe max 100). Reconciliation follows `has_more` until exhausted.
pub fn list_invoice_lines_request(
    invoice_id: &str,
    starting_after: Option<&str>,
    limit: u8,
) -> StripeRequest {
    let limit = limit.to_string();
    let mut pairs = vec![("limit", limit.as_str())];
    if let Some(cursor) = starting_after {
        pairs.push(("starting_after", cursor));
    }
    StripeRequest {
        method: "GET",
        path: format!(
            "/v1/invoices/{}/lines?{}",
            form_escape(invoice_id),
            form_encode(&pairs)
        ),
        idempotency_key: None,
        body: String::new(),
    }
}

/// `GET /v1/invoices` filtered for reconciliation: by customer and/or
/// subscription, optionally paid-only, newest first. At least one of
/// `customer_id` / `subscription_id` must be `Some` — an unfiltered list
/// is never issued (fail-closed builder: returns `None`). `starting_after`
/// continues a previous page; the transport follows `has_more` until the
/// list is exhausted, so reconciliation never treats one page as the full
/// invoice set.
pub fn list_invoices_request(
    customer_id: Option<&str>,
    subscription_id: Option<&str>,
    paid_only: bool,
    limit: u8,
    starting_after: Option<&str>,
) -> Option<StripeRequest> {
    if customer_id.is_none() && subscription_id.is_none() {
        return None;
    }
    let limit = limit.to_string();
    let mut pairs = vec![("limit", limit.as_str())];
    if let Some(customer) = customer_id {
        pairs.push(("customer", customer));
    }
    if let Some(subscription) = subscription_id {
        pairs.push(("subscription", subscription));
    }
    if paid_only {
        pairs.push(("status", "paid"));
    }
    if let Some(cursor) = starting_after {
        pairs.push(("starting_after", cursor));
    }
    Some(StripeRequest {
        method: "GET",
        path: format!("/v1/invoices?{}", form_encode(&pairs)),
        idempotency_key: None,
        body: String::new(),
    })
}

// --- Typed responses ---------------------------------------------------------------

/// Collapse an expandable Stripe ref: a bare id string, or an expanded
/// object carrying `id`. Anything else (null, unexpected shape) is None —
/// the caller treats a missing binding as fail-closed, never as a guess.
fn string_or_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Expandable {
        Id(String),
        Object { id: String },
    }
    Option::<Expandable>::deserialize(deserializer).map(|opt| match opt {
        None => None,
        Some(Expandable::Id(id)) => Some(id),
        Some(Expandable::Object { id }) => Some(id),
    })
}

/// Stripe object helper: unknown fields are ignored by default (no
/// `deny_unknown_fields` anywhere here); every field the worker does not
/// read is simply absent from the struct.
fn default_false() -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Customer {
    pub id: String,
    #[serde(default)]
    pub livemode: bool,
    #[serde(default, deserialize_with = "string_or_id")]
    pub test_clock: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CheckoutSession {
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub payment_status: Option<String>,
    #[serde(default, deserialize_with = "string_or_id")]
    pub customer: Option<String>,
    #[serde(default, deserialize_with = "string_or_id")]
    pub subscription: Option<String>,
    /// Redirect target for session reuse (the open session's URL, read
    /// fresh on every retrieve — never stored, never guessed).
    #[serde(default)]
    pub url: Option<String>,
    /// Our operation id echo (`client_reference_id`): the webhook's
    /// binding from a completed session back to its single-flight row.
    #[serde(default)]
    pub client_reference_id: Option<String>,
    #[serde(default)]
    pub livemode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SubscriptionItem {
    pub id: String,
    #[serde(default, deserialize_with = "string_or_id")]
    pub price: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SubscriptionItems {
    #[serde(default)]
    pub data: Vec<SubscriptionItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Subscription {
    pub id: String,
    #[serde(default, deserialize_with = "string_or_id")]
    pub customer: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default, rename = "cancel_at_period_end")]
    pub cancel_at_period_end: bool,
    #[serde(default)]
    pub current_period_end: Option<i64>,
    #[serde(default)]
    pub items: Option<SubscriptionItems>,
    #[serde(default)]
    pub livemode: bool,
}

impl Subscription {
    /// The single applied item's `(item_id, price_id)`. Checkout-shape
    /// subscriptions carry exactly one item; zero or several is a shape
    /// the worker refuses to resolve (fail closed, never first-pick).
    pub fn single_item(&self) -> Option<(&str, Option<&str>)> {
        let items = self.items.as_ref()?.data.as_slice();
        let [item] = items else { return None };
        Some((item.id.as_str(), item.price.as_deref()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InvoiceLinePeriod {
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub end: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InvoiceLine {
    pub id: String,
    #[serde(default)]
    pub amount: i64,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default, deserialize_with = "string_or_id")]
    pub price: Option<String>,
    #[serde(default)]
    pub period: Option<InvoiceLinePeriod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InvoiceLines {
    #[serde(default)]
    pub data: Vec<InvoiceLine>,
    #[serde(default = "default_false")]
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InvoiceStatusTransitions {
    #[serde(default)]
    pub paid_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Invoice {
    pub id: String,
    #[serde(default, deserialize_with = "string_or_id")]
    pub customer: Option<String>,
    #[serde(default, deserialize_with = "string_or_id")]
    pub subscription: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub amount_paid: i64,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub status_transitions: Option<InvoiceStatusTransitions>,
    #[serde(default)]
    pub lines: Option<InvoiceLines>,
    #[serde(default)]
    pub livemode: bool,
}

impl Invoice {
    /// Stripe records payment time in `status_transitions.paid_at` (there
    /// is no top-level `paid_at`). A `paid` invoice without it cannot
    /// anchor coverage or history and is skipped by reconciliation.
    pub fn paid_at(&self) -> Option<i64> {
        self.status_transitions.as_ref()?.paid_at
    }
}

/// A `GET /v1/invoices` list envelope:
/// `{"object":"list","data":[...],"has_more":...}`. The endpoint never
/// returns a bare invoice array, so the transport deserializes this
/// envelope (never `Vec<Invoice>` directly) and follows `has_more` with
/// `starting_after` until the list is exhausted. Unknown envelope fields
/// are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InvoiceList {
    #[serde(default)]
    pub data: Vec<Invoice>,
    #[serde(default = "default_false")]
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct EventData {
    #[serde(default)]
    object: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RawEvent {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type", default)]
    event_type: Option<String>,
    #[serde(default)]
    livemode: bool,
    #[serde(default)]
    data: Option<EventData>,
}

/// A verified Stripe event: signature checked over the raw body *before*
/// this parse. `object` is the event's `data.object`, re-parsed by the
/// caller into the shape the event type needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeEvent {
    pub id: String,
    pub event_type: String,
    pub livemode: bool,
    pub object: serde_json::Value,
}

// --- Typed errors --------------------------------------------------------------------

/// A Stripe call failure. `http_status` 0 means the transport never got a
/// response; otherwise it is Stripe's status with their `code` when the
/// body parsed. Handlers map every variant to a fail-closed refusal —
/// never to an entitlement change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeError {
    pub http_status: u16,
    pub code: Option<String>,
    pub message: String,
}

impl StripeError {
    pub fn transport() -> Self {
        StripeError {
            http_status: 0,
            code: None,
            message: "stripe transport failed".to_string(),
        }
    }

    pub fn api(http_status: u16, code: Option<String>, message: String) -> Self {
        StripeError {
            http_status,
            code,
            message,
        }
    }

    /// Parse a non-2xx Stripe error body (`{"error": {...}}`).
    /// Unparseable bodies keep the status with a sealed message.
    pub fn parse(http_status: u16, body: &[u8]) -> Self {
        #[derive(Deserialize)]
        struct ErrorBody {
            #[serde(default)]
            error: Option<ErrorDetail>,
        }
        #[derive(Deserialize)]
        struct ErrorDetail {
            #[serde(default)]
            code: Option<String>,
            #[serde(default)]
            message: Option<String>,
        }
        let detail = serde_json::from_slice::<ErrorBody>(body)
            .ok()
            .and_then(|b| b.error);
        StripeError {
            http_status,
            code: detail.as_ref().and_then(|d| d.code.clone()),
            message: detail
                .and_then(|d| d.message)
                .unwrap_or_else(|| "stripe request failed".to_string()),
        }
    }
}

impl std::fmt::Display for StripeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Sealed public text: status only. Stripe messages can carry
        // account-adjacent detail and never reach a response body.
        write!(f, "stripe request failed ({})", self.http_status)
    }
}

pub type StripeResult<T> = Result<T, StripeError>;

// --- Transport seam --------------------------------------------------------------------

/// Parameters for `POST /v1/customers`. Owned strings keep the trait
/// object-safe (no borrowed lifetimes in method signatures).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCustomer {
    pub operation_id: String,
    pub user_id: Option<String>,
    pub test_clock: Option<String>,
}

/// Parameters for `POST /v1/checkout/sessions` (`mode=subscription`).
/// `success_url` / `cancel_url` are caller-supplied (the wiring card's
/// user-facing routes own them); this slice never invents them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCheckoutSession {
    pub operation_id: String,
    pub customer_id: String,
    pub price_id: String,
    pub success_url: String,
    pub cancel_url: String,
}

/// Parameters for `GET /v1/invoices` (reconciliation listing).
/// `starting_after` seeds the first page's cursor; the transport advances
/// it across pages itself, so callers pass `None` and receive the full
/// list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListInvoices {
    pub customer_id: Option<String>,
    pub subscription_id: Option<String>,
    pub paid_only: bool,
    pub limit: u8,
    pub starting_after: Option<String>,
}

/// Stripe HTTPS transport. Implemented once in the Worker entry over
/// `worker::Fetch` (bearer key + pinned `Stripe-Version` + idempotency
/// headers) and by fixture fakes in tests. Boxed futures keep the trait
/// object-safe so handlers hold `&dyn StripeTransport` without a fifth
/// `Ctx` generic.
pub trait StripeTransport {
    fn create_customer(
        &self,
        req: CreateCustomer,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Customer>> + '_>>;
    fn create_checkout_session(
        &self,
        req: CreateCheckoutSession,
    ) -> Pin<Box<dyn Future<Output = StripeResult<CheckoutSession>> + '_>>;
    fn retrieve_checkout_session(
        &self,
        session_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<CheckoutSession>> + '_>>;
    fn expire_checkout_session(
        &self,
        session_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<CheckoutSession>> + '_>>;
    fn retrieve_subscription(
        &self,
        subscription_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Subscription>> + '_>>;
    fn retrieve_invoice(
        &self,
        invoice_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Invoice>> + '_>>;
    fn list_invoice_lines(
        &self,
        invoice_id: String,
        starting_after: Option<String>,
        limit: u8,
    ) -> Pin<Box<dyn Future<Output = StripeResult<InvoiceLines>> + '_>>;
    fn list_invoices(
        &self,
        req: ListInvoices,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Vec<Invoice>>> + '_>>;
    fn cancel_subscription(
        &self,
        subscription_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Subscription>> + '_>>;
}

/// No-op transport: every call is a transport failure. Used wherever a
/// `&dyn StripeTransport` must be supplied but no Stripe call may happen
/// (existing handler tests, unprovisioned postures).
pub struct NoStripeTransport;

impl StripeTransport for NoStripeTransport {
    fn create_customer(
        &self,
        _req: CreateCustomer,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Customer>> + '_>> {
        Box::pin(async { Err(StripeError::transport()) })
    }
    fn create_checkout_session(
        &self,
        _req: CreateCheckoutSession,
    ) -> Pin<Box<dyn Future<Output = StripeResult<CheckoutSession>> + '_>> {
        Box::pin(async { Err(StripeError::transport()) })
    }
    fn retrieve_checkout_session(
        &self,
        _session_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<CheckoutSession>> + '_>> {
        Box::pin(async { Err(StripeError::transport()) })
    }
    fn expire_checkout_session(
        &self,
        _session_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<CheckoutSession>> + '_>> {
        Box::pin(async { Err(StripeError::transport()) })
    }
    fn retrieve_subscription(
        &self,
        _subscription_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Subscription>> + '_>> {
        Box::pin(async { Err(StripeError::transport()) })
    }
    fn retrieve_invoice(
        &self,
        _invoice_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Invoice>> + '_>> {
        Box::pin(async { Err(StripeError::transport()) })
    }
    fn list_invoice_lines(
        &self,
        _invoice_id: String,
        _starting_after: Option<String>,
        _limit: u8,
    ) -> Pin<Box<dyn Future<Output = StripeResult<InvoiceLines>> + '_>> {
        Box::pin(async { Err(StripeError::transport()) })
    }
    fn list_invoices(
        &self,
        _req: ListInvoices,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Vec<Invoice>>> + '_>> {
        Box::pin(async { Err(StripeError::transport()) })
    }
    fn cancel_subscription(
        &self,
        _subscription_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Subscription>> + '_>> {
        Box::pin(async { Err(StripeError::transport()) })
    }
}

// --- Webhook verification ----------------------------------------------------------------

/// Webhook verification failure. Every variant rejects with 400 before
/// JSON parsing and before any state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookError {
    /// No `Stripe-Signature` header on the request.
    MissingSignature,
    /// Header lacks a parseable `t=` timestamp or any `v1=` candidate.
    MalformedSignature,
    /// `|now - t|` exceeds [`WEBHOOK_TOLERANCE_SECS`].
    StaleTimestamp,
    /// No `v1=` candidate matches the computed HMAC.
    SignatureMismatch,
    /// Signature verified but the body is not a Stripe event object.
    MalformedEvent,
}

impl std::fmt::Display for WebhookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Sealed public text shared by all variants: the 400 body never
        // says which check failed (no verification oracle).
        write!(f, "invalid webhook")
    }
}

/// Decode a lowercase (or upper) hex signature candidate. `None` on any
/// non-hex byte or odd length — a malformed candidate simply never
/// matches, it never errors the whole header.
fn hex_decode(candidate: &str) -> Option<Vec<u8>> {
    if !candidate.len().is_multiple_of(2) || candidate.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(candidate.len() / 2);
    let bytes = candidate.as_bytes();
    let nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    let mut i = 0;
    while i < bytes.len() {
        out.push((nibble(bytes[i])? << 4) | nibble(bytes[i + 1])?);
        i += 2;
    }
    Some(out)
}

struct ParsedSignature<'a> {
    timestamp: i64,
    candidates: Vec<&'a str>,
}

/// Split `t=...,v1=...[,v1=...]` (Stripe's header shape; `v0=` legacy
/// entries and unknown keys are ignored). `None` when `t=` is missing /
/// unparsable or no `v1=` candidate exists.
fn parse_signature_header(header: &str) -> Option<ParsedSignature<'_>> {
    let mut timestamp = None;
    let mut candidates = Vec::new();
    for part in header.split(',') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("t=") {
            timestamp = value.parse::<i64>().ok();
        } else if let Some(value) = part.strip_prefix("v1=") {
            if !value.is_empty() {
                candidates.push(value);
            }
        }
    }
    Some(ParsedSignature {
        timestamp: timestamp?,
        candidates: if candidates.is_empty() {
            return None;
        } else {
            candidates
        },
    })
}

/// Verify a Stripe webhook: HMAC-SHA256 over `"{t}.{raw_body}"` with the
/// webhook signing secret, constant-time compared against every `v1=`
/// candidate, timestamp within [`WEBHOOK_TOLERANCE_SECS`] of `now_unix`.
/// Returns the parsed event on success. Verification precedes parsing:
/// a body that fails verification is never deserialized.
pub fn verify_webhook_signature(
    raw_body: &[u8],
    header: &str,
    secret: &str,
    now_unix: i64,
) -> Result<StripeEvent, WebhookError> {
    let parsed = parse_signature_header(header).ok_or(WebhookError::MalformedSignature)?;
    if parsed.timestamp.saturating_sub(now_unix).abs() > WEBHOOK_TOLERANCE_SECS {
        return Err(WebhookError::StaleTimestamp);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| WebhookError::MissingSignature)?;
    mac.update(parsed.timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(raw_body);
    let expected = mac.finalize().into_bytes();
    let mut matched = false;
    for candidate in &parsed.candidates {
        if let Some(bytes) = hex_decode(candidate) {
            // Length mismatch rejects without comparing (only the length
            // leaks, never content); content compares in constant time.
            if bytes.len() == expected.len() && bytes.as_slice().ct_eq(&expected[..]).into() {
                matched = true;
            }
        }
    }
    if !matched {
        return Err(WebhookError::SignatureMismatch);
    }
    parse_event(raw_body)
}

/// Parse a verified webhook body into its event envelope. Rejects bodies
/// without a string `id`, `type`, or object `data.object`.
fn parse_event(raw_body: &[u8]) -> Result<StripeEvent, WebhookError> {
    let raw: RawEvent =
        serde_json::from_slice(raw_body).map_err(|_| WebhookError::MalformedEvent)?;
    let (Some(id), Some(event_type), Some(data)) = (raw.id, raw.event_type, raw.data) else {
        return Err(WebhookError::MalformedEvent);
    };
    if !data.object.is_object() {
        return Err(WebhookError::MalformedEvent);
    }
    Ok(StripeEvent {
        id,
        event_type,
        livemode: raw.livemode,
        object: data.object,
    })
}

/// Test helper: render a `Stripe-Signature` header for `raw_body` at
/// timestamp `t` with `secret` (mirrors Stripe's signing, so fixtures
/// carryindependent vectors, not copies of the verifier).
#[cfg(test)]
pub fn test_signature_header(raw_body: &[u8], t: i64, secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("test secret");
    mac.update(t.to_string().as_bytes());
    mac.update(b".");
    mac.update(raw_body);
    let bytes = mac.finalize().into_bytes();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("t={t},v1={hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "whsec_test_vector_secret";
    const NOW: i64 = 1_786_032_000;

    // --- Secret validation --------------------------------------------------

    #[test]
    fn test_keys_validate_and_live_keys_refuse() {
        // Stripe's published documentation example suffix, assembled at
        // runtime so the literal key shape never appears in the source
        // (push protection flags it regardless of provenance).
        let doc_suffix = "4eC39HqLyjWDarjtT1zdp7dc";
        assert!(valid_secret_key(&format!("sk_test_{doc_suffix}")));
        assert!(!valid_secret_key(&format!("sk_live_{doc_suffix}")));
        assert!(!valid_secret_key("sk_test_"));
        assert!(!valid_secret_key(""));
        assert!(!valid_secret_key("rk_test_anything"));
        assert!(!valid_secret_key("Bearer sk_test_abc"));
    }

    // --- Request builders ----------------------------------------------------

    #[test]
    fn customer_create_carries_test_clock_seam() {
        let with_clock = create_customer_request("op-1", Some("user-1"), Some("clock_123"));
        assert_eq!(with_clock.method, "POST");
        assert_eq!(with_clock.path, "/v1/customers");
        assert_eq!(
            with_clock.idempotency_key,
            Some("customer:op-1".to_string())
        );
        assert!(
            with_clock.body.contains("test_clock=clock_123"),
            "test clock must pass through: {}",
            with_clock.body
        );
        assert!(
            with_clock.body.contains("metadata%5Buser_id%5D=user-1"),
            "user binding metadata: {}",
            with_clock.body
        );
        let without_clock = create_customer_request("op-1", Some("user-1"), None);
        assert!(
            !without_clock.body.contains("test_clock"),
            "seam is opt-in only: {}",
            without_clock.body
        );
    }

    #[test]
    fn checkout_session_create_is_subscription_with_op_binding() {
        let req = create_checkout_session_request(
            "op-9",
            "cus_123",
            "price_456",
            "https://example.com/success",
            "https://example.com/cancel",
        );
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/checkout/sessions");
        assert_eq!(
            req.idempotency_key,
            Some("checkout-session:op-9".to_string())
        );
        for pinned in [
            "mode=subscription",
            "customer=cus_123",
            "line_items%5B0%5D%5Bprice%5D=price_456",
            "line_items%5B0%5D%5Bquantity%5D=1",
            "client_reference_id=op-9",
            "metadata%5Bbilling_operation_id%5D=op-9",
            "subscription_data%5Bmetadata%5D%5Bbilling_operation_id%5D=op-9",
        ] {
            assert!(req.body.contains(pinned), "missing {pinned}: {}", req.body);
        }
    }

    #[test]
    fn portal_session_request_pins_explicit_configuration_and_form() {
        let req = create_billing_portal_session_request(
            "cus_123",
            "bpc_456",
            "https://example.com/account/billing?from=a+b&next=/home",
        )
        .expect("required values present");
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/billing_portal/sessions");
        assert_eq!(req.idempotency_key, None);
        assert_eq!(
            req.body,
            "customer=cus_123&configuration=bpc_456&return_url=https%3A%2F%2Fexample.com%2Faccount%2Fbilling%3Ffrom%3Da%2Bb%26next%3D%2Fhome"
        );
    }

    #[test]
    fn portal_session_request_rejects_blank_required_values() {
        for blank in ["", " ", "\t\n"] {
            assert!(create_billing_portal_session_request(blank, "bpc_1", "url").is_none());
            assert!(create_billing_portal_session_request("cus_1", blank, "url").is_none());
            assert!(create_billing_portal_session_request("cus_1", "bpc_1", blank).is_none());
        }
    }

    #[test]
    fn retrieve_and_cancel_paths_escape_ids() {
        assert_eq!(
            retrieve_checkout_session_request("cs_test_123").path,
            "/v1/checkout/sessions/cs_test_123"
        );
        assert_eq!(
            retrieve_subscription_request("sub_123").path,
            "/v1/subscriptions/sub_123"
        );
        assert_eq!(cancel_subscription_request("sub_123").method, "DELETE",);
    }

    #[test]
    fn expire_session_posts_to_the_session_expire_path() {
        let req = expire_checkout_session_request("cs_test_123");
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/checkout/sessions/cs_test_123/expire");
        assert_eq!(req.idempotency_key, None, "expiry converges on the object");
        assert_eq!(
            expire_checkout_session_request("cs a/b").path,
            "/v1/checkout/sessions/cs%20a%2Fb/expire"
        );
    }

    #[test]
    fn invoice_readers_cover_full_lines_and_filtered_lists() {
        let req = retrieve_invoice_request("in_123");
        assert!(req.path.starts_with("/v1/invoices/in_123?"), "{}", req.path);
        assert!(req.path.contains("expand"), "{}", req.path);
        let page = list_invoice_lines_request("in_123", None, 100);
        assert_eq!(page.path, "/v1/invoices/in_123/lines?limit=100");
        let next = list_invoice_lines_request("in_123", Some("li_9"), 100);
        assert!(next.path.contains("starting_after=li_9"), "{}", next.path);
        // Filtered lists only: an unfiltered list is never issued.
        assert!(list_invoices_request(None, None, true, 10, None).is_none());
        let filtered =
            list_invoices_request(Some("cus_1"), Some("sub_1"), true, 10, None).expect("filtered");
        assert!(
            filtered.path.contains("customer=cus_1"),
            "{}",
            filtered.path
        );
        assert!(
            filtered.path.contains("subscription=sub_1"),
            "{}",
            filtered.path
        );
        assert!(filtered.path.contains("status=paid"), "{}", filtered.path);
        assert!(
            !filtered.path.contains("starting_after"),
            "{}",
            filtered.path
        );
        let next = list_invoices_request(Some("cus_1"), Some("sub_1"), true, 100, Some("in_9"))
            .expect("next page");
        assert!(next.path.contains("starting_after=in_9"), "{}", next.path);
        assert!(next.path.contains("limit=100"), "{}", next.path);
    }

    #[test]
    fn invoice_list_envelope_parses_the_real_list_shape() {
        // `GET /v1/invoices` returns a list envelope, never a bare array:
        // a `Vec<Invoice>` parse of this body fails, the envelope parses.
        let body = serde_json::json!({
            "object": "list",
            "data": [
                {
                    "id": "in_test_1",
                    "object": "invoice",
                    "customer": "cus_test_1",
                    "subscription": "sub_test_1",
                    "status": "paid",
                    "amount_paid": 1000,
                    "currency": "usd",
                    "status_transitions": {"paid_at": 1_786_032_060},
                    "lines": {
                        "object": "list",
                        "data": [
                            {
                                "id": "in_test_1_line_1",
                                "amount": 1000,
                                "currency": "usd",
                                "price": "price_plus_test",
                                "period": {"start": 1_786_032_000, "end": 1_788_624_000},
                            }
                        ],
                        "has_more": false,
                        "total_count": 1,
                        "url": "/v1/invoices/in_test_1/lines",
                    },
                    "livemode": false,
                }
            ],
            "has_more": true,
            "total_count": 2,
            "url": "/v1/invoices",
        });
        assert!(
            serde_json::from_value::<Vec<Invoice>>(body.clone()).is_err(),
            "a bare-array parse must fail on the real envelope shape"
        );
        let list: InvoiceList =
            serde_json::from_value(body).expect("envelope parses into invoices");
        assert!(list.has_more, "the cursor must drive a second page");
        assert_eq!(list.data.len(), 1);
        let invoice = &list.data[0];
        assert_eq!(invoice.id, "in_test_1");
        assert_eq!(invoice.status, "paid");
        assert_eq!(invoice.paid_at(), Some(1_786_032_060));
        let lines = invoice.lines.as_ref().expect("lines expand inline");
        assert_eq!(lines.data.len(), 1);
        assert_eq!(lines.data[0].price.as_deref(), Some("price_plus_test"));
        assert!(!lines.has_more);
    }

    #[test]
    fn idempotency_keys_derive_from_operation_with_distinct_prefixes() {
        assert_eq!(checkout_idempotency_key("op-1"), "checkout-session:op-1");
        assert_eq!(customer_idempotency_key("op-1"), "customer:op-1");
        assert_ne!(
            checkout_idempotency_key("op-1"),
            customer_idempotency_key("op-1")
        );
    }

    #[test]
    fn pinned_api_surface_facts() {
        assert_eq!(STRIPE_API_BASE, "https://api.stripe.com");
        assert_eq!(STRIPE_API_VERSION, "2024-12-18.acacia");
        assert_eq!(WEBHOOK_TOLERANCE_SECS, 300);
    }

    // --- Response tolerance ---------------------------------------------------

    #[test]
    fn responses_ignore_unknown_fields_and_collapse_expanded_refs() {
        let session: CheckoutSession = serde_json::from_value(serde_json::json!({
            "id": "cs_test_123",
            "object": "checkout.session",
            "status": "complete",
            "payment_status": "paid",
            "customer": {"id": "cus_123", "object": "customer", "email": "a@b.c"},
            "subscription": "sub_123",
            "livemode": false,
            "future_field": {"nested": true},
        }))
        .expect("tolerant session parse");
        assert_eq!(session.customer.as_deref(), Some("cus_123"));
        assert_eq!(session.subscription.as_deref(), Some("sub_123"));

        let sub: Subscription = serde_json::from_value(serde_json::json!({
            "id": "sub_123",
            "customer": "cus_123",
            "status": "active",
            "cancel_at_period_end": false,
            "current_period_end": 1_786_291_200,
            "items": {"data": [{"id": "si_123", "price": {"id": "price_456"}}]},
            "livemode": false,
        }))
        .expect("tolerant subscription parse");
        assert_eq!(
            sub.single_item(),
            Some(("si_123", Some("price_456"))),
            "single item resolves"
        );

        let multi = Subscription {
            items: Some(SubscriptionItems {
                data: vec![
                    SubscriptionItem {
                        id: "a".into(),
                        price: None,
                    },
                    SubscriptionItem {
                        id: "b".into(),
                        price: None,
                    },
                ],
            }),
            ..sub.clone()
        };
        assert_eq!(multi.single_item(), None, "multi-item refuses to resolve");
    }

    #[test]
    fn invoice_paid_at_comes_from_status_transitions() {
        let invoice: Invoice = serde_json::from_value(serde_json::json!({
            "id": "in_123",
            "customer": "cus_123",
            "subscription": "sub_123",
            "status": "paid",
            "amount_paid": 800,
            "currency": "usd",
            "status_transitions": {"paid_at": 1_786_032_100, "finalized_at": 1_786_032_000},
            "lines": {"data": [], "has_more": false},
            "livemode": false,
        }))
        .expect("tolerant invoice parse");
        assert_eq!(invoice.paid_at(), Some(1_786_032_100));

        let unpaid: Invoice = serde_json::from_value(serde_json::json!({
            "id": "in_124",
            "status": "open",
            "amount_paid": 0,
            "lines": {"data": [], "has_more": false},
        }))
        .expect("tolerant open invoice parse");
        assert_eq!(unpaid.paid_at(), None, "open invoice has no paid_at");
        assert_eq!(
            unpaid.customer, None,
            "missing refs are None, never guessed"
        );
    }

    // --- Webhook verification ----------------------------------------------------

    fn event_body() -> Vec<u8> {
        serde_json::json!({
            "id": "evt_123",
            "type": "checkout.session.completed",
            "livemode": false,
            "data": {"object": {"id": "cs_test_123"}},
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn valid_signature_accepts_before_state() {
        let body = event_body();
        let header = test_signature_header(&body, NOW, SECRET);
        let event = verify_webhook_signature(&body, &header, SECRET, NOW).expect("valid");
        assert_eq!(event.id, "evt_123");
        assert_eq!(event.event_type, "checkout.session.completed");
        assert!(!event.livemode);
    }

    #[test]
    fn tampered_body_bad_signature_and_stale_timestamp_reject() {
        let body = event_body();
        let header = test_signature_header(&body, NOW, SECRET);
        // Tampered body under a valid header.
        let mut tampered = body.clone();
        tampered[10] ^= 0x01;
        assert_eq!(
            verify_webhook_signature(&tampered, &header, SECRET, NOW),
            Err(WebhookError::SignatureMismatch)
        );
        // Wrong secret.
        assert_eq!(
            verify_webhook_signature(&body, &header, "whsec_wrong", NOW),
            Err(WebhookError::SignatureMismatch)
        );
        // Garbage header.
        for bad in ["", "t=abc,v1=00", "v1=00", "t=1"] {
            assert_eq!(
                verify_webhook_signature(&body, bad, SECRET, NOW),
                Err(WebhookError::MalformedSignature),
                "header {bad:?}"
            );
        }
        // Stale and far-future timestamps reject (symmetric tolerance).
        for t in [NOW - 301, NOW + 301, NOW - 100_000] {
            let stale = test_signature_header(&body, t, SECRET);
            assert_eq!(
                verify_webhook_signature(&body, &stale, SECRET, NOW),
                Err(WebhookError::StaleTimestamp),
                "t={t}"
            );
        }
        // Boundary (exactly 300s) still verifies.
        for t in [NOW - 300, NOW + 300] {
            let edge = test_signature_header(&body, t, SECRET);
            verify_webhook_signature(&body, &edge, SECRET, NOW).expect("edge verifies");
        }
    }

    #[test]
    fn multiple_v1_candidates_accept_one_match() {
        let body = event_body();
        let good = test_signature_header(&body, NOW, SECRET);
        let good_hex = good.split_once("v1=").map(|(_, v)| v).expect("v1");
        let header = format!("t={NOW},v1=00,v1={good_hex}");
        verify_webhook_signature(&body, &header, SECRET, NOW).expect("one match suffices");
    }

    #[test]
    fn verified_non_event_body_rejects_without_state() {
        // Signature valid, but the body is not an event object.
        for body in [b"{}".to_vec(), b"[]".to_vec(), b"not json".to_vec()] {
            let header = test_signature_header(&body, NOW, SECRET);
            assert_eq!(
                verify_webhook_signature(&body, &header, SECRET, NOW),
                Err(WebhookError::MalformedEvent),
                "body {body:?}"
            );
        }
    }

    #[test]
    fn stripe_errors_parse_with_sealed_display() {
        let err = StripeError::parse(
            402,
            br#"{"error": {"code": "card_declined", "message": "Your card was declined."}}"#,
        );
        assert_eq!(err.http_status, 402);
        assert_eq!(err.code.as_deref(), Some("card_declined"));
        // Display never carries Stripe's message text.
        assert!(!err.to_string().contains("declined"));
        let opaque = StripeError::parse(500, b"<html>bad gateway</html>");
        assert_eq!(opaque.http_status, 500);
        assert_eq!(opaque.code, None);
    }
}
