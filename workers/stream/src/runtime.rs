//! Workers runtime entry (wasm32 only): the per-user `NntpPool` Durable
//! Object and its probe route.
//!
//! - `POST /probe` on the Durable Object runs one ingest probe batch
//!   ([`crate::probe`]). There is no public route to it: callers reach the
//!   object through a Durable Object binding (`NNTP_POOL`, cross-script from
//!   the ingest Worker), addressed by `id_from_name(user_id)`, and the object
//!   refuses envelopes naming any other user.
//! - The Worker's own `fetch` answers `404` until the play routes land.
//!
//! I/O objects belong to the Durable Object rather than to one request, so a
//! warm provider connection parked in the pool is reused by later requests.

use std::cell::{OnceCell, RefCell};

use futures_util::StreamExt;
use nntune_core::nntp::NntpClient;
use nntune_core::transports::WorkerTransport;
use worker::{
    durable_object, event, Context, Date, DurableObject, Env, Headers, Request, Response, Result,
    State,
};

use crate::pool::Pool;
use crate::probe::{
    parse_request, run_probe, PoolProvider, ProbeError, ProbeReply, WallClock, MAX_PROBE_BODY_BYTES,
};
use crate::routes::{self, ProbeDecision};

/// Durable Object route for one probe batch.
pub const PROBE_PATH: &str = routes::PROBE_PATH;

/// The Worker's own route: only the Durable Object serves probes, so every
/// request here is a 404 until the play routes land.
#[event(fetch)]
async fn fetch(_req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    let (status, body) = routes::decide_public_fetch();
    Response::error(body, status)
}

#[durable_object(fetch)]
pub struct NntpPool {
    state: State,
    /// Built by the first probe that passes validation and loads provider
    /// settings, with the provider's cap; a changed cap applies when the
    /// runtime next evicts this instance.
    pool: OnceCell<RefCell<Pool<NntpClient<WorkerTransport>>>>,
    clock: WallClock,
}

impl DurableObject for NntpPool {
    /// Builds the object without a pool; `pool` stays unset until a probe
    /// passes validation and loads provider settings.
    fn new(state: State, _env: Env) -> Self {
        Self {
            state,
            pool: OnceCell::new(),
            clock: WallClock::default(),
        }
    }

    /// Serves `POST /probe`; any other method or path is a 404.
    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let has_owner_name = self.state.id().name().is_some();
        routes::dispatch_probe(
            routes::decide_probe_action(req.method().as_ref(), &req.path(), has_owner_name),
            |decision| match decision {
                ProbeDecision::NotFound => Response::error(decision.body(), decision.status()),
                _ => respond(route_reply(decision)),
            },
            || async { respond(self.probe(&mut req).await) },
        )
        .await
    }
}

impl NntpPool {
    /// Reads, validates and runs one probe envelope against this object's pool.
    async fn probe(&self, req: &mut Request) -> ProbeReply {
        // The id came from `id_from_name(user_id)`; without the name there is
        // no owner to check the envelope against, so refuse.
        let Some(pool_user) = self.state.id().name() else {
            return route_reply(ProbeDecision::OwnerMissing);
        };
        let body = match read_capped(req, MAX_PROBE_BODY_BYTES).await {
            Ok(body) => body,
            Err(e) => return ProbeReply::from(Err(e)),
        };
        let probe = match parse_request(&body, &pool_user) {
            Ok(probe) => probe,
            Err(e) => return ProbeReply::from(Err(e)),
        };
        let provider = match self.provider(&pool_user).await {
            Ok(provider) => provider,
            Err(ProbeError::ProviderUnavailable) => {
                return route_reply(routes::decide_probe(
                    routes::PROBE_METHOD,
                    routes::PROBE_PATH,
                    true,
                ));
            }
            Err(e) => return ProbeReply::from(Err(e)),
        };
        let pool = self
            .pool
            .get_or_init(|| RefCell::new(Pool::new(provider.capacity)));
        let config = provider.config;
        let dial = || {
            let config = config.clone();
            async move {
                let transport = WorkerTransport::connect(&config)
                    .map_err(|e| nntune_core::Error::Io(std::io::Error::other(e.to_string())))?;
                NntpClient::connect(transport, &config).await
            }
        };
        let clock = &self.clock;
        let tick = |pool: &mut Pool<NntpClient<WorkerTransport>>| {
            clock.sync(pool, Date::now().as_millis());
        };
        ProbeReply::from(run_probe(pool, dial, tick, &probe).await)
    }

    /// The user's provider settings and connection cap.
    ///
    /// Not connected yet: the encrypted provider row lives in the account
    /// vault, and reading it here (Hyperdrive, key-encryption key, and the
    /// SSRF guard on every connect) is the vault-read slice. Until then every
    /// probe answers `503 provider_unavailable`, which ingest retries.
    ///
    /// When it returns real settings, key the pool (or flush its idle
    /// connections) on a provider fingerprint: the pool is built once with the
    /// first provider's cap and parked connections carry no provider identity,
    /// so a changed host or credential would keep answering from the old
    /// connection until the instance is evicted.
    async fn provider(&self, _user_id: &str) -> core::result::Result<PoolProvider, ProbeError> {
        Err(ProbeError::ProviderUnavailable)
    }
}

/// Convert the portable routing decision into the reply rendered by the adapter.
fn route_reply(decision: ProbeDecision) -> ProbeReply {
    ProbeReply {
        status: decision.status(),
        body: decision.body().to_string(),
        retry_after_secs: None,
    }
}

/// Read the body without ever buffering more than `cap` bytes.
async fn read_capped(req: &mut Request, cap: usize) -> core::result::Result<Vec<u8>, ProbeError> {
    let unreadable = ProbeError::BadRequest("unreadable body");
    let mut stream = Box::pin(req.stream().map_err(|_| unreadable)?);
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| unreadable)?;
        if buf.len().saturating_add(chunk.len()) > cap {
            return Err(ProbeError::TooLarge);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Turns a [`ProbeReply`] into the JSON `Response`, adding `Retry-After` when
/// the reply carries one.
fn respond(reply: ProbeReply) -> Result<Response> {
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    if let Some(secs) = reply.retry_after_secs {
        headers.set("retry-after", &secs.to_string())?;
    }
    Ok(Response::ok(reply.body)?
        .with_status(reply.status)
        .with_headers(headers))
}
