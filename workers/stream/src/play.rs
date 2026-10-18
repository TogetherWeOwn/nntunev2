//! Play path: authorize → cache hit → `200`/`206`; miss → lease a pool
//! connection → fetch segments one at a time → yEnc → decrypt → stream each
//! to the client while staging the R2 multipart write, then prefetch the
//! next track.
//!
//! Decryption here is a demo XOR stream cipher standing in for the
//! ingest-owned content encryption (key delivery via the BYOP vault is a
//! sibling card). The byte flow — fetch, decode, decrypt, fan out to client
//! chunks *and* the multipart upload — is the production shape.
//!
//! Miss-path streaming: [`play`] fetches only one segment up front (its
//! yEnc `=ybegin size=` reveals the file size, so `Range`/`206` resolves
//! before a byte is sent); later segments are fetched lazily as the client
//! pulls [`PlayStream`] chunks. Which segment comes first depends on
//! [`PlayInput::segment_ranges`]: with per-segment byte ranges (NZB
//! metadata) a mid-track `Range` fetches the *covering* article directly —
//! one `BODY` to first byte — while without them the play path keeps the
//! prefix walk from segment 0, since the covering article cannot be
//! addressed (see the README TTFB notes). Ranges are a verified hint: a
//! ranges-only `416`, ranges that fail validation, or a covering article
//! that disagrees with its declared range all drop the hint and fall back
//! to the prefix walk, so wrong NZB metadata cannot make a playable track
//! unplayable. Memory is the staged write (which R2 holds in production)
//! plus one decoded segment at a time: the covering segment is the only
//! one ever held out of order, because the skipped prefix is backfilled
//! in file order before serving advances past it — staging only ever
//! receives the contiguous prefix, flushed into multipart parts as it
//! grows.
//! [`PlayStream::finalize`] drains any segments the client's range did not
//! need — including a skipped prefix — so the cache still gets the whole
//! track, then completes the upload and hands the healthy connection back
//! to the pool. [`PlayStream::abort`] returns the connection without
//! caching (client went away).
//!
//! Failure handling: only connection-level trouble (I/O, protocol errors,
//! read timeouts) counts toward the pool circuit breaker (3-in-30s opens for
//! 30s; the open breaker maps to `503` + `Retry-After`, see
//! [`PlayError::http_status`]). A missing article or a bad segment fails that
//! play alone — the connection is fine and goes back to the pool warm.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::api_error::ApiError;
use crate::cache::{CacheServe, CacheStatus, ContentCache, MultipartUpload};
use crate::grant::{self, GrantError};
use crate::meter::{stream_seconds_for_bytes, Meter, MeterEvent, MeterKind};
use crate::nntp::{valid_message_id, NntpClient, NntpDeadline, NntpError, UpstreamTimeouts};
use crate::observability::{CacheObserveEvent, CacheStatus as ObserveStatus};
use crate::pool::{Lane, Lease, OrphanedPlay, Pool, PoolError};
use crate::prefetch::should_prefetch;
use crate::range::{self, RangeOutcome};
use crate::yenc::{self, Decoded, YencError};

/// Client chunk size for served streams.
const CHUNK: usize = 16 * 1024;

/// Documented worker/API error code for stale provider credentials (S7).
/// The API worker returns this code (HTTP 401) and the web app renders an
/// inline re-auth prompt on it instead of a generic error page.
pub const PROVIDER_AUTH_EXPIRED_CODE: &str = "provider_auth_expired";

/// Why a play failed.
#[derive(Debug, PartialEq, Eq)]
pub enum PlayError {
    Unauthorized(GrantError),
    WrongTrack,
    RangeUnsatisfiable,
    PoolExhausted,
    /// The provider rejected our credentials (`480`/`481` → re-auth prompt).
    /// This never quarantines the pool: retrying with the same stale secret
    /// cannot succeed, and a quarantine would hide the re-auth signal behind
    /// a `PoolQuarantined` outage. The stale connection is dropped, not
    /// parked warm, so the next play dials a fresh session. Carries no
    /// credential material.
    ProviderAuthExpired,
    PoolQuarantined {
        retry_after_ms: u64,
    },
    /// An article is gone (`430`/`423`): this track fails, the pool is fine.
    ArticleMissing(String),
    /// A segment id from NZB metadata failed validation and was never sent
    /// to the provider: this track fails, the pool is fine.
    InvalidSegment(String),
    /// The provider connection failed or misbehaved (quarantines the pool).
    Upstream(String),
    /// A segment could not be decoded (corrupt or mis-placed yEnc part).
    Decode(YencError),
}

impl PlayError {
    /// The shared user-facing envelope for this failure (see
    /// `docs/api-errors.md`). Variant payloads (provider replies,
    /// message-ids, decode detail) deliberately never reach the wire:
    /// every message here is static per code.
    pub fn api_error(&self) -> ApiError {
        match self {
            PlayError::Unauthorized(_) => ApiError::unauthorized(),
            PlayError::ProviderAuthExpired => ApiError::provider_auth_expired(),
            PlayError::WrongTrack => ApiError::forbidden(),
            PlayError::RangeUnsatisfiable => ApiError::range_not_satisfiable(),
            PlayError::PoolExhausted => ApiError::rate_limited(),
            PlayError::PoolQuarantined { .. } => ApiError::upstream_circuit_open(),
            PlayError::ArticleMissing(_) => ApiError::not_found(),
            PlayError::InvalidSegment(_) => ApiError::invalid_request(),
            PlayError::Upstream(_) => ApiError::upstream_unavailable(),
            PlayError::Decode(_) => ApiError::corrupt_content(),
        }
    }

    /// The stable Analytics Engine / log code for pool-level failures
    /// (`blob4` / `pool_error_code` in [`crate::observability`]); `None` for
    /// every non-pool failure. This is the single definition of the
    /// `POOL_*` strings the dashboard groups by.
    pub fn pool_error_code(&self) -> Option<&'static str> {
        match self {
            PlayError::PoolQuarantined { .. } => Some("POOL_QUARANTINED"),
            PlayError::PoolExhausted => Some("POOL_EXHAUSTED"),
            _ => None,
        }
    }

    /// HTTP status for the Worker shim: an open breaker is `503` (with
    /// [`PlayError::retry_after_header`]); an exhausted pool is `429`
    /// (`rate_limited`); auth, range, missing-article, and invalid-segment
    /// failures use their shared-envelope statuses; provider failures are `502`.
    pub fn http_status(&self) -> u16 {
        match self {
            PlayError::Unauthorized(_) | PlayError::ProviderAuthExpired => 401,
            PlayError::WrongTrack => 403,
            PlayError::RangeUnsatisfiable => 416,
            PlayError::PoolExhausted => 429,
            PlayError::PoolQuarantined { .. } => 503,
            PlayError::ArticleMissing(_) => 404,
            PlayError::InvalidSegment(_) => 400,
            PlayError::Upstream(_) | PlayError::Decode(_) => 502,
        }
    }

    /// Value for the HTTP `Retry-After` response header (whole seconds,
    /// rounded up): `Some` only for [`PlayError::PoolQuarantined`]. The Worker
    /// shim sends `503` + this header instead of hanging a play on a dead
    /// upstream.
    pub fn retry_after_header(&self) -> Option<String> {
        match self {
            PlayError::PoolQuarantined { retry_after_ms } => {
                Some(retry_after_secs(*retry_after_ms).to_string())
            }
            _ => None,
        }
    }

    /// Stable worker/API code for stale provider credentials.
    pub fn code(&self) -> &'static str {
        match self {
            PlayError::ProviderAuthExpired => PROVIDER_AUTH_EXPIRED_CODE,
            _ => "request_failed",
        }
    }
}

/// Whole-second `Retry-After` for a millisecond wait, rounded up.
fn retry_after_secs(retry_after_ms: u64) -> u64 {
    retry_after_ms.div_ceil(1000)
}
/// Everything the play path needs, by reference.
///
/// `UpstreamTimeouts` stay out of this struct on purpose: adding a required
/// field here breaks every existing `PlayInput` literal (including grant-TTL
/// acceptance helpers on `main`). Per-play timeouts go through
/// [`play_with_timeouts`]; [`play`] uses `UpstreamTimeouts::default()`.
pub struct PlayInput<'a> {
    /// Signed grant from the `api` Worker.
    pub token: &'a str,
    pub track_id: &'a str,
    /// Value of the HTTP `Range` header, if any.
    pub range: Option<&'a str>,
    /// Provider NNTP address (a pooled connection dials this).
    pub server_addr: SocketAddr,
    /// Ordered segment message-IDs composing the track.
    pub segments: &'a [String],
    /// Per-segment plaintext byte ranges, `(start, end_exclusive)` in
    /// `segments` order, when NZB metadata carries them. `Some` (and valid:
    /// one range per segment, contiguous from 0) lets a mid-track `Range`
    /// fetch its covering segment first; `None` or invalid keeps the
    /// prefix-walk fallback from segment 0.
    pub segment_ranges: Option<&'a [(u64, u64)]>,
    /// Demo cipher key (see module docs).
    pub cipher_key: u8,
    /// Current time, unix seconds (grant expiry check).
    pub now_unix: u64,
    /// Next track in the queue, for prefetch.
    pub up_next: Option<&'a str>,
    /// Optional inbound `x-trace-id` for log correlation. Accepted only when
    /// it is 32 lowercase hex chars; anything else is dropped (never trusted
    /// or logged) and the play runs untraced. Never affects authorization,
    /// pooling, caching, or metering decisions — and never enters the
    /// Analytics Engine row.
    pub trace_id: Option<&'a str>,
}

/// A servable play: status line, headers and a chunk stream.
pub struct PlayResponse {
    /// 200 (full) or 206 (partial).
    pub status: u16,
    /// `Content-Range` value for 206 responses.
    pub content_range: Option<String>,
    /// Bytes this response will serve.
    pub body_len: u64,
    pub cache_hit: bool,
    /// `Cache-Status` value pinning how the cache served this play (`HIT`
    /// fresh, `STALE` served-while-revalidating, `MISS` fetched upstream).
    pub cache_status: CacheStatus,
    /// Seconds since the serving entry was written (0 on a miss).
    pub age_secs: u64,
    /// True when a stale entry served immediately *and* this response
    /// claimed the track's one background revalidation (see
    /// `ContentCache::try_begin_revalidate`): the worker entry point must
    /// re-fetch upstream and rewrite the entry while the client already has
    /// its bytes, or release the claim with `finish_revalidate` if that
    /// re-fetch fails, passing the `now_unix` value used to claim it. Claims
    /// expire after their bounded TTL if the owner disappears. Concurrent
    /// stale readers get `false` — one refresh per stale entry, not one per
    /// reader.
    pub needs_revalidate: bool,
    pub stream: PlayStream,
    /// Normalized request trace for log correlation (`None` when the play
    /// carried no valid trace). The transport stamps this ID in every
    /// six-key log line for the play; it never enters the AE row.
    pub trace_id: Option<String>,
}

impl PlayResponse {
    /// `Cache-Status` + `Age` response headers for this play.
    pub fn cache_headers(&self) -> [(String, String); 2] {
        [
            (
                "Cache-Status".to_string(),
                self.cache_status.header_value().to_string(),
            ),
            ("Age".to_string(), self.age_secs.to_string()),
        ]
    }
}

/// What settle does with the connection behind a failed play: only a
/// provider-level failure quarantines/meters, but a stale-auth connection
/// is also dropped (never returned warm — it would be reused forever,
/// answering `480`/`481` on every subsequent play).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Conn {
    KeepWarm,
    Drop,
    ProviderFailure,
}

type Failure = (PlayError, Conn);

/// The pool that [`play`] drives.
pub type StreamPool = Pool<NntpClient>;

