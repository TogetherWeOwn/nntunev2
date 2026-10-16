//! Workers runtime entry (wasm32 only): the per-user `NntpPool` Durable Object.
//!
//! The pool's provider configuration is sourced from the account vault in a
//! separate integration slice. Until that wiring is present, pool requests
//! fail closed with a retryable `503`; no public route can dial an arbitrary
//! provider. The Worker's fetch handler is a not-found response until the play
//! route is connected to the authorization, R2, and metering adapters.

use worker::{
    durable_object, event, Context, DurableObject, Env, Method, Request, Response, Result, State,
};

/// The Worker has no public HTTP routes until the play adapters are wired.
#[event(fetch)]
async fn fetch(_req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    Response::error("not found", 404)
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
        if req.method() != Method::Post || req.path() != "/probe" {
            return Response::error("not found", 404);
        }

        // Durable Objects must be addressed with id_from_name(user_id). Refuse
        // unnamed instances rather than serving an ownerless pool.
        if self.state.id().name().is_none() {
            return Response::error("pool owner unavailable", 400);
        }

        let headers = worker::Headers::new();
        headers.set("content-type", "application/json")?;
        headers.set("retry-after", "5")?;
        Ok(Response::ok(r#"{"error":"provider_unavailable"}"#)?
            .with_status(503)
            .with_headers(headers))
    }
}
