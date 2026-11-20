//! Hosted disposable Worker control for caller recovery after a lost COMMIT reply.
use super::{handlers, DohDns, MirrorBinding, PgStore, SocketDialer, StoreError, WebhookMailer};
use crate::stripe::{self, StripeError, StripeRequest, StripeResult};
use serde_json::{json, Value};
use std::cell::Cell;
use worker::{Env, Request, Response};

const NOW: u64 = 1_786_032_000;
const OPERATION: &str = "a0000000-0000-4000-8000-000000000001";
const CUSTOMER: &str = "cus_worker_fault";
pub(super) const SUBSCRIPTION: &str = "sub_worker_fault";
const PRICE: &str = "price_worker_fault";
const SESSION: &str = "cs_worker_fault";

pub(super) async fn connect() -> Result<tokio_postgres::Client, StoreError> {
    // Ancillary reads, claims and checkout marks never cross the COMMIT relay.
    let socket = worker::Socket::builder()
        .connect("127.0.0.1", 5432)
        .map_err(|_| StoreError)?;
    socket.opened().await.map_err(|_| StoreError)?;
    let config: tokio_postgres::Config = super::billing_deadline_test::CONNECTION
        .parse()
        .map_err(|_| StoreError)?;
    let (client, driver) = config
        .connect_raw(socket, tokio_postgres::NoTls)
        .await
        .map_err(|_| StoreError)?;
    wasm_bindgen_futures::spawn_local(async move {
        let _ = driver.await;
    });
    client
        .batch_execute(&format!(
            "SET statement_timeout = {}",
            super::TOTAL_TIMEOUT_MS
        ))
        .await
        .map_err(|_| StoreError)?;
    Ok(client)
}

pub(super) struct StripeScript {
    expected: [(StripeRequest, Value); 3],
    next: Cell<usize>,
}

impl StripeScript {
    fn new() -> Self {
        let invoice = |id: &str, currency: &str| {
            json!({
                "id": id, "customer": CUSTOMER, "subscription": SUBSCRIPTION,
                "status": "paid", "currency": currency, "amount_paid": 800,
                "status_transitions": {"paid_at": NOW},
                "lines": {"data": [{"id": format!("line_{id}"), "amount": 800,
                    "price": PRICE, "period": {"start": NOW, "end": NOW + 100}}]}
            })
        };
        Self {
            expected: [
                (
                    stripe::retrieve_checkout_session_request(SESSION),
                    json!({"id": SESSION, "customer": CUSTOMER, "subscription": SUBSCRIPTION,
                           "client_reference_id": OPERATION, "status": "complete", "payment_status": "paid"}),
                ),
                (
                    stripe::retrieve_subscription_request(SUBSCRIPTION),
                    json!({"id": SUBSCRIPTION, "customer": CUSTOMER, "status": "active",
                           "current_period_end": NOW + 100,
                           "items": {"data": [{"id": "si_worker_fault", "price": PRICE}]}}),
                ),
                (
                    stripe::list_invoices_request(
                        Some(CUSTOMER),
                        Some(SUBSCRIPTION),
                        true,
                        100,
                        None,
                    )
                    .expect("bound invoice listing"),
                    json!({"data": [invoice("in_worker_first", "usd"), invoice("in_worker_second", "eur")]}),
                ),
            ],
            next: Cell::new(0),
        }
    }

    pub(super) fn send<T: serde::de::DeserializeOwned>(
        &self,
        req: &StripeRequest,
    ) -> StripeResult<T> {
        let index = self.next.replace(usize::MAX);
        let (expected, response) = self
            .expected
            .get(index)
            .ok_or_else(StripeError::transport)?;
        if req != expected {
            return Err(StripeError::transport());
        }
        let decoded =
            serde_json::from_value(response.clone()).map_err(|_| StripeError::transport())?;
        self.next.set(index + 1);
        Ok(decoded)
    }
}

pub(crate) async fn deliver(
    mut req: Request,
    env: Env,
    lose_ack: bool,
) -> worker::Result<Response> {
    let Ok(cfg) = handlers::Config::new(
        "billing.example.test",
        "https://billing.example.test",
        vec![0; 32],
        [0; 32],
        1,
        60,
        false,
        false,
        None,
    ) else {
        return Response::error("webhook control config failed", 500);
    };
    let cfg = cfg.with_stripe(Some("synthetic".into()), Some("webhook-ack-fixture".into()));
    let expired = std::rc::Rc::new(Cell::new(false));
    let trace = if lose_ack {
        "worker-webhook-ack-test"
    } else {
        "worker-webhook-retry-test"
    };
    let store = PgStore {
        env: None,
        trace_id: trace.into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: Some(expired.clone()),
        cancel_token_observer: None,
    };
    let stripe = super::WorkerStripeTransport {
        secret_key: None,
        webhook_script: Some(StripeScript::new()),
    };
    let mailer = WebhookMailer {
        env,
        origin: cfg.origin.clone(),
        trace_id: trace.into(),
    };
    let ctx = handlers::Ctx {
        cfg: &cfg,
        store: &store,
        mailer: &mailer,
        dialer: &SocketDialer,
        dns: &DohDns,
        stripe: &stripe,
        now: NOW,
        source_ip: "127.0.0.1".into(),
    };
    let delivery = handlers::Request {
        method: "POST".into(),
        path: handlers::BILLING_WEBHOOK_PATH.into(),
        cookie: None,
        origin: None,
        authorization: None,
        stripe_signature: req.headers().get("Stripe-Signature")?,
        body: req.bytes().await?,
    };
    let response = handlers::handle(&ctx, &delivery).await;
    let script = stripe
        .webhook_script
        .as_ref()
        .expect("installed fixture script");
    if script.next.get() != script.expected.len() || expired.get() {
        return Response::error(
            "webhook control missed provider reads or reached the work deadline",
            500,
        );
    }
    super::render(&response, &[], trace)
}