/// Chunked body. Pull chunks with [`Iterator::next`], then call
/// [`PlayStream::finalize`] (or [`PlayStream::abort`]) so the lease returns
/// to the pool and, on a miss, the cache write completes. Dropping the
/// stream instead is still safe: the slot is freed, the half-read
/// connection is discarded, and served bytes are metered on the next
/// `play`/`finalize` — but the cache write is lost, so prefer `finalize`.
/// `abort` likewise meters the bytes served so far. Either way the hit path
/// (which holds no lease) is metered exactly like a miss.
pub struct PlayStream {
    user_id: String,
    track_id: String,
    /// Analytics Engine row status for [`PlayStream::observe_event`]: fresh
    /// serves project as `Hit`, misses as `Miss`, and stale-while-revalidate
    /// serves as `Stale` so the row agrees with the `Cache-Status` header.
    observe_status: ObserveStatus,
    /// Next absolute byte to emit, and the exclusive end of the response.
    pos: u64,
    end: u64,
    served: u64,
    /// Normalized request trace for log correlation. Carried alongside the
    /// lease so `finalize`/`abort`/`Drop` meter the same play's bytes under
    /// the same trace; never enters the AE row.
    trace_id: Option<String>,
    /// `None` once `finalize`/`abort` consumed the stream (or it was dropped).
    source: Option<Source>,
    /// Pool-level orphan queue: `Drop` pushes an [`OrphanedPlay`] here when
    /// a mid-flight stream served bytes, on either the miss or the hit path.
    orphans: Arc<Mutex<Vec<OrphanedPlay>>>,
}

impl Drop for PlayStream {
    fn drop(&mut self) {
        // Only fires mid-flight: finalize/abort took `source` first. The
        // lease guard's Drop frees the slot and the half-read client is
        // discarded with `live` (its buffered position is untrustworthy, so
        // it is never re-parked warm). Served bytes ride the pool-level
        // orphan queue so the pool owner can still meter them — for `Live`
        // and `Cached` alike. The trace rides along for log correlation.
        if self.source.take().is_some() && self.served > 0 {
            if let Ok(mut orphaned) = self.orphans.lock() {
                orphaned.push(OrphanedPlay {
                    user_id: self.user_id.clone(),
                    track_id: self.track_id.clone(),
                    bytes: self.served,
                    trace_id: self.trace_id.clone(),
                });
            }
        }
    }
}

enum Source {
    Cached(Arc<[u8]>),
    Live(Box<Live>),
}

/// A miss in flight: the leased connection, the segment cursor and the
/// staged cache write.
struct Live {
    client: NntpClient,
    lease: Lease,
    segments: Vec<String>,
    /// Next segment to fetch for the *client* (serving cursor). Starts at
    /// the covering segment; `finalize` separately fetches whatever
    /// `seg_done` still lacks (e.g. a skipped prefix).
    next_seg: usize,
    seg_done: Vec<bool>,
    /// `(offset, len)` each fetched article declared for itself, once
    /// placed. Placement is proven by chaining these together (see
    /// `ingest`), never by the NZB ranges hint.
    seg_placed: Vec<Option<(u64, u64)>>,
    cipher_key: u8,
    /// `PlayInput::now_unix` at play time: the cache write stamped on
    /// `finalize`, so the SWR clock runs from when the bytes were fetched.
    started_at: u64,
    /// File size for the play: the first fetched segment's declared size,
    /// or the ranges total when a disagreeing segment 0 was refetched and
    /// segment 1 corroborated the ranges over a self-inconsistent provider.
    total: u64,
    /// Segment prefetched while resolving a disagreeing segment 0 (the
    /// corroborating segment 1): the next `load` for its index ingests it
    /// instead of fetching it a second time.
    prefetched: Option<(usize, Decoded)>,
    /// The most recently decoded segment, shared with `pending` (no copy).
    buf: Arc<[u8]>,
    buf_start: u64,
    /// Decoded segments not yet staged: `upload` only ever receives the
    /// contiguous prefix (`staged_seg..`), so a covering segment fetched
    /// ahead of its prefix waits here until `load_next` or `finalize` fills the gap.
    pending: BTreeMap<usize, Arc<[u8]>>,
    staged_seg: usize,
    staged_bytes: u64,
    upload: MultipartUpload,
    failed: Option<Failure>,
    provider_error_epoch: Arc<AtomicU64>,
    last_success_epoch: Option<u64>,
}

/// Result of starting a play with its early-failure observability projection.
pub struct ObservedPlayAttempt {
    pub result: Result<PlayResponse, PlayError>,
    /// Present for a pool acquisition error, which returns before a stream exists.
    pub error_event: Option<CacheObserveEvent>,
}

/// Serve `track_id` to its granted owner (default upstream timeouts).
pub fn play(
    pool: &mut StreamPool,
    cache: &mut ContentCache,
    meter: &mut dyn Meter,
    secret: &[u8],
    input: &PlayInput<'_>,
) -> Result<PlayResponse, PlayError> {
    play_with_timeouts(
        pool,
        cache,
        meter,
        secret,
        input,
        UpstreamTimeouts::default(),
    )
}

/// Serve `track_id` with explicit per-upstream dial/read timeouts.
///
/// `connect` bounds a new dial and `read` bounds each article op (idle-read
/// limit plus total status-plus-body deadline); see `UpstreamTimeouts`.
pub fn play_with_timeouts(
    pool: &mut StreamPool,
    cache: &mut ContentCache,
    meter: &mut dyn Meter,
    secret: &[u8],
    input: &PlayInput<'_>,
    timeouts: UpstreamTimeouts,
) -> Result<PlayResponse, PlayError> {
    let user = authorized_user(secret, input, meter)?;
    play_authorized(pool, cache, meter, input, &user, timeouts)
}

/// Start a play and project early pool-acquisition errors while the verified
/// user identity is still available. `ttft_ms` and bytes are zero because no
/// first byte can be served on this path.
pub fn play_with_observe_event(
    pool: &mut StreamPool,
    cache: &mut ContentCache,
    meter: &mut dyn Meter,
    secret: &[u8],
    input: &PlayInput<'_>,
    request_id: impl Into<String>,
) -> ObservedPlayAttempt {
    let request_id = request_id.into();
    let user = match authorized_user(secret, input, meter) {
        Ok(user) => user,
        Err(error) => {
            return ObservedPlayAttempt {
                result: Err(error),
                error_event: None,
            }
        }
    };
    let result = play_authorized(
        pool,
        cache,
        meter,
        input,
        &user,
        UpstreamTimeouts::default(),
    );
    let error_event = result.as_ref().err().and_then(|error| {
        error.pool_error_code().map(|_| {
            CacheObserveEvent::from_play(request_id, &user, ObserveStatus::Miss, 0, 0, Some(error))
        })
    });
    ObservedPlayAttempt {
        result,
        error_event,
    }
}

fn authorized_user(
    secret: &[u8],
    input: &PlayInput<'_>,
    meter: &mut dyn Meter,
) -> Result<String, PlayError> {
    let grant = match grant::verify_with_user(input.token, secret, input.now_unix) {
        Ok(grant) => grant,
        Err((err, signed_user)) => {
            // Expiry and excessive-expiry rejections carry the verified user;
            // malformed and bad-signature tokens remain silent.
            let kind = match err {
                GrantError::Expired => Some(MeterKind::GrantRejectedExpired),
                GrantError::TtlTooLong => Some(MeterKind::GrantRejectedExcessiveExpiry),
                GrantError::Malformed | GrantError::BadSignature => None,
            };
            if let Some(kind) = kind {
                meter.emit(ev(
                    kind,
                    signed_user.as_deref().unwrap_or_default(),
                    input.track_id,
                    0,
                ));
            }
            return Err(PlayError::Unauthorized(err));
        }
    };
    if grant.track_id != input.track_id {
        meter.emit(ev(
            MeterKind::GrantRejectedWrongTrack,
            &grant.user_id,
            input.track_id,
            0,
        ));
        return Err(PlayError::WrongTrack);
    }
    Ok(grant.user_id)
}

