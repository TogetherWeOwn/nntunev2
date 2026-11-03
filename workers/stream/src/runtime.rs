//! Workers runtime entry (wasm32 only): the public play fetch and the
//! per-user `NntpPool` Durable Object with its internal routes.
//!
//! - The Worker's public `fetch` serves only `GET /play/{track_id}`
//!   ([`crate::play_route`]): the signed `v2` grant is verified before any
//!   source is touched (no grant → no bytes), then the requested range is
//!   served from the R2 popularity cache (`AUDIO_CACHE`) and the
//!   client-written byte interval of the completed response is reported
//!   to the user's pool Durable Object ([`crate::delivered`]). A cache
//!   miss answers `502 upstream_unavailable`: the wasm NNTP play adapter
//!   lands with the operator provider wiring in the follow-up card (the
//!   portable adapter over [`crate::play`] is fixture-proven natively);
//!   everything else is a `404`.
//! - `POST /probe` on the Durable Object runs one ingest probe batch
//!   ([`crate::probe`]); `POST /body` fetches one article body
//!   ([`crate::body`]); `POST /delivered` validates and acknowledges one
//!   delivered-byte report ([`crate::delivered`]). There is no public
//!   route to any of them: callers reach the object through a Durable
//!   Object binding (`NNTP_POOL`, cross-script from the ingest Worker for
//!   probe/body; this Worker's own binding for delivered), addressed by
//!   `id_from_name(user_id)`, and the object refuses envelopes naming any
//!   other user.
//!
//! I/O objects belong to the Durable Object rather than to one request, so a
//! warm provider connection parked in the pool is reused by later requests.

use std::cell::{OnceCell, RefCell};
use std::pin::Pin;
use std::task::{Context as PollContext, Poll};
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use nntune_core::nntp::NntpClient;
use nntune_core::transports::WorkerTransport;
use worker::wasm_bindgen::JsValue;
use worker::{
    durable_object, event, ByteStream, Context, Date, Delay, DurableObject, Env, Headers, Method,
    Range, Request, RequestInit, Response, Result, State,
};

use crate::api_error::ApiError;
use crate::body::{parse_body_request, run_body, BodyReply, MAX_BODY_REQUEST_BYTES};
use crate::delivered::{serve_delivered, DeliveredInterval, MAX_DELIVERED_REQUEST_BYTES};
use crate::grant;
use crate::play_route::{self, PLAY_CONTENT_TYPE, TRACK_MANIFEST_SUFFIX};
use crate::pool::Pool;
use crate::probe::{
    parse_request, run_probe, PoolProvider, ProbeError, ProbeReply, WallClock, MAX_PROBE_BODY_BYTES,
};
use crate::range::{self, RangeOutcome};
use crate::routes::{self, PoolAction, ProbeDecision, PublicRoute};

/// Durable Object route for one probe batch.
pub const PROBE_PATH: &str = routes::PROBE_PATH;

/// Secret binding holding the play-grant HMAC key (`wrangler secret put
/// PLAY_GRANT_SECRET`). The `api` Worker signs grants with the same value
/// at issuance; this Worker only verifies.
const PLAY_GRANT_SECRET_BINDING: &str = "PLAY_GRANT_SECRET";
/// R2 popularity-cache binding ([`crate::cache`] key layout).
const AUDIO_CACHE_BINDING: &str = "AUDIO_CACHE";
/// This Worker's own `NntpPool` Durable Object binding.
const POOL_BINDING: &str = "NNTP_POOL";
/// Delivered-report URL on the pool object (path: [`routes::DELIVERED_PATH`]).
const POOL_DELIVERED_URL: &str = "https://nntp-pool/delivered";
/// A track pointer object is one sha256-hex hash; anything bigger is not a
/// pointer and is never buffered whole.
const MAX_POINTER_BYTES: u64 = 256;

/// The Worker's own route: the public play fetch; every other request is a
/// 404 (the Durable Object routes have no public path).
#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    match routes::decide_public_route(req.method().as_ref(), &req.path()) {
        PublicRoute::Play { track_id } => play_fetch(req, env, ctx, &track_id).await,
        PublicRoute::NotFound => {
            let (status, body) = routes::decide_public_fetch();
            Response::error(body, status)
        }
    }
}

