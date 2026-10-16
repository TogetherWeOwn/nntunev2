//! Workers runtime entry (wasm32 only): the per-user `NntpPool` Durable Object.
//!
//! The pool's provider configuration is sourced from the account vault in a
//! separate integration slice. Until that wiring is present, pool requests
//! fail closed with a retryable `503`; no public route can dial an arbitrary
//! provider. The Worker's fetch handler is a not-found response until the play
//! route is connected to the authorization, R2, and metering adapters.
//!
//! Route/status decisions live in [`crate::routes`] (portable, natively
//! tested); this module only adapts them to `worker` responses.

use worker::{
    durable_object, event, Context, DurableObject, Env, Request, Response, Result, State,
};

use crate::routes::{self, ProbeDecision};

/// The Worker has no public HTTP routes until the play adapters are wired.
#[event(fetch)]
async fn fetch(_req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    let (status, body) = routes::decide_public_fetch();
    Response::error(body, status)
}

/// Per-user Durable Object entrypoint for NNTP pool operations.
#[durable_object(fetch)]
pub struct NntpPool {
    state: State,
}

impl DurableObject for NntpPool {
    fn new(state: State, _env: Env) -> Self {
        Self { state }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let method = req.method();
        // Durable Objects must be addressed with id_from_name(user_id). Refuse
        // unnamed instances rather than serving an ownerless pool.
        let has_owner_name = self.state.id().name().is_some();
        let decision = routes::decide_probe(method.as_ref(), &req.path(), has_owner_name);
        let response = Response::error(decision.body(), decision.status())?;
        match decision {
            ProbeDecision::ProviderUnavailable => {
                let headers = worker::Headers::new();
                headers.set("content-type", "application/json")?;
                headers.set("retry-after", "5")?;
                Ok(response.with_headers(headers))
            }
            ProbeDecision::NotFound | ProbeDecision::OwnerMissing => Ok(response),
        }
    }
}