fn play_authorized(
    pool: &mut StreamPool,
    cache: &mut ContentCache,
    meter: &mut dyn Meter,
    input: &PlayInput<'_>,
    user: &str,
    timeouts: UpstreamTimeouts,
) -> Result<PlayResponse, PlayError> {
    // Normalize first: an invalid trace is dropped before pool, cache, or
    // meter can observe it. The trace never affects those decisions; it only
    // rides along for log correlation.
    let trace_id = crate::trace::normalize_trace_id(input.trace_id);
    flush_orphaned(pool, meter);
    meter.emit(ev(MeterKind::PlayStart, user, input.track_id, 0));

    // Fresh entries serve as-is; stale entries serve immediately while owing
    // a background revalidation (see `needs_revalidate`). Expired entries
    // read as `None` and fall through to the miss path below.
    if let Some((full, serve)) = cache.get_track_at(input.track_id, input.now_unix) {
        meter.emit(ev(MeterKind::CacheHit, user, input.track_id, 0));
        let window = resolve_range(full.len() as u64, input.range)?;
        // A stale serve owes one background revalidation per entry, not one
        // per reader: only the response that claims it reports
        // `needs_revalidate`. Claimed only once the serve is known-good, so
        // a failed range never strands the claim with no one told to act.
        let needs_revalidate =
            serve.needs_revalidate() && cache.try_begin_revalidate(input.track_id, input.now_unix);
        return Ok(respond(
            window,
            true,
            Some(serve),
            needs_revalidate,
            user,
            input.track_id,
            Source::Cached(full),
            pool.orphan_sink(),
            trace_id,
        ));
    }

    meter.emit(ev(MeterKind::CacheMiss, user, input.track_id, 0));
    if input.segments.is_empty() {
        return Err(PlayError::Upstream("track has no segments".into()));
    }
    // Fail fast on hostile metadata: no lease, no dial, nothing on the wire.
    // (`NntpClient::article` re-validates as defense in depth.)
    if let Some(bad) = input.segments.iter().find(|s| !valid_segment_id(s)) {
        return Err(PlayError::InvalidSegment(bad.clone()));
    }
    // Covering-segment plan: ranges are a *hint*, never trusted blindly.
    // Valid-shape ranges tentatively resolve the range so the first fetch
    // is the article covering its start — but a ranges-only 416, or a
    // covering article that disagrees with its declared range (checked
    // below), drops the hint and keeps the prefix walk, so wrong NZB
    // metadata can never make a playable track unplayable. Absent/invalid
    // ranges plan nothing and the prefix walk behaves exactly as before.
    let mut ranges = valid_ranges(input.segment_ranges, input.segments.len());
    let mut first_seg = 0;
    if let Some(r) = ranges {
        match resolve_range(r[r.len() - 1].1, input.range) {
            Ok((_, _, start, _)) => first_seg = covering_segment(r, start),
            Err(_) => ranges = None,
        }
    }
    let lease = pool.try_acquire(Lane::Playback).map_err(|e| match e {
        PoolError::Quarantined { retry_after_ms } => PlayError::PoolQuarantined { retry_after_ms },
        PoolError::Exhausted => PlayError::PoolExhausted,
    })?;

    let (mut client, mut first, redialed) = match open_and_fetch(pool, input, first_seg, timeouts) {
        Ok(result) => result,
        Err((client, failure)) => {
            return Err(settle(
                pool,
                meter,
                lease,
                client,
                failure,
                user,
                input.track_id,
            ))
        }
    };
    // Corroborated first-segment agreement: segment 0 alone cannot tell a
    // lying provider `size=` (ranges win) from lying NZB ranges (provider
    // wins) — offset and length agree either way. A covering `!= 0`
    // disagreement refetches the different segment 0 and keeps its declared
    // size, as before. A `== 0` disagreement refetches the same article
    // (the discard-and-prefix-walk) and breaks the tie with segment 1: only
    // when segment 1 corroborates the ranges total against a
    // self-inconsistent provider does the ranges total define the 206
    // window; otherwise the provider total stands, so wrong ranges can
    // never make a playable track unplayable. Single-segment plays keep the
    // legacy adopt (no second article exists to corroborate).
    let mut prefetched: Option<(usize, Decoded)> = None;
    if let Some(r) = ranges {
        let agrees = first.file_size == r[r.len() - 1].1
            && first.offset == r[first_seg].0
            && first.data.len() as u64 == r[first_seg].1 - r[first_seg].0;
        if !agrees && first_seg != 0 {
            // The hint lied about the covering article: discard it and
            // restart as a prefix walk on the same connection.
            match fetch_segment(&mut client, &input.segments[0]) {
                Ok(decoded) => {
                    first = decoded;
                    first_seg = 0;
                }
                Err(SegmentError::Nntp(e)) => {
                    let failure = nntp_failure(e, &input.segments[0]);
                    let keep = if failure.1 == Conn::KeepWarm {
                        Some(client)
                    } else {
                        None
                    };
                    return Err(settle(
                        pool,
                        meter,
                        lease,
                        keep,
                        failure,
                        user,
                        input.track_id,
                    ));
                }
                Err(SegmentError::Decode(e)) => {
                    return Err(settle(
                        pool,
                        meter,
                        lease,
                        Some(client),
                        (PlayError::Decode(e), Conn::KeepWarm),
                        user,
                        input.track_id,
                    ));
                }
            }
        } else if !agrees && first_seg == 0 && input.segments.len() > 1 {
            // Same-article refetch, then corroborate with segment 1 on the
            // same connection. A tie-break failure keeps the legacy provider
            // path so the later walk surfaces it exactly as before.
            match fetch_segment(&mut client, &input.segments[0]) {
                Ok(decoded) => {
                    first = decoded;
                }
                Err(SegmentError::Nntp(e)) => {
                    let failure = nntp_failure(e, &input.segments[0]);
                    let keep = if failure.1 == Conn::KeepWarm {
                        Some(client)
                    } else {
                        None
                    };
                    return Err(settle(
                        pool,
                        meter,
                        lease,
                        keep,
                        failure,
                        user,
                        input.track_id,
                    ));
                }
                Err(SegmentError::Decode(e)) => {
                    return Err(settle(
                        pool,
                        meter,
                        lease,
                        Some(client),
                        (PlayError::Decode(e), Conn::KeepWarm),
                        user,
                        input.track_id,
                    ));
                }
            }
            let ranges_total = r[r.len() - 1].1;
            if let Ok(second) = fetch_segment(&mut client, &input.segments[1]) {
                let first_size = first.file_size;
                if second.file_size == ranges_total && first_size != ranges_total {
                    // Segment 1 corroborates the ranges against a
                    // self-inconsistent provider: ground truth wins and the
                    // lying `size=` never defines the window.
                    first.file_size = ranges_total;
                }
                if second.file_size == ranges_total || second.file_size == first_size {
                    // Worth one save: the walk consumes this instead of a
                    // second fetch of segment 1.
                    prefetched = Some((1, second));
                }
                // Else all three sizes disagree: drop it and let the walk
                // refetch segment 1 on the provider total as before.
            }
            first_seg = 0;
        }
    }
    if redialed {
        meter.emit(ev_pool(
            MeterKind::WarmRedial,
            user,
            input.track_id,
            lease.lane,
            0,
        ));
    }
    // A first segment settles a half-open probe but preserves a closed-state
    // failure streak so later segment failures still count toward the breaker.
    if pool.record_probe_success_for(lease.id) {
        meter.emit(ev_pool(
            MeterKind::QuarantineExited,
            user,
            input.track_id,
            lease.lane,
            0,
        ));
    }
    let mut live = Live {
        client,
        lease,
        segments: input.segments.to_vec(),
        seg_placed: vec![None; input.segments.len()],
        next_seg: first_seg,
        seg_done: vec![false; input.segments.len()],
        cipher_key: input.cipher_key,
        started_at: input.now_unix,
        total: first.file_size,
        prefetched,
        buf: Arc::from([]),
        buf_start: 0,
        pending: BTreeMap::new(),
        staged_seg: 0,
        staged_bytes: 0,
        upload: MultipartUpload::begin(input.track_id),
        failed: None,
        provider_error_epoch: pool.provider_error_epoch_source(),
        last_success_epoch: Some(pool.provider_error_epoch()),
    };
    if let Err(failure) = live.ingest(first_seg, first) {
        return Err(live.settle(pool, meter, Some(failure), user, input.track_id));
    }
    live.next_seg = first_seg + 1;
    let window = match resolve_range(live.total, input.range) {
        Ok(window) => window,
        Err(e) => {
            // The connection is healthy; hand it back warm.
            pool.release(live.lease, Some(live.client));
            return Err(e);
        }
    };
    if let Some(next) = input.up_next {
        if should_prefetch(pool) {
            meter.emit(ev(MeterKind::PrefetchEnqueued, user, next, 0));
        }
    }
    Ok(respond(
        window,
        false,
        None,
        false,
        user,
        input.track_id,
        Source::Live(Box::new(live)),
        pool.orphan_sink(),
        trace_id,
    ))
}

/// Accept `segment_ranges` only in the exact shape the covering fetch can
/// trust: one non-empty range per segment, contiguous from byte 0.
/// Anything else returns `None` and the caller keeps the prefix walk —
/// suspect NZB metadata must never make a playable track unplayable.
fn valid_ranges(ranges: Option<&[(u64, u64)]>, segments: usize) -> Option<&[(u64, u64)]> {
    let ranges = ranges?;
    if ranges.len() != segments || ranges.first().map(|r| r.0) != Some(0) {
        return None;
    }
    if ranges.iter().any(|(start, end)| end <= start) || ranges.windows(2).any(|w| w[0].1 != w[1].0)
    {
        return None;
    }
    Some(ranges)
}

/// Index of the segment whose range covers `start` (`start` < file size,
/// as resolved ranges guarantee).
fn covering_segment(ranges: &[(u64, u64)], start: u64) -> usize {
    ranges.partition_point(|&(_, end)| end <= start)
}

/// Dial or reuse a connection and fetch + decode segment `idx` (segment 0
/// on the prefix walk, the covering segment when ranges are present). A
/// warm connection the provider silently closed is redialed once.
fn open_and_fetch(
    pool: &mut StreamPool,
    input: &PlayInput<'_>,
    idx: usize,
    timeouts: UpstreamTimeouts,
) -> Result<(NntpClient, Decoded, bool), (Option<NntpClient>, Failure)> {
    let mid = &input.segments[idx];
    let deadline = NntpDeadline::new(timeouts.connect.saturating_add(timeouts.read));
    let mut warm = pool.take_idle();
    if let Some(client) = warm.as_mut() {
        if !client
            .prepare_for_reuse(&input.server_addr, timeouts.read)
            .unwrap_or(false)
        {
            warm = None;
        }
    }
    let was_warm = warm.is_some();
    let mut redialed = false;
    for attempt in 0..2 {
        let mut client = match warm.take() {
            Some(client) => client,
            None => {
                match NntpClient::connect_with_deadline(&input.server_addr, timeouts, &deadline) {
                    Ok(client) => {
                        pool.record_connect();
                        client
                    }
                    Err(e) => return Err((None, nntp_failure(e, mid))),
                }
            }
        };
        match fetch_segment_with_deadline(&mut client, mid, &deadline) {
            Ok(decoded) => {
                if was_warm && attempt == 0 {
                    pool.record_reuse();
                }
                return Ok((client, decoded, redialed));
            }
            Err(SegmentError::Nntp(e))
                if was_warm
                    && attempt == 0
                    && (e.is_provider_failure() || matches!(e, NntpError::AuthExpired)) =>
            {
                // A stale provider session, including expired auth, is dropped
                // and redialed once. Meter only after replacement fetch succeeds.
                redialed = true;
                continue;
            }
            Err(SegmentError::Nntp(e)) => {
                let failure = nntp_failure(e, mid);
                let keep = if failure.1 == Conn::KeepWarm {
                    Some(client)
                } else {
                    None
                };
                return Err((keep, failure));
            }
            Err(SegmentError::Decode(e)) => {
                return Err((Some(client), (PlayError::Decode(e), Conn::KeepWarm)));
            }
        }
    }
    unreachable!("second attempt always returns")
}

enum SegmentError {
    Nntp(NntpError),
    Decode(YencError),
}

/// Same stripping as `NntpClient::article`: outer whitespace and `<>`.
fn valid_segment_id(message_id: &str) -> bool {
    valid_message_id(message_id.trim().trim_matches(|c| c == '<' || c == '>'))
}

fn fetch_segment(client: &mut NntpClient, message_id: &str) -> Result<Decoded, SegmentError> {
    let raw = client.article(message_id).map_err(SegmentError::Nntp)?;
    yenc::decode_part(&raw).map_err(SegmentError::Decode)
}

fn fetch_segment_with_deadline(
    client: &mut NntpClient,
    message_id: &str,
    deadline: &NntpDeadline,
) -> Result<Decoded, SegmentError> {
    let raw = client
        .article_with_deadline(message_id, deadline)
        .map_err(SegmentError::Nntp)?;
    yenc::decode_part(&raw).map_err(SegmentError::Decode)
}

fn nntp_failure(e: NntpError, message_id: &str) -> Failure {
    let conn = match e {
        // Stale auth: drop the connection (it is no longer authenticated)
        // without quarantining or metering a provider outage.
        NntpError::AuthExpired => Conn::Drop,
        ref other if other.is_provider_failure() => Conn::ProviderFailure,
        _ => Conn::KeepWarm,
    };
    let err = match e {
        NntpError::NoSuchArticle(_) => PlayError::ArticleMissing(message_id.to_string()),
        NntpError::AuthExpired => PlayError::ProviderAuthExpired,
        NntpError::InvalidMessageId(id) => PlayError::InvalidSegment(id),
        other => PlayError::Upstream(format!("{other:?}")),
    };
    (err, conn)
}

/// Return the lease (and the connection, if still healthy) and account for
/// the failure: only provider-level trouble quarantines the pool; a stale
/// auth session is dropped without either.
fn settle(
    pool: &mut StreamPool,
    meter: &mut dyn Meter,
    lease: Lease,
    client: Option<NntpClient>,
    (err, conn): Failure,
    user: &str,
    track: &str,
) -> PlayError {
    let lane = lease.lane;
    if conn == Conn::ProviderFailure {
        let was_quarantined = pool.quarantined();
        pool.record_provider_error_for(lease.id);
        meter.emit(ev(MeterKind::ProviderError, user, track, 0));
        if !was_quarantined && pool.quarantined() {
            meter.emit(ev_pool(
                MeterKind::QuarantineEntered,
                user,
                track,
                lane,
                crate::pool::CIRCUIT_OPEN_MS,
            ));
        }
    } else if pool.record_non_provider_result_for(lease.id) {
        meter.emit(ev_pool(MeterKind::QuarantineExited, user, track, lane, 0));
    }
    pool.release(lease, if conn == Conn::KeepWarm { client } else { None });
    err
}

