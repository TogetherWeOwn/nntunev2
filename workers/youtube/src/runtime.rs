//! Workers runtime entry (wasm32 only): the `YtExtract` Durable Object and
//! its extraction route.
//!
//! - `POST /extract` on the Durable Object runs one `/player` extraction
//!   ([`crate::client`], [`crate::player`]) once the extraction wiring
//!   lands; until then it answers retryable `503` ([`crate::routes`]).
//!   Callers reach the object through the `YT_EXTRACT` binding (see
//!   `workers/youtube/wrangler.toml.example`); there is no public route to
//!   it.
//! - The Worker's own `fetch` answers `404` until the measurement routes
//!   land.
//!
//! The object owns the extraction rate limits and circuit breakers, so a
//! warm instance sheds load before the next `/player` call instead of after
//! it.

use worker::{
    durable_object, event, Context, DurableObject, Env, Headers, Request, Response, Result,
};

use crate::routes::{self, ExtractDecision};

/// The Worker's own route: only the Durable Object serves extractions, so
/// every request here is a 404 until the measurement routes land.
#[event(fetch)]
async fn fetch(_req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    let (status, body) = routes::decide_public_fetch();
    Response::error(body, status)
}

#[durable_object(fetch)]
pub struct YtExtract;

impl DurableObject for YtExtract {
    /// Builds the object without extraction state; rate-limit counters and
    /// the circuit breaker arrive with the extraction wiring, which also
    /// starts reading the KV bindings from `env` here.
    fn new(_state: worker::State, _env: Env) -> Self {
        Self
    }

    /// Serves `POST /extract`; any other method or path is a 404.
    async fn fetch(&self, req: Request) -> Result<Response> {
        let decision = routes::decide_extract(req.method().as_ref(), &req.path());
        match decision {
            ExtractDecision::NotFound => Response::error(
                ExtractDecision::NotFound.body(),
                ExtractDecision::NotFound.status(),
            ),
            ExtractDecision::ExtractionUnavailable => respond_unavailable(),
        }
    }
}

/// Renders the retryable `503` extraction-unavailable JSON body.
fn respond_unavailable() -> Result<Response> {
    let decision = ExtractDecision::ExtractionUnavailable;
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    Ok(Response::ok(decision.body())?
        .with_status(decision.status())
        .with_headers(headers))
}