/// Serve one `GET /play/{track_id}` from the R2 popularity cache:
/// verify the `v2` grant before touching any source, resolve the HTTP
/// range against the cached blob, stream exactly that slice, and report
/// the client-written interval when the response stream settles.
async fn play_fetch(req: Request, env: Env, ctx: Context, track_id: &str) -> Result<Response> {
    let unavailable = || api_error_response(&ApiError::upstream_unavailable(), None);
    let Ok(secret) = env.secret(PLAY_GRANT_SECRET_BINDING) else {
        return api_error_response(&ApiError::internal_error(), None);
    };
    let secret = secret.to_string();
    if secret.is_empty() {
        return api_error_response(&ApiError::internal_error(), None);
    }
    let authorization = req.headers().get("authorization").ok().flatten();
    let token = play_route::bearer_token(authorization.as_deref()).unwrap_or("");
    let now_unix = Date::now().as_millis() / 1000;
    let Ok(grant) = grant::verify(token, secret.as_bytes(), now_unix) else {
        return api_error_response(&ApiError::unauthorized(), None);
    };
    if grant.track_id != track_id {
        return api_error_response(&ApiError::forbidden(), None);
    }
    // A `v1` grant carries no play id to meter against; this route only
    // serves `v2` (see [`crate::play_route`]).
    let Some(play_id) = grant.play_id.clone() else {
        return api_error_response(&ApiError::unauthorized(), None);
    };

    let Ok(bucket) = env.bucket(AUDIO_CACHE_BINDING) else {
        return api_error_response(&ApiError::internal_error(), None);
    };
    let pointer = match bucket.get(format!("track/{track_id}")).execute().await {
        Ok(pointer) => pointer,
        Err(_) => return unavailable(),
    };
    let Some(pointer) = pointer else {
        // No cache pointer: a manifest means a known track the wasm NNTP
        // adapter does not serve yet; no manifest means unknown track.
        let manifest_key = format!("track/{track_id}{TRACK_MANIFEST_SUFFIX}");
        return match bucket.head(manifest_key).await {
            Ok(Some(_)) => unavailable(),
            Ok(None) => api_error_response(&ApiError::not_found(), None),
            Err(_) => unavailable(),
        };
    };
    if pointer.size() > MAX_POINTER_BYTES {
        return unavailable();
    }
    let Some(pointer_body) = pointer.body() else {
        return unavailable();
    };
    let Ok(pointer_bytes) = pointer_body.bytes().await else {
        return unavailable();
    };
    let Ok(hash) = std::str::from_utf8(&pointer_bytes) else {
        return unavailable();
    };
    let hash = hash.trim();
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return unavailable();
    }
    let blob_key = format!("audio/{hash}");
    let blob_len = match bucket.head(blob_key.clone()).await {
        Ok(Some(blob)) => blob.size(),
        _ => return unavailable(),
    };

    let range_header = req.headers().get("range").ok().flatten();
    let (start, expected, status, content_range) =
        match range::parse(range_header.as_deref(), blob_len) {
            RangeOutcome::Full => (0, blob_len, 200, None),
            RangeOutcome::Partial(slice) => (
                slice.start,
                slice.len(),
                206,
                Some(range::content_range_value(slice, blob_len)),
            ),
            RangeOutcome::Unsatisfiable => {
                return api_error_response(
                    &ApiError::range_not_satisfiable(),
                    Some(("content-range", &format!("bytes */{blob_len}"))),
                );
            }
        };
    let headers = Headers::new();
    headers.set("content-type", PLAY_CONTENT_TYPE)?;
    headers.set("accept-ranges", "bytes")?;
    headers.set("content-length", &expected.to_string())?;
    if let Some(content_range) = &content_range {
        headers.set("content-range", content_range)?;
    }
    if expected == 0 {
        return Ok(Response::empty()?.with_status(status).with_headers(headers));
    }
    let object = bucket
        .get(blob_key)
        .range(Range::OffsetWithLength {
            offset: start,
            length: expected,
        })
        .execute()
        .await;
    let Some(object) = (match object {
        Ok(object) => object,
        Err(_) => return unavailable(),
    }) else {
        return unavailable();
    };
    let Some(body) = object.body() else {
        return unavailable();
    };
    let Ok(stream) = body.stream() else {
        return unavailable();
    };
    let metered = MeteredStream {
        inner: Box::pin(stream),
        written: 0,
        expected,
        report: Some((
            env,
            ctx,
            DeliveredInterval {
                user_id: grant.user_id.clone(),
                play_id,
                track_id: track_id.to_string(),
                start,
                end: start,
            },
        )),
    };
    Response::builder()
        .with_headers(headers)
        .with_status(status)
        .from_stream(metered)
}