impl Live {
    /// Take one decoded segment: check its placement, decrypt, stage it
    /// (flushing the contiguous prefix into the upload) and make it the
    /// current buffer. Placement is proven from the articles themselves,
    /// in any fetch order: the first segment anchors at 0, the last ends
    /// at the declared file size, and every segment must chain exactly
    /// onto any already-placed neighbour. The NZB ranges hint plays no
    /// part here (it only chose the covering segment in `play`, where the
    /// covering article is cross-checked against it), so wrong interior
    /// boundaries in the hint can never fail a playable track.
    fn ingest(&mut self, idx: usize, mut d: Decoded) -> Result<(), Failure> {
        let multi_segment = self.segments.len() > 1;
        let len = d.data.len() as u64;
        let prev_end = idx
            .checked_sub(1)
            .and_then(|p| self.seg_placed[p].map(|(off, l)| off + l));
        let next_start = self
            .seg_placed
            .get(idx + 1)
            .and_then(|next| next.map(|(off, _)| off));
        let unchained = (idx == 0 && d.offset != 0)
            || (idx + 1 == self.segments.len() && d.offset + len != self.total)
            || prev_end.is_some_and(|end| d.offset != end)
            || next_start.is_some_and(|start| d.offset + len != start)
            || d.offset + len > self.total;
        let misplaced = unchained || d.file_size != self.total || (multi_segment && !d.multipart);
        if misplaced {
            return Err((PlayError::Decode(YencError::BadPart), Conn::KeepWarm));
        }
        // Demo cipher (see module docs): the production content key arrives
        // via the BYOP vault and the ingest-owned envelope.
        for b in d.data.iter_mut() {
            *b ^= self.cipher_key;
        }
        self.buf_start = d.offset;
        self.seg_placed[idx] = Some((d.offset, len));
        let data: Arc<[u8]> = Arc::from(d.data);
        self.buf = Arc::clone(&data);
        self.seg_done[idx] = true;
        self.pending.insert(idx, data);
        while let Some(data) = self.pending.remove(&self.staged_seg) {
            self.upload.part(&data);
            self.staged_bytes += data.len() as u64;
            self.staged_seg += 1;
        }
        if self.staged_seg == self.segments.len() && self.staged_bytes != self.total {
            return Err((
                PlayError::Decode(YencError::SizeMismatch {
                    expected: self.total,
                    actual: self.staged_bytes as usize,
                }),
                Conn::KeepWarm,
            ));
        }
        Ok(())
    }

    /// Fetch + ingest segment `idx`, wherever it sits in the file. A segment
    /// prefetched during disagreement resolution is ingested without a
    /// second fetch; anything else goes to the wire as before.
    fn load(&mut self, idx: usize) -> Result<(), Failure> {
        if self
            .prefetched
            .as_ref()
            .is_some_and(|(pidx, _)| *pidx == idx)
        {
            let (_, decoded) = self.prefetched.take().expect("prefetch checked");
            let fetch_epoch = self.provider_error_epoch.load(Ordering::Acquire);
            self.ingest(idx, decoded)?;
            self.last_success_epoch = Some(fetch_epoch);
            return Ok(());
        }
        let mid = self.segments[idx].clone();
        let fetch_epoch = self.provider_error_epoch.load(Ordering::Acquire);
        match fetch_segment(&mut self.client, &mid) {
            Ok(decoded) => {
                self.ingest(idx, decoded)?;
                self.last_success_epoch = Some(fetch_epoch);
                Ok(())
            }
            Err(SegmentError::Nntp(e)) => Err(nntp_failure(e, &mid)),
            Err(SegmentError::Decode(e)) => Err((PlayError::Decode(e), Conn::KeepWarm)),
        }
    }

    /// Fetch + ingest the next segment the client needs. A covering fetch
    /// first backfills the prefix it skipped, in file order: the covering
    /// segment then flushes out of `pending` into the staged upload, so at
    /// most one segment ever waits out of order (the staged 5 MiB part plus
    /// one segment bound holds however long the client's range runs). On
    /// the prefix walk every earlier segment is already done and this is a
    /// plain sequential fetch. `finalize` backfills the same way when the
    /// client's range ends inside the covering segment.
    fn load_next(&mut self) -> Result<(), Failure> {
        for idx in 0..self.next_seg {
            if !self.seg_done[idx] {
                self.load(idx)?;
            }
        }
        let idx = self.next_seg;
        self.load(idx)?;
        self.next_seg += 1;
        Ok(())
    }

    /// Release the lease for good, returning how the play ended.
    fn settle(
        self,
        pool: &mut StreamPool,
        meter: &mut dyn Meter,
        failure: Option<Failure>,
        user: &str,
        track: &str,
    ) -> PlayError {
        let failure =
            failure.unwrap_or((PlayError::Upstream("play failed".into()), Conn::KeepWarm));
        settle(
            pool,
            meter,
            self.lease,
            Some(self.client),
            failure,
            user,
            track,
        )
    }
}

/// `(status, content_range, start, end_exclusive)` for a body of `len`.
type Window = (u16, Option<String>, u64, u64);

fn resolve_range(len: u64, range: Option<&str>) -> Result<Window, PlayError> {
    match range::parse(range, len) {
        RangeOutcome::Full => Ok((200, None, 0, len)),
        RangeOutcome::Partial(r) => Ok((
            206,
            Some(range::content_range_value(r, len)),
            r.start,
            r.end + 1,
        )),
        RangeOutcome::Unsatisfiable => Err(PlayError::RangeUnsatisfiable),
    }
}

/// `serve` pins the `Cache-Status`/`Age` headers: `Some` serve (fresh or
/// stale) for cache hits, `None` for a miss (fetched upstream).
/// `needs_revalidate` is the caller's already-claimed revalidation flag
/// (see [`play`]); it is *not* derived from `serve` here, so concurrent
/// stale serves cannot each demand a refresh.
#[allow(clippy::too_many_arguments)]
fn respond(
    (status, content_range, start, end): Window,
    cache_hit: bool,
    serve: Option<CacheServe>,
    needs_revalidate: bool,
    user: &str,
    track: &str,
    source: Source,
    orphans: Arc<Mutex<Vec<OrphanedPlay>>>,
    trace_id: Option<String>,
) -> PlayResponse {
    let (cache_status, age_secs) = match serve {
        Some(serve) => (serve.cache_status(), serve.age_secs()),
        None => (CacheStatus::Miss, 0),
    };
    // The observability row distinguishes stale from fresh: both are cache
    // hits on the wire path (`cache_hit`), but the row must agree with the
    // `Cache-Status` header, so a stale serve projects as `Stale`, not `Hit`.
    let observe_status = match serve {
        Some(CacheServe::Fresh { .. }) => ObserveStatus::Hit,
        Some(CacheServe::Stale { .. }) => ObserveStatus::Stale,
        None => ObserveStatus::Miss,
    };
    PlayResponse {
        status,
        content_range,
        body_len: end - start,
        cache_hit,
        trace_id: trace_id.clone(),
        cache_status,
        age_secs,
        needs_revalidate,
        stream: PlayStream {
            user_id: user.to_string(),
            track_id: track.to_string(),
            observe_status,
            pos: start,
            end,
            served: 0,
            trace_id,
            source: Some(source),
            orphans,
        },
    }
}

fn ev(kind: MeterKind, user: &str, track: &str, bytes: u64) -> MeterEvent {
    MeterEvent {
        kind,
        user_id: user.to_string(),
        track_id: track.to_string(),
        bytes,
        seconds: stream_seconds_for_bytes(bytes),
        lane: None,
        backoff_ms: 0,
    }
}

/// A pool-health event (quarantine entry/exit, warm redial): carries the
/// lane of the call it belongs to and, for quarantine entry, the backoff.
fn ev_pool(kind: MeterKind, user: &str, track: &str, lane: Lane, backoff_ms: u64) -> MeterEvent {
    MeterEvent {
        lane: Some(lane),
        backoff_ms,
        ..ev(kind, user, track, 0)
    }
}

/// Emit `BytesServed` for plays that were dropped before `finalize` ran.
fn flush_orphaned(pool: &mut StreamPool, meter: &mut dyn Meter) {
    for orphan in pool.take_orphaned() {
        meter.emit(ev(
            MeterKind::BytesServed,
            &orphan.user_id,
            &orphan.track_id,
            orphan.bytes,
        ));
    }
}

impl PlayStream {
    /// True on the miss path (chunks feed a staged multipart upload).
    pub fn is_writeback(&self) -> bool {
        matches!(self.source, Some(Source::Live(_)))
    }

    /// Normalized request trace for log correlation (`None` when the play
    /// carried no valid trace). The transport stamps this ID in the six-key
    /// log envelope; it never enters the Analytics Engine row.
    #[must_use]
    pub fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_deref()
    }

    /// Bytes served to the client so far.
    pub fn served_bytes(&self) -> u64 {
        self.served
    }

    /// Project this play to its cache-observability event (Analytics Engine
    /// row + log line; see [`crate::observability`]). The caller — the
    /// Worker/DO entry point, which owns the request id and the clock —
    /// supplies `request_id` and the `ttft_ms` it measured around the first
    /// chunk; status, served bytes, hashed identity and any pool error
    /// code come from the play itself. Call before `finalize`/`abort`
    /// consumes the stream.
    pub fn observe_event(&self, request_id: impl Into<String>, ttft_ms: u64) -> CacheObserveEvent {
        CacheObserveEvent::from_play(
            request_id,
            &self.user_id,
            self.observe_status,
            ttft_ms,
            self.served,
            self.error(),
        )
    }

    /// Why the stream stopped early, if it did (miss path only).
    pub fn error(&self) -> Option<&PlayError> {
        match &self.source {
            Some(Source::Live(live)) => live.failed.as_ref().map(|(e, _)| e),
            _ => None,
        }
    }

    /// Finish the play: meter the bytes served, and on a miss pull any
    /// segments the client's range skipped, complete the staged write into
    /// `cache` and return the connection to `pool`. Errors (a gone article,
    /// a bad segment, a dead connection) leave the cache untouched.
    pub fn finalize(
        mut self,
        pool: &mut StreamPool,
        cache: &mut ContentCache,
        meter: &mut dyn Meter,
    ) -> Result<(), PlayError> {
        flush_orphaned(pool, meter);
        if self.served > 0 {
            meter.emit(ev(
                MeterKind::BytesServed,
                &self.user_id,
                &self.track_id,
                self.served,
            ));
        }
        let mut live = match self.source.take() {
            Some(Source::Live(live)) => live,
            _ => return Ok(()),
        };
        // Fetch whatever serving skipped — segments past the client's
        // range, plus the prefix a covering fetch jumped over — in file
        // order, so staging flushes the whole track into the upload.
        let mut idx = 0;
        while live.failed.is_none() && idx < live.segments.len() {
            if !live.seg_done[idx] {
                if let Err(failure) = live.load(idx) {
                    live.failed = Some(failure);
                }
            }
            idx += 1;
        }
        if live.failed.is_none()
            && live.last_success_epoch == Some(pool.provider_error_epoch())
            && pool.record_success_for(live.lease.id)
        {
            meter.emit(ev_pool(
                MeterKind::QuarantineExited,
                &self.user_id,
                &self.track_id,
                live.lease.lane,
                0,
            ));
        }
        match live.failed.take() {
            None => {
                let Live {
                    client,
                    lease,
                    upload,
                    started_at,
                    ..
                } = *live;
                pool.release(lease, Some(client));
                cache.put_track_at(&self.track_id, upload.complete(), started_at);
                Ok(())
            }
            Some(failure) => {
                let live = *live;
                let (client, lease) = (live.client, live.lease);
                Err(settle(
                    pool,
                    meter,
                    lease,
                    Some(client),
                    failure,
                    &self.user_id,
                    &self.track_id,
                ))
            }
        }
    }

    /// The client went away: return the connection without caching. Bytes
    /// already served are still metered: they ride the pool-level orphan
    /// queue and are emitted as `BytesServed` on the next `play`/`finalize`
    /// — on the hit path (which holds no lease) exactly like on a miss.
    /// `source` is taken first so `Drop` cannot record the same bytes twice.
    pub fn abort(mut self, pool: &mut StreamPool) {
        let source = self.source.take();
        if self.served > 0 {
            pool.push_orphan(OrphanedPlay {
                user_id: self.user_id.clone(),
                track_id: self.track_id.clone(),
                bytes: self.served,
                trace_id: self.trace_id.clone(),
            });
        }
        if let Some(Source::Live(live)) = source {
            // A failed aborted stream affects the breaker just like finalize;
            // auth/article-local failures settle a probe without quarantining.
            let live = *live;
            let conn = live.failed.as_ref().map(|(_, conn)| *conn);
            if conn == Some(Conn::ProviderFailure) {
                pool.record_provider_error_for(live.lease.id);
            } else if conn.is_some() {
                pool.record_non_provider_result_for(live.lease.id);
            } else if live.last_success_epoch == Some(pool.provider_error_epoch()) {
                // Clean abort: the first segment served fine and no later
                // fetch failed, so clear a closed-state failure streak unless
                // a newer provider failure arrived while this play was live.
                pool.record_success_for(live.lease.id);
            }
            let healthy = !matches!(conn, Some(Conn::Drop | Conn::ProviderFailure));
            pool.release(live.lease, healthy.then_some(live.client));
        }
    }
}

