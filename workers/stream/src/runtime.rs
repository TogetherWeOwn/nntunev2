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
//!   everything else is a `404`. The same follow-up owns grant-rejection
//!   meter signals on this path: the wasm fetch has no `Meter` sink yet,
//!   so an authenticated rejection here emits nothing, where the
//!   portable core emits exactly one signal (see [`crate::play_route`]).
//! - `POST /probe` on the Durable Object runs one ingest probe batch
//!   ([`crate::probe`]); `POST /body` fetches one article body
//!   ([`crate::body`]); `POST /delivered` accounts one delivered-byte
//!   report into the per-play meter book ([`crate::delivered`],
//!   [`crate::meter_sink`]) persisted in DO storage, and the DO alarm
//!   flushes cumulative windows to the api Worker's signed internal
//!   endpoint over the `API` service binding until acked. There is no public
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
use crate::delivered::{parse_delivered_report, DeliveredInterval, MAX_DELIVERED_REQUEST_BYTES};
use crate::grant;
use crate::meter_sink::{self, MeterBook};
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
/// Service binding to the api Worker (meter sink flush target).
const API_SERVICE_BINDING: &str = "API";
/// Flush URL on the api Worker (path: [`meter_sink::SINK_PATH`]); the
/// host is never resolved — the service binding routes the fetch.
const API_SINK_URL: &str = "https://nntune-api/internal/v1/play-windows";
/// First flush delay after a delivered report lands: long enough to
/// batch a burst of range responses from one play, short enough that
/// monthly usage trails playback by seconds, not minutes.
const METER_FLUSH_DELAY_MS: i64 = 20_000;
/// Re-arm delay after a failed or unconfigured flush; the windows
/// stay pending in DO storage across alarms and evictions.
const METER_RETRY_DELAY_MS: i64 = 60_000;
/// Re-arm delay when a batch cap left more windows pending.
const METER_CONTINUE_DELAY_MS: i64 = 1_000;
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
    // Same order as the portable route: the `v2` requirement before the
    // track binding, so a `v1` grant — even one minted for another
    // track — is a `401`, never a `403`. A `v1` grant carries no play
    // id to meter against; this route only serves `v2` (see
    // [`crate::play_route`]).
    let Some(play_id) = grant.play_id.clone() else {
        return api_error_response(&ApiError::unauthorized(), None);
    };
    if grant.track_id != track_id {
        return api_error_response(&ApiError::forbidden(), None);
    }

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
    // Pointer shape is portable-core contract (`cache.rs` owns the key
    // layout and unit-tests it); this adapter only fetches the bytes.
    let Some(blob_key) = crate::cache::blob_key_for_pointer(&pointer_bytes) else {
        return unavailable();
    };
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
                file_bytes: Some(blob_len),
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
pub(crate) async fn report_delivered(env: Env, interval: DeliveredInterval) {
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

#[durable_object(alarm)]
pub struct NntpPool {
    state: State,
    /// The Worker's bindings, kept for the alarm: the meter flush needs
    /// the `API` service binding and the `METER_SINK_SECRET` secret.
    env: Env,
    /// Built by the first probe that passes validation and loads provider
    /// settings, with the provider's cap; a changed cap applies when the
    /// runtime next evicts this instance.
    pool: OnceCell<RefCell<Pool<NntpClient<WorkerTransport>>>>,
    clock: WallClock,
}

impl DurableObject for NntpPool {
    /// Builds the object without a pool; `pool` stays unset until a probe
    /// passes validation and loads provider settings.
    fn new(state: State, env: Env) -> Self {
        Self {
            state,
            env,
            pool: OnceCell::new(),
            clock: WallClock::default(),
        }
    }

    /// Serves `POST /probe`, `POST /body` and `POST /delivered`; any other method or path is a 404.
    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let path = match crate::account_request::worker::path(&req) {
            Ok(path) => path,
            Err(_) => return Response::error("not found", 404),
        };
        if matches!(
            path.as_str(),
            nntune_core::pool_operation::INGEST_BODY_PATH
                | nntune_core::pool_operation::INGEST_STAT_PATH
        ) {
            return crate::account_runtime::ingest_fetch(
                &self.env,
                self.state.id().name().as_deref(),
                &mut req,
            )
            .await;
        }
        let has_owner_name = self.state.id().name().is_some();
        match routes::decide_pool_action(req.method().as_ref(), &path, has_owner_name) {
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

    /// Meter flush alarm: drains pending play windows to the api sink
    /// and re-arms itself while any remain ([`NntpPool::flush_meter`]).
    /// Failure paths reschedule explicitly; only when the reschedule
    /// itself fails (the alarm storage write) does this error out, so
    /// the platform's alarm retry is the backstop of last resort
    /// instead of a silently stranded book.
    async fn alarm(&self) -> Result<Response> {
        match self.flush_meter().await {
            Ok(()) => Response::empty(),
            Err(()) => Err(worker::Error::RustError(
                "meter flush could not reschedule its alarm".to_string(),
            )),
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

    /// Accounts one delivered-byte report for this object's owner:
    /// validate ([`crate::delivered`]), merge into the persisted meter
    /// book ([`MeterBook`]), and make sure a flush alarm is armed. The
    /// `202` acknowledge renders through [`crate::delivered::DeliveredReply`]
    /// exactly as before accounting existed; storage trouble answers
    /// `503` so the play route re-reports (duplicates merge to zero).
    async fn delivered(&self, req: &mut Request) -> ProbeReply {
        let Some(pool_user) = self.state.id().name() else {
            return route_reply(ProbeDecision::OwnerMissing);
        };
        let body = match read_capped(req, MAX_DELIVERED_REQUEST_BYTES).await {
            Ok(body) => body,
            Err(e) => return ProbeReply::from(Err(e)),
        };
        let interval = match parse_delivered_report(&body, &pool_user) {
            Ok(interval) => interval,
            Err(e) => {
                return crate::delivered::DeliveredReply::from(Result::<(), ProbeError>::Err(e))
                    .into()
            }
        };
        let storage_busy = || {
            crate::delivered::DeliveredReply::from(Result::<(), ProbeError>::Err(
                ProbeError::Busy {
                    retry_after_ms: 1000,
                },
            ))
        };
        let now_unix = Date::now().as_millis() / 1000;
        let mut book = match self.load_meter_book(&pool_user).await {
            Ok(book) => book,
            Err(()) => return storage_busy().into(),
        };
        if !book.record(&interval, now_unix) {
            return crate::delivered::DeliveredReply::from(Result::<(), ProbeError>::Err(
                ProbeError::BadRequest("report does not fit the meter book"),
            ))
            .into();
        }
        if self.save_meter_book(&book).await.is_err() {
            return storage_busy().into();
        }
        // The 202 tells the play route to stop retrying, so it is
        // only earned once a flush alarm demonstrably exists: on an
        // arming failure answer busy and let the route re-report —
        // the duplicate merges to zero and re-arms.
        if self.arm_meter_alarm(METER_FLUSH_DELAY_MS).await.is_err() {
            return storage_busy().into();
        }
        crate::delivered::DeliveredReply::from(Ok(())).into()
    }

    /// The persisted meter book for this object's owner, or a fresh
    /// one when storage holds none (or holds another owner's — the
    /// object id IS the user, so a mismatched book can only be
    /// corruption; starting over under-meters rather than misbills).
    async fn load_meter_book(&self, pool_user: &str) -> core::result::Result<MeterBook, ()> {
        let stored: Option<MeterBook> = self
            .state
            .storage()
            .get(meter_sink::METER_BOOK_KEY)
            .await
            .map_err(|_| ())?;
        Ok(match stored {
            Some(book) if book.user_id == pool_user => book,
            _ => MeterBook::new(pool_user),
        })
    }

    async fn save_meter_book(&self, book: &MeterBook) -> core::result::Result<(), ()> {
        self.state
            .storage()
            .put(meter_sink::METER_BOOK_KEY, book)
            .await
            .map_err(|_| ())
    }

    /// Arm the flush alarm unless one is already pending: one alarm
    /// drains the whole book, so re-arming per report would only push
    /// the flush later. `Err` means no alarm is known to exist — the
    /// caller must not acknowledge work as durably scheduled.
    async fn arm_meter_alarm(&self, delay_ms: i64) -> core::result::Result<(), ()> {
        let storage = self.state.storage();
        match storage.get_alarm().await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => storage.set_alarm(delay_ms).await.map_err(|_| ()),
            Err(_) => Err(()),
        }
    }

    /// Unconditionally (re)schedule the next flush attempt.
    async fn reschedule_meter_alarm(&self, delay_ms: i64) -> core::result::Result<(), ()> {
        self.state
            .storage()
            .set_alarm(delay_ms)
            .await
            .map_err(|_| ())
    }

    /// One flush pass: send the pending cumulative windows to the api
    /// sink, mark what it answered terminally, persist, and re-arm
    /// while anything remains. Every failure path keeps the windows
    /// pending and schedules a retry — a window is only ever dropped
    /// by the api naming it terminal, or by the flushed-retention
    /// eviction after an ack. `Err` is reserved for "the retry alarm
    /// itself could not be scheduled"; the alarm handler surfaces it
    /// so the platform retries the alarm instead of stranding the
    /// book.
    async fn flush_meter(&self) -> core::result::Result<(), ()> {
        let Some(pool_user) = self.state.id().name() else {
            return Ok(());
        };
        let Ok(mut book) = self.load_meter_book(&pool_user).await else {
            return self.reschedule_meter_alarm(METER_RETRY_DELAY_MS).await;
        };
        let now_unix = Date::now().as_millis() / 1000;
        let sent = book.pending_windows();
        if sent.is_empty() {
            book.evict_flushed(now_unix);
            let _ = self.save_meter_book(&book).await;
            return Ok(());
        }
        let Some(body) = meter_sink::batch_body(&sent) else {
            return self.reschedule_meter_alarm(METER_RETRY_DELAY_MS).await;
        };
        let secret = self
            .env
            .secret(meter_sink::METER_SINK_SECRET_VAR)
            .ok()
            .map(|value| value.to_string().into_bytes());
        let Some(authorization) =
            meter_sink::authorization_header(secret.as_deref(), now_unix, &body)
        else {
            // Unprovisioned or short secret: never flush unsigned.
            return self.reschedule_meter_alarm(METER_RETRY_DELAY_MS).await;
        };
        let ack = match self.post_meter_batch(&authorization, body).await {
            Ok(ack) => ack,
            Err(()) => {
                return self.reschedule_meter_alarm(METER_RETRY_DELAY_MS).await;
            }
        };
        // The fetch above awaited with the input gates open (they
        // only hold during storage operations), so /delivered may
        // have recorded and saved a newer book mid-flight. Apply the
        // ack to a RELOADED book: saving the pre-fetch copy would
        // erase those intervals, and their 202 already told the play
        // route not to retry. `mark_flushed` clamps to each play's
        // current union, so mid-flight growth stays pending. If the
        // reload fails, leave everything pending — re-sending the
        // same cumulative snapshots is a no-op at the api.
        let Ok(mut book) = self.load_meter_book(&pool_user).await else {
            return self.reschedule_meter_alarm(METER_RETRY_DELAY_MS).await;
        };
        book.mark_flushed(&sent, &ack.terminal_play_ids());
        book.evict_flushed(now_unix);
        if self.save_meter_book(&book).await.is_err() {
            // The api already converged these cumulative snapshots;
            // re-sending them after a storage failure is a no-op there.
            return self.reschedule_meter_alarm(METER_RETRY_DELAY_MS).await;
        }
        if book.has_pending() {
            return self.reschedule_meter_alarm(METER_CONTINUE_DELAY_MS).await;
        }
        Ok(())
    }

    /// POST one signed batch to the api sink and parse its ack. Any
    /// transport error, non-200 status, or unreadable ack retries via
    /// the next alarm: the api upsert is cumulative and monotonic, so
    /// re-sending a batch whose ack was lost adds zero.
    async fn post_meter_batch(
        &self,
        authorization: &str,
        body: Vec<u8>,
    ) -> core::result::Result<meter_sink::FlushAck, ()> {
        let fetcher = self.env.service(API_SERVICE_BINDING).map_err(|_| ())?;
        let headers = Headers::new();
        headers
            .set("content-type", "application/json")
            .map_err(|_| ())?;
        headers
            .set("authorization", authorization)
            .map_err(|_| ())?;
        let body_text = String::from_utf8(body).map_err(|_| ())?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(JsValue::from_str(&body_text)));
        let request = Request::new_with_init(API_SINK_URL, &init).map_err(|_| ())?;
        let mut response = fetcher.fetch_request(request).await.map_err(|_| ())?;
        if response.status_code() != 200 {
            return Err(());
        }
        // The ack names at most the batch's play ids; cap the read so
        // a hostile or broken endpoint cannot balloon the DO.
        let text = response.text().await.map_err(|_| ())?;
        if text.len() > 64 * 1024 {
            return Err(());
        }
        meter_sink::parse_ack(text.as_bytes()).ok_or(())
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