/// Response-stream wrapper that counts bytes actually pulled toward the
/// client and, when the stream settles (completed, errored, or dropped
/// on client abort), reports the written interval exactly once. The
/// report rides `wait_until`, so it survives the response finishing.
struct MeteredStream {
    inner: Pin<Box<ByteStream>>,
    written: u64,
    expected: u64,
    /// `(env, ctx, interval-template)`; the template's `end` is patched
    /// from `written` at settle time and the report fires at most once.
    report: Option<(Env, Context, DeliveredInterval)>,
}

impl Stream for MeteredStream {
    type Item = Result<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut PollContext<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                self.written = self.written.saturating_add(chunk.len() as u64);
                Poll::Ready(Some(Ok(chunk)))
            }
            other => other,
        }
    }
}

impl Drop for MeteredStream {
    fn drop(&mut self) {
        if self.written == 0 {
            return;
        }
        let Some((env, ctx, mut interval)) = self.report.take() else {
            return;
        };
        interval.end = interval.start + self.written.min(self.expected);
        if interval.end > interval.start {
            ctx.wait_until(report_delivered(env, interval));
        }
    }
}

/// POST one delivered interval to the user's pool object. Bounded
/// retries: a duplicate report is safe (the meter merges unions per
/// play), and a lost one only under-meters.
async fn report_delivered(env: Env, interval: DeliveredInterval) {
    let Ok(body) = serde_json::to_string(&interval) else {
        return;
    };
    for attempt in 0..3 {
        if attempt > 0 {
            let backoff = if attempt == 1 { 250 } else { 1000 };
            Delay::from(Duration::from_millis(backoff)).await;
        }
        if post_delivered(&env, &interval.user_id, &body).await.is_ok() {
            return;
        }
    }
}

async fn post_delivered(env: &Env, user_id: &str, body: &str) -> core::result::Result<(), ()> {
    let stub = env
        .durable_object(POOL_BINDING)
        .and_then(|ns| ns.id_from_name(user_id)?.get_stub())
        .map_err(|_| ())?;
    let headers = Headers::new();
    headers
        .set("content-type", "application/json")
        .map_err(|_| ())?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(body)));
    let request = Request::new_with_init(POOL_DELIVERED_URL, &init).map_err(|_| ())?;
    let response = stub.fetch_with_request(request).await.map_err(|_| ())?;
    if response.status_code() == 202 {
        Ok(())
    } else {
        Err(())
    }
}