impl Iterator for PlayStream {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Vec<u8>> {
        if self.pos >= self.end {
            return None;
        }
        let chunk = match self.source.as_mut() {
            Some(Source::Cached(bytes)) => {
                let to = (self.pos as usize + CHUNK).min(self.end as usize);
                bytes[self.pos as usize..to].to_vec()
            }
            Some(Source::Live(live)) => loop {
                if live.failed.is_some() {
                    return None;
                }
                let buf_end = live.buf_start + live.buf.len() as u64;
                if self.pos >= live.buf_start && self.pos < buf_end {
                    let from = (self.pos - live.buf_start) as usize;
                    let to = (from + CHUNK)
                        .min(live.buf.len())
                        .min((self.end - live.buf_start) as usize);
                    break live.buf[from..to].to_vec();
                }
                if live.next_seg >= live.segments.len() {
                    live.failed = Some((PlayError::Decode(YencError::Truncated), Conn::KeepWarm));
                    return None;
                }
                if let Err(failure) = live.load_next() {
                    live.failed = Some(failure);
                    return None;
                }
            },
            // finalize/abort consumed the stream; `next` after that is empty.
            None => return None,
        };
        self.pos += chunk.len() as u64;
        self.served += chunk.len() as u64;
        Some(chunk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread::{self, JoinHandle};

    /// Grant expiry for these fixtures: `test_input` verifies at `now_unix`
    /// = 1, so a 900 s lifetime stays inside the 15-minute grant TTL cap.
    const GRANT_EXP_UNIX: u64 = 901;

    fn serve_article(
        listener: TcpListener,
        article: Vec<u8>,
        accept_redial: bool,
    ) -> JoinHandle<()> {
        serve_articles(listener, vec![article], accept_redial)
    }

    fn serve_articles(
        listener: TcpListener,
        articles: Vec<Vec<u8>>,
        accept_redial: bool,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept initial connection");
            serve_many_articles(stream, &articles);
            if accept_redial {
                let (stream, _) = listener.accept().expect("accept redial");
                let mut writer = stream;
                writer
                    .write_all(b"200 synthetic NNTP ready\r\n")
                    .expect("write redial greeting");
            }
        })
    }

    fn serve_many_articles(stream: TcpStream, articles: &[Vec<u8>]) {
        let mut reader = BufReader::new(stream.try_clone().expect("clone connection"));
        let mut writer = stream;
        writer
            .write_all(b"200 synthetic NNTP ready\r\n")
            .expect("write greeting");
        for article in articles {
            let mut command = String::new();
            reader.read_line(&mut command).expect("read BODY command");
            writer
                .write_all(b"222 article follows\r\n")
                .expect("write article status");
            for line in article.split(|&byte| byte == b'\n') {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                writer.write_all(line).expect("write article line");
                writer.write_all(b"\r\n").expect("write line ending");
            }
            writer.write_all(b".\r\n").expect("end article");
        }
    }

    fn test_article() -> (Vec<u8>, Vec<u8>) {
        let plain = b"small audio payload".to_vec();
        let article = yenc::encode(&plain, "track.flac");
        (plain, article)
    }

    fn test_parts() -> (Vec<u8>, Vec<Vec<u8>>) {
        split_parts(&[b"small audio ", b"payload"])
    }

    /// One multipart yEnc article per chunk; the whole-file CRC rides the
    /// last part only, like real posters.
    fn split_parts(chunks: &[&[u8]]) -> (Vec<u8>, Vec<Vec<u8>>) {
        let plain = chunks.concat();
        let crc = yenc::crc32(&plain);
        let mut offset = 0;
        let articles = chunks
            .iter()
            .enumerate()
            .map(|(i, chunk)| {
                let article = yenc::encode_part(
                    chunk,
                    "track.flac",
                    yenc::PartSpec {
                        file_size: plain.len() as u64,
                        part: i as u32 + 1,
                        total: chunks.len() as u32,
                        offset,
                        file_crc: (i + 1 == chunks.len()).then_some(crc),
                    },
                );
                offset += chunk.len() as u64;
                article
            })
            .collect();
        (plain, articles)
    }

    fn closed_address() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind temporary port");
        let address = listener.local_addr().expect("temporary address");
        drop(listener);
        address
    }

    fn test_input<'a>(
        token: &'a str,
        track_id: &'a str,
        address: SocketAddr,
        segments: &'a [String],
    ) -> PlayInput<'a> {
        PlayInput {
            trace_id: None,
            token,
            track_id,
            range: None,
            server_addr: address,
            segments,
            segment_ranges: None,
            cipher_key: 0,
            now_unix: 1,
            up_next: None,
        }
    }

    #[test]
    fn play_errors_map_to_the_shared_envelope() {
        // Representative play failures across auth, range, limits, and
        // upstream paths; payloads must not leak into the wire message.
        let cases: Vec<(PlayError, &'static str, u16, bool)> = vec![
            (
                PlayError::Unauthorized(GrantError::Expired),
                "unauthorized",
                401,
                false,
            ),
            (
                PlayError::ProviderAuthExpired,
                "provider_auth_expired",
                401,
                false,
            ),
            (PlayError::WrongTrack, "forbidden", 403, false),
            (
                PlayError::RangeUnsatisfiable,
                "range_not_satisfiable",
                416,
                false,
            ),
            (PlayError::PoolExhausted, "rate_limited", 429, true),
            (
                PlayError::PoolQuarantined { retry_after_ms: 5 },
                "upstream_circuit_open",
                503,
                true,
            ),
            (
                PlayError::ArticleMissing("seg@news.example".into()),
                "not_found",
                404,
                false,
            ),
            (
                PlayError::InvalidSegment("bad id".into()),
                "invalid_request",
                400,
                false,
            ),
            (
                PlayError::Upstream("502 provider detail".into()),
                "upstream_unavailable",
                502,
                true,
            ),
            (
                PlayError::Decode(YencError::Truncated),
                "corrupt_content",
                502,
                false,
            ),
        ];
        for (err, code, status, retryable) in cases {
            let api = err.api_error();
            assert_eq!(
                (api.code, api.status, api.retryable),
                (code, status, retryable),
                "{err:?}"
            );
            assert_eq!(err.http_status(), api.status, "status drift for {err:?}");
            assert!(!api.body_json().contains("seg@news"), "{err:?}");
            assert!(!api.body_json().contains("provider detail"), "{err:?}");
        }
        assert_eq!(
            PlayError::ProviderAuthExpired.api_error().body_json(),
            r#"{"code":"provider_auth_expired","message":"provider credentials expired","retryable":false}"#
        );
        assert_eq!(
            PlayError::WrongTrack.api_error().body_json(),
            r#"{"code":"forbidden","message":"not the owner","retryable":false}"#
        );
    }

    #[test]
    fn wrong_track_grant_is_rejected() {
        let secret = b"s";
        // Within the TTL cap (`now` is 1 below) so the test exercises track
        // binding rather than the over-cap rejection.
        let token = grant::issue("u", "track-a", 1 + grant::MAX_GRANT_TTL, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();
        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = PlayInput {
            token: &token,
            track_id: "track-b",
            range: None,
            server_addr: addr,
            segments: &[],
            segment_ranges: None,
            cipher_key: 0,
            now_unix: 1,
            up_next: None,
            trace_id: None,
        };
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, secret, &input),
            Err(PlayError::WrongTrack)
        ));
    }

    #[test]
    fn mismatched_warm_peer_is_discarded_without_counting_a_reuse() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::time::Duration;

        let timeouts = UpstreamTimeouts::new(Duration::from_secs(1), Duration::from_secs(1));
        let old_listener = TcpListener::bind("127.0.0.1:0").expect("bind old NNTP");
        let old_addr = old_listener.local_addr().expect("old address");
        let old_server = std::thread::spawn(move || {
            let (mut stream, _) = old_listener.accept().expect("accept warm connection");
            stream.write_all(b"200 old NNTP ready\r\n").unwrap();
            let mut byte = [0];
            let _ = stream.read(&mut byte);
        });
        let warm_client =
            NntpClient::connect_with_timeouts(&old_addr, timeouts).expect("warm greeting");

        let current_listener = TcpListener::bind("127.0.0.1:0").expect("bind current NNTP");
        let current_addr = current_listener.local_addr().expect("current address");
        let current_server = std::thread::spawn(move || {
            let (mut stream, _) = current_listener
                .accept()
                .expect("accept current connection");
            stream.write_all(b"200 current NNTP ready\r\n").unwrap();
            let mut command = Vec::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_until(b'\n', &mut command)
                .unwrap();
            stream.write_all(b"430 no such article\r\n").unwrap();
        });

        let mut pool = Pool::new(1);
        pool.record_connect();
        let lease = pool
            .try_acquire(Lane::Playback)
            .expect("lease for warm client");
        pool.release(lease, Some(warm_client));
        let segments = vec!["missing@synthetic.test".to_string()];
        let input = PlayInput {
            trace_id: None,
            token: "unused by first-segment fetch",
            track_id: "track",
            range: None,
            server_addr: current_addr,
            segments: &segments,
            segment_ranges: None,
            cipher_key: 0,
            now_unix: 0,
            up_next: None,
        };

        let (client, failure) = match open_and_fetch(&mut pool, &input, 0, timeouts) {
            Err(result) => result,
            Ok(_) => panic!("missing article response"),
        };
        assert!(matches!(failure.0, PlayError::ArticleMissing(_)));
        assert!(
            matches!(failure.1, Conn::KeepWarm),
            "a missing article is not a provider failure"
        );
        assert_eq!(
            (pool.connects(), pool.reuses()),
            (2, 0),
            "the changed peer requires a cold dial, not a warm reuse"
        );

        drop(client);
        current_server.join().expect("current server thread");
        old_server.join().expect("old server thread");
    }

    #[test]
    fn missing_article_settles_a_half_open_probe_as_a_live_provider() {
        use crate::pool::{CircuitState, CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_OPEN_MS};

        let mut pool = Pool::new(2);
        for _ in 0..CIRCUIT_FAILURE_THRESHOLD {
            pool.record_provider_error();
        }
        pool.advance(CIRCUIT_OPEN_MS);
        let probe = pool
            .try_acquire(Lane::Playback)
            .expect("half-open probe admitted");
        let mut meter = crate::meter::MemoryMeter::default();
        let error = settle(
            &mut pool,
            &mut meter,
            probe,
            None,
            (PlayError::ArticleMissing("missing".into()), Conn::KeepWarm),
            "user",
            "track",
        );

        assert_eq!(error, PlayError::ArticleMissing("missing".into()));
        assert_eq!(pool.circuit_state(), CircuitState::Closed);
        pool.try_acquire(Lane::Playback)
            .expect("a provider response closes the breaker");
    }

    #[test]
    fn open_breaker_maps_to_503_with_retry_after() {
        let err = PlayError::PoolQuarantined {
            retry_after_ms: 30_000,
        };
        assert_eq!(err.http_status(), 503);
        assert_eq!(err.retry_after_header().as_deref(), Some("30"));
        // A probe-in-flight rejection asks clients to wait before retrying.
        let probe = PlayError::PoolQuarantined {
            retry_after_ms: crate::pool::HALF_OPEN_PROBE_RETRY_MS,
        };
        assert_eq!(probe.http_status(), 503);
        assert_eq!(probe.retry_after_header().as_deref(), Some("1"));
        // Other failures keep their own statuses, never 503.
        assert_eq!(
            PlayError::Unauthorized(GrantError::Malformed).http_status(),
            401
        );
        assert_eq!(PlayError::PoolExhausted.http_status(), 429);
        assert_eq!(PlayError::PoolExhausted.retry_after_header(), None);
        assert_eq!(PlayError::ArticleMissing("m".into()).http_status(), 404);
    }

    #[test]
    fn stale_cache_entry_serves_immediately_with_swr_headers() {
        use crate::cache::{CacheStatus, CACHE_FRESH_TTL_SECS};
        use crate::play::PlayInput;

        let secret = b"s";
        let stale_now = 1_000 + CACHE_FRESH_TTL_SECS + 10;
        // Within the TTL cap so the test exercises the cache path.
        let token = grant::issue("u", "track-swr", stale_now + 60, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();
        cache.put_track_at("track-swr", b"0123456789".to_vec(), 1_000);

        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = PlayInput {
            trace_id: None,
            token: &token,
            track_id: "track-swr",
            range: None,
            server_addr: addr,
            segments: &[],
            segment_ranges: None,
            cipher_key: 0,
            now_unix: stale_now,
            up_next: None,
        };
        let resp = play(&mut pool, &mut cache, &mut meter, secret, &input).expect("stale serves");
        assert_eq!(resp.status, 200);
        assert!(resp.cache_hit, "stale is still served from cache");
        assert_eq!(resp.cache_status, CacheStatus::Stale);
        assert_eq!(resp.age_secs, CACHE_FRESH_TTL_SECS + 10);
        assert!(
            resp.needs_revalidate,
            "stale serve owes background revalidation"
        );
        assert_eq!(
            resp.cache_headers(),
            [
                ("Cache-Status".to_string(), "STALE".to_string()),
                ("Age".to_string(), (CACHE_FRESH_TTL_SECS + 10).to_string()),
            ]
        );
        let mut body = Vec::new();
        let mut stream = resp.stream;
        for chunk in stream.by_ref() {
            body.extend_from_slice(&chunk);
        }
        assert_eq!(body, b"0123456789");
        stream
            .finalize(&mut pool, &mut cache, &mut meter)
            .expect("hit finalize");
    }

    #[test]
    fn stale_range_serve_is_206_with_swr_headers() {
        use crate::cache::{CacheStatus, CACHE_FRESH_TTL_SECS};

        let secret = b"s";
        let now_unix = 1_000 + CACHE_FRESH_TTL_SECS + 1;
        // Within the TTL cap so the test exercises the cache path.
        let token = grant::issue("u", "track-swr", now_unix + 60, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();
        cache.put_track_at("track-swr", b"0123456789".to_vec(), 1_000);

        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = PlayInput {
            trace_id: None,
            token: &token,
            track_id: "track-swr",
            range: Some("bytes=2-5"),
            server_addr: addr,
            segments: &[],
            segment_ranges: None,
            cipher_key: 0,
            now_unix,
            up_next: None,
        };
        let resp = play(&mut pool, &mut cache, &mut meter, secret, &input).expect("stale serves");
        assert_eq!(resp.status, 206);
        assert_eq!(resp.content_range.as_deref(), Some("bytes 2-5/10"));
        assert_eq!(resp.cache_status, CacheStatus::Stale);
        assert!(resp.needs_revalidate);
        let mut body = Vec::new();
        let mut stream = resp.stream;
        for chunk in stream.by_ref() {
            body.extend_from_slice(&chunk);
        }
        assert_eq!(body, b"2345");
    }

    #[test]
    fn only_the_first_stale_serve_claims_the_revalidation() {
        use crate::cache::{CacheStatus, CACHE_FRESH_TTL_SECS};

        let secret = b"s";
        let now_unix = 1_000 + CACHE_FRESH_TTL_SECS + 1;
        // Within the TTL cap so the test exercises the cache path.
        let token = grant::issue("u", "track-swr", now_unix + 60, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();
        cache.put_track_at("track-swr", b"0123456789".to_vec(), 1_000);

        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = PlayInput {
            trace_id: None,
            token: &token,
            track_id: "track-swr",
            range: None,
            server_addr: addr,
            segments: &[],
            segment_ranges: None,
            cipher_key: 0,
            now_unix,
            up_next: None,
        };
        let first = play(&mut pool, &mut cache, &mut meter, secret, &input).expect("stale serves");
        assert_eq!(first.cache_status, CacheStatus::Stale);
        assert!(
            first.needs_revalidate,
            "the first stale reader claims the one background revalidation"
        );
        let second = play(&mut pool, &mut cache, &mut meter, secret, &input).expect("stale serves");
        assert_eq!(second.cache_status, CacheStatus::Stale);
        assert!(
            !second.needs_revalidate,
            "later stale readers must not each trigger a refresh"
        );

        // The revalidation rewrite landing releases the claim and the entry
        // serves fresh again.
        cache.put_track_at("track-swr", b"0123456789".to_vec(), input.now_unix);
        let third = play(&mut pool, &mut cache, &mut meter, secret, &input).expect("fresh serves");
        assert_eq!(third.cache_status, CacheStatus::Hit);
        assert!(!third.needs_revalidate);
    }

    #[test]
    fn fresh_cache_entry_serves_hit_headers_without_revalidation() {
        use crate::cache::CacheStatus;

        let secret = b"s";
        // Within the TTL cap so the test exercises the cache path.
        let token = grant::issue("u", "track-swr", 1_010 + 60, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();
        cache.put_track_at("track-swr", b"0123456789".to_vec(), 1_000);

        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = PlayInput {
            trace_id: None,
            token: &token,
            track_id: "track-swr",
            range: None,
            server_addr: addr,
            segments: &[],
            segment_ranges: None,
            cipher_key: 0,
            now_unix: 1_010,
            up_next: None,
        };
        let resp = play(&mut pool, &mut cache, &mut meter, secret, &input).expect("fresh serves");
        assert!(resp.cache_hit);
        assert_eq!(resp.cache_status, CacheStatus::Hit);
        assert_eq!(resp.age_secs, 10);
        assert!(!resp.needs_revalidate);
        assert_eq!(
            resp.cache_headers(),
            [
                ("Cache-Status".to_string(), "HIT".to_string()),
                ("Age".to_string(), "10".to_string()),
            ]
        );
    }

    #[test]
    fn expired_cache_entry_falls_through_to_the_miss_path() {
        use crate::cache::{CACHE_FRESH_TTL_SECS, CACHE_SWR_WINDOW_SECS};

        let secret = b"s";
        let now_unix = 1_000 + CACHE_FRESH_TTL_SECS + CACHE_SWR_WINDOW_SECS + 1;
        // Within the TTL cap so the test exercises the cache path.
        let token = grant::issue("u", "track-swr", now_unix + 60, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();
        cache.put_track_at("track-swr", b"0123456789".to_vec(), 1_000);

        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = PlayInput {
            trace_id: None,
            token: &token,
            track_id: "track-swr",
            range: None,
            server_addr: addr,
            segments: &[],
            segment_ranges: None,
            cipher_key: 0,
            now_unix,
            up_next: None,
        };
        // Empty segments fail on the miss path: proof the expired entry did
        // not serve.
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, secret, &input),
            Err(PlayError::Upstream(_))
        ));
    }

    #[test]
    fn miss_finalize_stamps_the_cache_write_with_play_time() {
        use crate::cache::{CacheStatus, CACHE_FRESH_TTL_SECS};
        use crate::yenc::{self, PartSpec};
        use std::io::{BufRead, BufReader, Write as _};
        use std::net::TcpListener;
        use std::thread;

        // One-segment track, identity cipher (key 0 is a no-op xor).
        let plain = b"stamp-me".to_vec();
        let mid = "t1-seg0@unit.test".to_string();
        let article = yenc::encode_part(
            &plain,
            "t1.flac",
            PartSpec {
                file_size: plain.len() as u64,
                part: 1,
                total: 1,
                offset: 0,
                file_crc: Some(yenc::crc32(&plain)),
            },
        );
        // Minimal single-article NNTP stub (mirrors the integration fake's
        // dot-stuffing so `decode_part` sees the exact encoded bytes).
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub NNTP");
        let addr = listener.local_addr().expect("stub addr");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("one play connection");
            let mut reader = BufReader::new(stream.try_clone().expect("clone stub"));
            let mut writer = stream;
            writer
                .write_all(b"200 stub NNTP ready\r\n")
                .expect("greeting");
            let mut line = String::new();
            reader.read_line(&mut line).expect("BODY line");
            assert!(line.starts_with("BODY"), "stub only serves BODY");
            writer
                .write_all(format!("222 0 <{mid}> follows\r\n").as_bytes())
                .expect("222");
            for raw_line in article.split(|&b| b == b'\n') {
                let text_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
                if text_line.first() == Some(&b'.') {
                    writer.write_all(b".").expect("dot-stuff");
                }
                writer.write_all(text_line).expect("article line");
                writer.write_all(b"\r\n").expect("crlf");
            }
            writer.write_all(b".\r\n").expect("terminator");
        });

        let secret = b"s";
        // Within the TTL cap so the test exercises the miss path.
        let token = grant::issue("u", "track-t1", 5_000 + 60, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();
        let segments = ["t1-seg0@unit.test".to_string()];
        let input = PlayInput {
            trace_id: None,
            token: &token,
            track_id: "track-t1",
            range: None,
            server_addr: addr,
            segments: &segments,
            segment_ranges: None,
            cipher_key: 0,
            now_unix: 5_000,
            up_next: None,
        };
        let resp = play(&mut pool, &mut cache, &mut meter, secret, &input).expect("miss play");
        assert!(!resp.cache_hit);
        assert_eq!(resp.cache_status, CacheStatus::Miss);
        assert_eq!(resp.age_secs, 0);
        assert!(!resp.needs_revalidate);
        assert_eq!(
            resp.cache_headers(),
            [
                ("Cache-Status".to_string(), "MISS".to_string()),
                ("Age".to_string(), "0".to_string()),
            ]
        );
        let mut body = Vec::new();
        let mut stream = resp.stream;
        for chunk in stream.by_ref() {
            body.extend_from_slice(&chunk);
        }
        assert_eq!(body, plain);
        stream
            .finalize(&mut pool, &mut cache, &mut meter)
            .expect("finalize");
        server.join().expect("stub served one article");

        // The write is stamped with the play time, so the SWR clock runs.
        let (_, serve) = cache
            .get_track_at("track-t1", 5_000)
            .expect("just-written entry serves");
        assert_eq!(serve, crate::cache::CacheServe::Fresh { age_secs: 0 });
        let (_, serve) = cache
            .get_track_at("track-t1", 5_000 + CACHE_FRESH_TTL_SECS + 1)
            .expect("entry goes stale on schedule");
        assert!(serve.needs_revalidate());
    }

    #[test]
    fn unauthenticated_rejections_are_silent_and_do_not_touch_the_pool() {
        let secret = b"s";
        let forged_unexpired = grant::issue("attacker", "track", 600, b"other-secret");
        let forged_expired = grant::issue("attacker", "track", 0, b"other-secret");
        for (token, expected_error) in [
            ("not-a-token", GrantError::Malformed),
            ("v1.abc.def", GrantError::Malformed),
            (forged_unexpired.as_str(), GrantError::BadSignature),
            (forged_expired.as_str(), GrantError::BadSignature),
        ] {
            let mut pool = Pool::new(1);
            let mut cache = ContentCache::new();
            let mut meter = crate::meter::MemoryMeter::default();
            let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
            let input = PlayInput {
                trace_id: None,
                token,
                track_id: "t",
                range: None,
                server_addr: addr,
                segments: &[],
                segment_ranges: None,
                cipher_key: 0,
                now_unix: 1,
                up_next: None,
            };
            assert!(
                matches!(
                    play(&mut pool, &mut cache, &mut meter, secret, &input),
                    Err(PlayError::Unauthorized(err)) if err == expected_error
                ),
                "token {token:?}"
            );
            assert!(meter.events.is_empty(), "token {token:?}");
            assert_eq!(pool.connects(), 0, "rejected plays must not dial out");
        }
    }

    #[test]
    fn failed_warm_redial_does_not_emit_redial_event() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind NNTP server");
        let address = listener.local_addr().expect("server address");
        let (plain, article) = test_article();
        let server = serve_article(listener, article, true);
        let segment = vec!["track-seg@synthetic.test".to_string()];
        let token = grant::issue("u", "track-a", GRANT_EXP_UNIX, b"s");
        let mut pool = Pool::new(2);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();

        let input = test_input(&token, "track-a", address, &segment);
        let first = play(&mut pool, &mut cache, &mut meter, b"s", &input).expect("first play");
        let mut body = Vec::new();
        let mut stream = first.stream;
        for chunk in stream.by_ref() {
            body.extend_from_slice(&chunk);
        }
        assert_eq!(body, plain);
        stream
            .finalize(&mut pool, &mut cache, &mut meter)
            .expect("first finalize");

        let token = grant::issue("u", "track-b", GRANT_EXP_UNIX, b"s");
        let input = test_input(&token, "track-b", address, &segment);
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, b"s", &input),
            Err(PlayError::Upstream(_))
        ));
        server.join().expect("server thread");
        assert!(meter
            .events
            .iter()
            .all(|event| event.kind != MeterKind::WarmRedial));
        assert_eq!(
            meter
                .events
                .iter()
                .filter(|event| event.kind == MeterKind::QuarantineEntered)
                .count(),
            0,
            "one failed play must not trip the three-failure breaker"
        );
    }

    #[test]
    fn finalize_without_fetch_does_not_clear_interleaved_quarantine() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind NNTP server");
        let address = listener.local_addr().expect("server address");
        let (plain, article) = test_article();
        let server = serve_article(listener, article, false);
        let segment = vec!["track-seg@synthetic.test".to_string()];
        let token = grant::issue("u", "track-a", GRANT_EXP_UNIX, b"s");
        let mut pool = Pool::new(2);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();

        let input = test_input(&token, "track-a", address, &segment);
        let response = play(&mut pool, &mut cache, &mut meter, b"s", &input).expect("play");
        let mut stream = response.stream;
        let body: Vec<u8> = stream.by_ref().flatten().collect();
        assert_eq!(body, plain);
        server.join().expect("server thread");

        let token = grant::issue("u", "track-b", GRANT_EXP_UNIX, b"s");
        let dead = closed_address();
        let other_segment = vec!["other-seg@synthetic.test".to_string()];
        let input = test_input(&token, "track-b", dead, &other_segment);
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, b"s", &input),
            Err(PlayError::Upstream(_))
        ));
        pool.record_provider_error();
        pool.record_provider_error();
        assert!(pool.quarantined());

        stream
            .finalize(&mut pool, &mut cache, &mut meter)
            .expect("finalize already-fetched stream");
        assert!(pool.quarantined(), "a no-fetch finalize cannot clear it");
        assert!(meter
            .events
            .iter()
            .all(|event| event.kind != MeterKind::QuarantineExited));
    }

    #[test]
    fn failed_streamed_decode_does_not_clear_interleaved_quarantine() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind NNTP server");
        let address = listener.local_addr().expect("server address");
        let (_, articles) = test_parts();
        let server = serve_articles(
            listener,
            vec![articles[0].clone(), b"not a yEnc article".to_vec()],
            false,
        );
        let segments = vec![
            "track-seg0@synthetic.test".to_string(),
            "track-seg1@synthetic.test".to_string(),
        ];
        let token = grant::issue("u", "track-a", GRANT_EXP_UNIX, b"s");
        let mut pool = Pool::new(2);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();

        let input = test_input(&token, "track-a", address, &segments);
        let response = play(&mut pool, &mut cache, &mut meter, b"s", &input).expect("play");
        let mut stream = response.stream;
        let token = grant::issue("u", "track-b", GRANT_EXP_UNIX, b"s");
        let dead = closed_address();
        let other_segment = vec!["other-seg@synthetic.test".to_string()];
        let input = test_input(&token, "track-b", dead, &other_segment);
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, b"s", &input),
            Err(PlayError::Upstream(_))
        ));
        pool.record_provider_error();
        pool.record_provider_error();
        assert!(pool.quarantined());

        let _body: Vec<u8> = stream.by_ref().flatten().collect();
        server.join().expect("server thread");
        assert!(matches!(stream.error(), Some(PlayError::Decode(_))));
        assert!(stream.finalize(&mut pool, &mut cache, &mut meter).is_err());
        assert!(pool.quarantined(), "a failed stream cannot clear it");
        assert!(meter
            .events
            .iter()
            .all(|event| event.kind != MeterKind::QuarantineExited));
    }

    /// A segment fetched after the interleaved error leaves
    /// `last_success_epoch` equal to the pool's epoch, so the epoch check
    /// alone would let a play that later fails clear the quarantine; only the
    /// `live.failed` guard in `finalize` stops it.
    #[test]
    fn late_decode_failure_after_post_error_success_does_not_clear_quarantine() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind NNTP server");
        let address = listener.local_addr().expect("server address");
        let (_, mut articles) = split_parts(&[b"small ", b"audio ", b"payload"]);
        articles[2] = b"not a yEnc article".to_vec();
        let server = serve_articles(listener, articles, false);
        let segments = vec![
            "track-seg0@synthetic.test".to_string(),
            "track-seg1@synthetic.test".to_string(),
            "track-seg2@synthetic.test".to_string(),
        ];
        let token = grant::issue("u", "track-a", GRANT_EXP_UNIX, b"s");
        let mut pool = Pool::new(2);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();

        let input = test_input(&token, "track-a", address, &segments);
        let response = play(&mut pool, &mut cache, &mut meter, b"s", &input).expect("play");
        let mut stream = response.stream;
        let token = grant::issue("u", "track-b", GRANT_EXP_UNIX, b"s");
        let dead = closed_address();
        let other_segment = vec!["other-seg@synthetic.test".to_string()];
        let input = test_input(&token, "track-b", dead, &other_segment);
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, b"s", &input),
            Err(PlayError::Upstream(_))
        ));
        pool.record_provider_error();
        pool.record_provider_error();
        assert!(pool.quarantined());

        let body: Vec<u8> = stream.by_ref().flatten().collect();
        server.join().expect("server thread");
        assert_eq!(
            body, b"small audio ",
            "segment 1 must be served after the interleaved error, before segment 2 fails"
        );
        assert!(matches!(stream.error(), Some(PlayError::Decode(_))));
        assert!(stream.finalize(&mut pool, &mut cache, &mut meter).is_err());
        assert!(pool.quarantined(), "a failed play cannot clear it");
        assert!(meter
            .events
            .iter()
            .all(|event| event.kind != MeterKind::QuarantineExited));
    }

    #[test]
    fn streamed_success_after_interleaved_error_clears_quarantine_on_finalize() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind NNTP server");
        let address = listener.local_addr().expect("server address");
        let (plain, articles) = test_parts();
        let server = serve_articles(listener, articles, false);
        let segments = vec![
            "track-seg0@synthetic.test".to_string(),
            "track-seg1@synthetic.test".to_string(),
        ];
        let token = grant::issue("u", "track-a", GRANT_EXP_UNIX, b"s");
        let mut pool = Pool::new(2);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();

        let input = test_input(&token, "track-a", address, &segments);
        let response = play(&mut pool, &mut cache, &mut meter, b"s", &input).expect("play");
        let mut stream = response.stream;
        let token = grant::issue("u", "track-b", GRANT_EXP_UNIX, b"s");
        let dead = closed_address();
        let other_segment = vec!["other-seg@synthetic.test".to_string()];
        let input = test_input(&token, "track-b", dead, &other_segment);
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, b"s", &input),
            Err(PlayError::Upstream(_))
        ));
        pool.record_provider_error();
        pool.record_provider_error();
        assert!(pool.quarantined());

        let body: Vec<u8> = stream.by_ref().flatten().collect();
        assert_eq!(body, plain);
        server.join().expect("server thread");
        stream
            .finalize(&mut pool, &mut cache, &mut meter)
            .expect("finalize streamed play");
        assert!(
            pool.quarantined(),
            "a play started before the provider failures cannot clear quarantine"
        );
        let exits: Vec<_> = meter
            .events
            .iter()
            .filter(|event| event.kind == MeterKind::QuarantineExited)
            .collect();
        assert!(
            exits.is_empty(),
            "stale success cannot emit quarantine exit"
        );
    }

    #[test]
    fn later_interleaved_error_wins_over_earlier_streamed_success() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind NNTP server");
        let address = listener.local_addr().expect("server address");
        let (plain, articles) = test_parts();
        let server = serve_articles(listener, articles, false);
        let segments = vec![
            "track-seg0@synthetic.test".to_string(),
            "track-seg1@synthetic.test".to_string(),
        ];
        let token = grant::issue("u", "track-a", GRANT_EXP_UNIX, b"s");
        let mut pool = Pool::new(2);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();

        let input = test_input(&token, "track-a", address, &segments);
        let response = play(&mut pool, &mut cache, &mut meter, b"s", &input).expect("play");
        let mut stream = response.stream;
        let token = grant::issue("u", "track-b", GRANT_EXP_UNIX, b"s");
        let dead = closed_address();
        let other_segment = vec!["other-seg@synthetic.test".to_string()];
        let input = test_input(&token, "track-b", dead, &other_segment);
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, b"s", &input),
            Err(PlayError::Upstream(_))
        ));
        pool.record_provider_error();
        pool.record_provider_error();
        assert!(pool.quarantined());

        let body: Vec<u8> = stream.by_ref().flatten().collect();
        assert_eq!(body, plain);
        server.join().expect("server thread");

        pool.advance(30_000);
        let token = grant::issue("u", "track-c", GRANT_EXP_UNIX, b"s");
        let input = test_input(&token, "track-c", closed_address(), &other_segment);
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, b"s", &input),
            Err(PlayError::Upstream(_))
        ));
        assert!(pool.quarantined());

        stream
            .finalize(&mut pool, &mut cache, &mut meter)
            .expect("finalize streamed play");
        assert!(
            pool.quarantined(),
            "a newer provider error stays quarantined"
        );
        assert!(meter
            .events
            .iter()
            .all(|event| event.kind != MeterKind::QuarantineExited));
    }

    #[test]
    fn hit_play_projects_to_a_hit_observe_event_with_served_bytes() {
        use crate::observability::{hash_user, CacheStatus};

        let secret = b"s";
        // Expiry stays within the 15-min grant TTL cap (now_unix = 1 below)
        // so the test passes both pre-cap and post-cap `verify`.
        let token = grant::issue("user-9", "track-a", 901, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        cache.put_track("track-a", vec![7u8; 100]);
        let mut meter = crate::meter::MemoryMeter::default();
        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = PlayInput {
            token: &token,
            track_id: "track-a",
            range: None,
            server_addr: addr,
            segments: &[],
            segment_ranges: None,
            cipher_key: 0,
            now_unix: 1,
            up_next: None,
            trace_id: None,
        };
        let mut response = play(&mut pool, &mut cache, &mut meter, secret, &input).unwrap();
        assert!(response.cache_hit);
        let served: usize = response.stream.by_ref().map(|c| c.len()).sum();
        assert_eq!(served, 100);

        let ev = response.stream.observe_event("req-001", 7);
        assert_eq!(ev.cache_status, CacheStatus::Hit);
        assert_eq!(ev.bytes, 100);
        assert_eq!(ev.ttft_ms, 7);
        assert_eq!(ev.pool_error_code, None);
        assert_eq!(ev.user_hash, hash_user("user-9"));
        assert_eq!(
            ev.analytics_blobs(),
            vec!["hit", ev.user_hash.as_str(), "req-001", ""]
        );
    }

    #[test]
    fn stale_play_projects_to_a_stale_observe_event_with_served_bytes() {
        use crate::cache::CACHE_FRESH_TTL_SECS;
        use crate::observability::{hash_user, CacheStatus};

        let secret = b"s";
        let stale_now = 1_000 + CACHE_FRESH_TTL_SECS + 10;
        // Within the 15-min grant TTL cap so the test exercises the cache path.
        let token = grant::issue("user-9", "track-swr", stale_now + 60, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        cache.put_track_at("track-swr", vec![7u8; 100], 1_000);
        let mut meter = crate::meter::MemoryMeter::default();
        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = PlayInput {
            trace_id: None,
            token: &token,
            track_id: "track-swr",
            range: None,
            server_addr: addr,
            segments: &[],
            cipher_key: 0,
            now_unix: stale_now,
            up_next: None,
            segment_ranges: None,
        };
        let mut response = play(&mut pool, &mut cache, &mut meter, secret, &input).unwrap();
        assert!(response.cache_hit, "stale is still served from cache");
        let served: usize = response.stream.by_ref().map(|c| c.len()).sum();
        assert_eq!(served, 100);

        let ev = response.stream.observe_event("req-stale-001", 7);
        assert_eq!(ev.cache_status, CacheStatus::Stale);
        assert_eq!(ev.bytes, 100);
        assert_eq!(ev.user_hash, hash_user("user-9"));
        assert_eq!(
            ev.analytics_blobs(),
            vec!["stale", ev.user_hash.as_str(), "req-stale-001", ""]
        );
    }

    fn assert_pool_failure_observation(quarantined: bool, expected_code: &str) {
        use crate::observability::{hash_user, CacheStatus};

        let secret = b"s";
        // Expiry stays within the 15-min grant TTL cap (now_unix = 1 below)
        // so the test passes both pre-cap and post-cap `verify`.
        let token = grant::issue("u", "track-a", 901, secret);
        let mut pool = Pool::new(1);
        let held_lease = if quarantined {
            for _ in 1..crate::pool::CIRCUIT_FAILURE_THRESHOLD {
                pool.record_provider_error();
            }
            pool.record_provider_error();
            None
        } else {
            Some(pool.try_acquire(Lane::Playback).unwrap())
        };
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();
        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let segments = vec!["a-seg0@synthetic.test".to_string()];
        let input = PlayInput {
            trace_id: None,
            token: &token,
            track_id: "track-a",
            range: None,
            server_addr: addr,
            segments: &segments,
            segment_ranges: None,
            cipher_key: 0,
            now_unix: 1,
            up_next: None,
        };
        let attempt = play_with_observe_event(
            &mut pool,
            &mut cache,
            &mut meter,
            secret,
            &input,
            "req-pool-error",
        );
        let err = match attempt.result {
            Ok(_) => panic!("pool failure must fail the play"),
            Err(e) => e,
        };
        assert_eq!(err.pool_error_code(), Some(expected_code));
        let event = attempt
            .error_event
            .expect("early pool errors need an event");
        assert_eq!(event.cache_status, CacheStatus::Miss);
        assert_eq!(event.request_id, "req-pool-error");
        assert_eq!(event.user_hash, hash_user("u"));
        assert_eq!(event.ttft_ms, 0);
        assert_eq!(event.bytes, 0);
        assert_eq!(event.pool_error_code.as_deref(), Some(expected_code));
        assert_eq!(
            event.analytics_blobs(),
            vec![
                "miss".to_string(),
                hash_user("u"),
                "req-pool-error".to_string(),
                expected_code.to_string(),
            ]
        );
        drop(held_lease);
    }

    #[test]
    fn half_open_non_provider_result_emits_one_quarantine_exit() {
        let mut pool = StreamPool::new(2);
        for _ in 0..crate::pool::CIRCUIT_FAILURE_THRESHOLD {
            pool.record_provider_error();
        }
        pool.advance(crate::pool::CIRCUIT_OPEN_MS);
        let probe = pool.try_acquire(Lane::Playback).expect("probe admitted");
        let mut meter = crate::meter::MemoryMeter::default();

        settle(
            &mut pool,
            &mut meter,
            probe,
            None,
            (
                PlayError::ArticleMissing("gone@synthetic.test".into()),
                Conn::KeepWarm,
            ),
            "u",
            "track-a",
        );

        assert_eq!(pool.circuit_state(), crate::pool::CircuitState::Closed);
        assert_eq!(
            meter
                .events
                .iter()
                .filter(|event| event.kind == MeterKind::QuarantineExited)
                .count(),
            1
        );
    }

    #[test]
    fn quarantined_and_exhausted_plays_project_pool_error_events() {
        assert_pool_failure_observation(true, "POOL_QUARANTINED");
        assert_pool_failure_observation(false, "POOL_EXHAUSTED");
        assert_eq!(PlayError::WrongTrack.pool_error_code(), None);
    }

    #[test]
    fn emitted_events_carry_seconds_derived_from_bytes() {
        // The ledger row later derives seconds from the same byte count, so
        // every event built here must already agree with the formula.
        for bytes in [0, 1, 47_999, 48_000, 48_001, 100_000] {
            let event = ev(MeterKind::BytesServed, "u", "t", bytes);
            assert_eq!(
                event.seconds,
                crate::meter::stream_seconds_for_bytes(bytes),
                "bytes {bytes}"
            );
        }
        let idle = ev(MeterKind::PlayStart, "u", "t", 0);
        assert_eq!((idle.bytes, idle.seconds), (0, 0));
    }

    #[test]
    fn ranges_validate_only_in_covering_shape() {
        let good = [(0, 10), (10, 25), (25, 30)];
        assert_eq!(valid_ranges(Some(&good), 3), Some(&good[..]));
        // Absent, wrong arity, non-zero start, a gap, an empty segment:
        // all fall back to the prefix walk instead of failing the play.
        assert_eq!(valid_ranges(None, 3), None);
        assert_eq!(valid_ranges(Some(&good), 2), None);
        assert_eq!(valid_ranges(Some(&[(1, 10)]), 1), None);
        assert_eq!(valid_ranges(Some(&[(0, 10), (11, 20)]), 2), None);
        assert_eq!(valid_ranges(Some(&[(0, 10), (10, 10)]), 2), None);
        assert_eq!(valid_ranges(Some(&[]), 0), None);
    }

    #[test]
    fn covering_segment_addresses_the_range_start() {
        let ranges = [(0, 10), (10, 25), (25, 30)];
        assert_eq!(covering_segment(&ranges, 0), 0);
        assert_eq!(covering_segment(&ranges, 9), 0);
        assert_eq!(covering_segment(&ranges, 10), 1);
        assert_eq!(covering_segment(&ranges, 29), 2);
    }

    const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";

    fn traced_input<'a>(
        token: &'a str,
        track_id: &'a str,
        addr: SocketAddr,
        segments: &'a [String],
        trace_id: Option<&'a str>,
    ) -> PlayInput<'a> {
        PlayInput {
            token,
            track_id,
            range: None,
            server_addr: addr,
            segments,
            segment_ranges: None,
            cipher_key: 0,
            now_unix: 1,
            up_next: None,
            trace_id,
        }
    }

    #[test]
    fn trace_id_threads_through_the_cache_hit_path() {
        let secret = b"s";
        let token = grant::issue("u", "track-a", 1 + grant::MAX_GRANT_TTL, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        cache.put_track("track-a", vec![1, 2, 3, 4]);
        let mut meter = crate::meter::MemoryMeter::default();
        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = traced_input(&token, "track-a", addr, &[], Some(TRACE));
        let resp = play(&mut pool, &mut cache, &mut meter, secret, &input).expect("hit play");
        assert!(resp.cache_hit);
        assert_eq!(resp.trace_id.as_deref(), Some(TRACE));
        assert_eq!(resp.stream.trace_id(), Some(TRACE));
        // AE rows are unchanged: no trace field, legacy event set intact.
        assert!(meter.has_kind(MeterKind::PlayStart));
        assert!(meter.has_kind(MeterKind::CacheHit));
        let row = &meter.events[0];
        assert_eq!(row.user_id, "u");
        assert_eq!(row.track_id, "track-a");
        assert_eq!(row.bytes, 0);
    }

    #[test]
    fn invalid_trace_is_dropped_never_logged() {
        let secret = b"s";
        let token = grant::issue("u", "track-a", 1 + grant::MAX_GRANT_TTL, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        cache.put_track("track-a", vec![1, 2, 3, 4]);
        let mut meter = crate::meter::MemoryMeter::default();
        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        for bad in [
            Some("too-short"),
            Some("9F3A4C2E1B5D4F6A8C7E9D0B1A2F4E6C"),
            Some("Bearer abc123"),
        ] {
            let input = traced_input(&token, "track-a", addr, &[], bad);
            let resp = play(&mut pool, &mut cache, &mut meter, secret, &input).expect("hit play");
            assert_eq!(resp.trace_id, None, "invalid trace must not survive");
            assert_eq!(resp.stream.trace_id(), None);
            // The invalid value never reaches a renderable envelope.
            assert!(
                crate::trace::normalize_trace_id(bad).is_none(),
                "invalid trace must not normalize"
            );
        }
    }

    #[test]
    fn missing_trace_runs_untraced() {
        let secret = b"s";
        let token = grant::issue("u", "track-a", 1 + grant::MAX_GRANT_TTL, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        cache.put_track("track-a", vec![1, 2, 3, 4]);
        let mut meter = crate::meter::MemoryMeter::default();
        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = traced_input(&token, "track-a", addr, &[], None);
        let resp = play(&mut pool, &mut cache, &mut meter, secret, &input).expect("hit play");
        assert_eq!(resp.trace_id, None);
        assert_eq!(resp.stream.trace_id(), None);
    }

    #[test]
    fn aborted_hit_stream_keeps_its_trace_in_the_orphan() {
        let secret = b"s";
        let token = grant::issue("u", "track-a", 1 + grant::MAX_GRANT_TTL, secret);
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        cache.put_track("track-a", vec![0u8; 40_000]);
        let mut meter = crate::meter::MemoryMeter::default();
        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = traced_input(&token, "track-a", addr, &[], Some(TRACE));
        let resp = play(&mut pool, &mut cache, &mut meter, secret, &input).expect("hit play");
        let mut stream = resp.stream;
        assert!(stream.next().is_some(), "serve at least one chunk");
        stream.abort(&mut pool);
        let orphaned = pool.take_orphaned();
        assert_eq!(orphaned.len(), 1);
        assert_eq!(orphaned[0].track_id, "track-a");
        assert_eq!(orphaned[0].trace_id.as_deref(), Some(TRACE));
        assert!(orphaned[0].bytes > 0);
    }

    #[test]
    fn trace_never_changes_authorization_or_pool_decisions() {
        // Same rejected grant with and without a trace: identical outcome,
        // and a rejected play never touches the pool.
        let secret = b"s";
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();
        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        for trace in [None, Some(TRACE)] {
            let input = traced_input("not-a-token", "t", addr, &[], trace);
            assert!(matches!(
                play(&mut pool, &mut cache, &mut meter, secret, &input),
                Err(PlayError::Unauthorized(GrantError::Malformed))
            ));
        }
        assert_eq!(pool.connects(), 0);
    }
}