/// Render a shared-envelope error (`docs/api-errors.md`) as the play
/// route's refusal; `extra` carries at most one route header (the `416`
/// `Content-Range`).
fn api_error_response(error: &ApiError, extra: Option<(&str, &str)>) -> Result<Response> {
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    if let Some((name, value)) = extra {
        headers.set(name, value)?;
    }
    Ok(Response::ok(error.body_json())?
        .with_status(error.status)
        .with_headers(headers))
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

    /// Serves `POST /probe`, `POST /body` and `POST /delivered`; any other method or path is a 404.
    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let has_owner_name = self.state.id().name().is_some();
        match routes::decide_pool_action(req.method().as_ref(), &req.path(), has_owner_name) {
            PoolAction::Respond(ProbeDecision::NotFound) => Response::error(
                ProbeDecision::NotFound.body(),
                ProbeDecision::NotFound.status(),
            ),
            PoolAction::Respond(decision) => respond(route_reply(decision)),
            PoolAction::RunProbe => respond(self.probe(&mut req).await),
            PoolAction::RunBody => respond_body(self.body(&mut req).await),
            PoolAction::RunDelivered => respond(self.delivered(&mut req).await),
        }
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
                let transport = WorkerTransport::connect_pinned(&config)
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

    /// Reads, validates and runs one body envelope against this object's pool.
    async fn body(&self, req: &mut Request) -> BodyReply {
        let Some(pool_user) = self.state.id().name() else {
            return BodyReply::from(Err(ProbeError::BadRequest("pool has no owner")));
        };
        let body = match read_capped(req, MAX_BODY_REQUEST_BYTES).await {
            Ok(body) => body,
            Err(e) => return BodyReply::from(Err(e)),
        };
        let fetch = match parse_body_request(&body, &pool_user) {
            Ok(fetch) => fetch,
            Err(e) => return BodyReply::from(Err(e)),
        };
        let provider = match self.provider(&pool_user).await {
            Ok(provider) => provider,
            Err(e) => return BodyReply::from(Err(e)),
        };
        let pool = self
            .pool
            .get_or_init(|| RefCell::new(Pool::new(provider.capacity)));
        let config = provider.config;
        let dial = || {
            let config = config.clone();
            async move {
                let transport = WorkerTransport::connect_pinned(&config)
                    .map_err(|e| nntune_core::Error::Io(std::io::Error::other(e.to_string())))?;
                NntpClient::connect(transport, &config).await
            }
        };
        let clock = &self.clock;
        let tick = |pool: &mut Pool<NntpClient<WorkerTransport>>| {
            clock.sync(pool, Date::now().as_millis());
        };
        BodyReply::from(run_body(pool, dial, tick, &fetch).await)
    }

    /// Validates and acknowledges one delivered-byte report for this
    /// object's owner ([`crate::delivered`]); accounting lands with
    /// the enforcement and meter card behind this envelope.
    async fn delivered(&self, req: &mut Request) -> ProbeReply {
        let Some(pool_user) = self.state.id().name() else {
            return route_reply(ProbeDecision::OwnerMissing);
        };
        let body = match read_capped(req, MAX_DELIVERED_REQUEST_BYTES).await {
            Ok(body) => body,
            Err(e) => return ProbeReply::from(Err(e)),
        };
        serve_delivered(&pool_user, &body).into()
    }

    /// The user's provider settings and connection cap.
    ///
    /// Not connected yet: the encrypted provider row lives in the account
    /// vault, and reading it here (Hyperdrive, key-encryption key, and the
    /// SSRF guard on every connect) is the vault-read slice. Until then every
    /// probe answers `503 provider_unavailable`, which ingest retries.
    ///
    /// (The play route above deliberately keeps no Hyperdrive/DB binding:
    /// provider config for playback arrives another way in its follow-up
    /// card, never via a database read in this Worker.)
    ///
    /// When it returns real settings, key the pool (or flush its idle
    /// connections) on a provider fingerprint: the pool is built once with the
    /// first provider's cap and parked connections carry no provider identity,
    /// so a changed host or credential would keep answering from the old
    /// connection until the instance is evicted.
    ///
    /// Copy the row's `allow_insecure_plaintext` acknowledgment onto the
    /// returned config: the pool dials through the pinned gate, which
    /// rejects unacknowledged TLS-off configs, so dropping the flag would
    /// lock out providers the vault accepted with the cleartext
    /// acknowledgment.
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

/// Turns a [`BodyReply`] into the `Response`: raw article bytes on `200`,
/// the JSON error object otherwise, adding `Retry-After` when carried.
fn respond_body(reply: BodyReply) -> Result<Response> {
    let headers = Headers::new();
    headers.set("content-type", reply.content_type)?;
    if let Some(secs) = reply.retry_after_secs {
        headers.set("retry-after", &secs.to_string())?;
    }
    Ok(Response::from_bytes(reply.body)?
        .with_status(reply.status)
        .with_headers(headers))
}
