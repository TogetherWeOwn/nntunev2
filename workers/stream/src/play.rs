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
//! Failure handling: only connection-level trouble (I/O, protocol errors)
//! quarantines the pool. A missing article or a bad segment fails that play
//! alone — the connection is fine and goes back to the pool warm.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use crate::cache::{ContentCache, MultipartUpload};
use crate::grant::{self, GrantError};
use crate::meter::{Meter, MeterEvent, MeterKind};
use crate::nntp::{valid_message_id, NntpClient, NntpError};
use crate::pool::{Lane, Lease, OrphanedPlay, Pool, PoolError};
use crate::prefetch::should_prefetch;
use crate::range::{self, RangeOutcome};
use crate::yenc::{self, Decoded, YencError};

/// Client chunk size for served streams.
const CHUNK: usize = 16 * 1024;

/// Why a play failed.
#[derive(Debug, PartialEq, Eq)]
pub enum PlayError {
    Unauthorized(GrantError),
    WrongTrack,
    RangeUnsatisfiable,
    PoolExhausted,
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

/// Everything the play path needs, by reference.
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
    pub stream: PlayStream,
}

/// A failure plus whether it indicts the provider connection.
type Failure = (PlayError, bool);

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
    /// Next absolute byte to emit, and the exclusive end of the response.
    pos: u64,
    end: u64,
    served: u64,
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
        // and `Cached` alike.
        if self.source.take().is_some() && self.served > 0 {
            if let Ok(mut orphaned) = self.orphans.lock() {
                orphaned.push(OrphanedPlay {
                    user_id: self.user_id.clone(),
                    track_id: self.track_id.clone(),
                    bytes: self.served,
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
    /// Validated per-segment ranges, when the covering fetch is in play.
    ranges: Option<Vec<(u64, u64)>>,
    /// Next segment to fetch for the *client* (serving cursor). Starts at
    /// the covering segment; `finalize` separately fetches whatever
    /// `seg_done` still lacks (e.g. a skipped prefix).
    next_seg: usize,
    seg_done: Vec<bool>,
    cipher_key: u8,
    /// File size declared by the first fetched segment.
    total: u64,
    /// Bytes decoded so far, in fetch order (placement check on the
    /// prefix-walk path, where fetch order is file order).
    decoded: u64,
    /// The most recently decoded segment, shared with `pending` (no copy).
    buf: Arc<[u8]>,
    buf_start: u64,
    /// Decoded segments not yet staged: `upload` only ever receives the
    /// contiguous prefix (`staged_seg..`), so a covering segment fetched
    /// ahead of its prefix waits here until `finalize` fills the gap.
    pending: BTreeMap<usize, Arc<[u8]>>,
    staged_seg: usize,
    staged_bytes: u64,
    upload: MultipartUpload,
    failed: Option<Failure>,
}

/// Serve `track_id` to its granted owner.
pub fn play(
    pool: &mut StreamPool,
    cache: &mut ContentCache,
    meter: &mut dyn Meter,
    secret: &[u8],
    input: &PlayInput<'_>,
) -> Result<PlayResponse, PlayError> {
    let grant =
        grant::verify(input.token, secret, input.now_unix).map_err(PlayError::Unauthorized)?;
    if grant.track_id != input.track_id {
        return Err(PlayError::WrongTrack);
    }
    let user = grant.user_id.clone();
    flush_orphaned(pool, meter);
    meter.emit(ev(MeterKind::PlayStart, &user, input.track_id, 0));

    if let Some(full) = cache.get_track(input.track_id) {
        meter.emit(ev(MeterKind::CacheHit, &user, input.track_id, 0));
        let window = resolve_range(full.len() as u64, input.range)?;
        return Ok(respond(
            window,
            true,
            &user,
            input.track_id,
            Source::Cached(full),
            pool.orphan_sink(),
        ));
    }

    meter.emit(ev(MeterKind::CacheMiss, &user, input.track_id, 0));
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

    let (mut client, mut first) = match open_and_fetch(pool, input, first_seg) {
        Ok(pair) => pair,
        Err((client, failure)) => {
            return Err(settle(
                pool,
                meter,
                lease,
                client,
                failure,
                &user,
                input.track_id,
            ))
        }
    };
    if let Some(r) = ranges {
        let agrees = first.file_size == r[r.len() - 1].1
            && first.offset == r[first_seg].0
            && first.data.len() as u64 == r[first_seg].1 - r[first_seg].0;
        if !agrees {
            ranges = None;
            if first_seg != 0 {
                // The hint lied about the covering article: discard it and
                // restart as a prefix walk on the same connection.
                match fetch_segment(&mut client, &input.segments[0]) {
                    Ok(decoded) => {
                        first = decoded;
                        first_seg = 0;
                    }
                    Err(SegmentError::Nntp(e)) => {
                        let failure = nntp_failure(e, &input.segments[0]);
                        let keep = if failure.1 { None } else { Some(client) };
                        return Err(settle(
                            pool,
                            meter,
                            lease,
                            keep,
                            failure,
                            &user,
                            input.track_id,
                        ));
                    }
                    Err(SegmentError::Decode(e)) => {
                        return Err(settle(
                            pool,
                            meter,
                            lease,
                            Some(client),
                            (PlayError::Decode(e), false),
                            &user,
                            input.track_id,
                        ));
                    }
                }
            }
        }
    }
    pool.record_success();
    let mut live = Live {
        client,
        lease,
        segments: input.segments.to_vec(),
        ranges: ranges.map(<[(u64, u64)]>::to_vec),
        next_seg: first_seg,
        seg_done: vec![false; input.segments.len()],
        cipher_key: input.cipher_key,
        total: first.file_size,
        decoded: 0,
        buf: Arc::from([]),
        buf_start: 0,
        pending: BTreeMap::new(),
        staged_seg: 0,
        staged_bytes: 0,
        upload: MultipartUpload::begin(input.track_id),
        failed: None,
    };
    if let Err(failure) = live.ingest(first_seg, first) {
        return Err(live.settle(pool, meter, Some(failure), &user, input.track_id));
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
            meter.emit(ev(MeterKind::PrefetchEnqueued, &user, next, 0));
        }
    }
    Ok(respond(
        window,
        false,
        &user,
        input.track_id,
        Source::Live(Box::new(live)),
        pool.orphan_sink(),
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
) -> Result<(NntpClient, Decoded), (Option<NntpClient>, Failure)> {
    let mid = &input.segments[idx];
    let mut warm = pool.take_idle();
    let was_warm = warm.is_some();
    for attempt in 0..2 {
        let mut client = match warm.take() {
            Some(client) => client,
            None => match NntpClient::connect(&input.server_addr) {
                Ok(client) => {
                    pool.record_connect();
                    client
                }
                Err(e) => return Err((None, nntp_failure(e, mid))),
            },
        };
        match fetch_segment(&mut client, mid) {
            Ok(decoded) => return Ok((client, decoded)),
            Err(SegmentError::Nntp(e)) if was_warm && attempt == 0 && e.is_provider_failure() => {
                // Stale pooled connection: drop it and redial once.
                continue;
            }
            Err(SegmentError::Nntp(e)) => {
                let failure = nntp_failure(e, mid);
                let keep = if failure.1 { None } else { Some(client) };
                return Err((keep, failure));
            }
            Err(SegmentError::Decode(e)) => {
                return Err((Some(client), (PlayError::Decode(e), false)));
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

fn nntp_failure(e: NntpError, message_id: &str) -> Failure {
    let provider = e.is_provider_failure();
    let err = match e {
        NntpError::NoSuchArticle(_) => PlayError::ArticleMissing(message_id.to_string()),
        NntpError::InvalidMessageId(id) => PlayError::InvalidSegment(id),
        other => PlayError::Upstream(format!("{other:?}")),
    };
    (err, provider)
}

/// Return the lease (and the connection, if still healthy) and account for
/// the failure: only provider-level trouble quarantines the pool.
fn settle(
    pool: &mut StreamPool,
    meter: &mut dyn Meter,
    lease: Lease,
    client: Option<NntpClient>,
    (err, provider): Failure,
    user: &str,
    track: &str,
) -> PlayError {
    pool.release(lease, if provider { None } else { client });
    if provider {
        pool.record_provider_error();
        meter.emit(ev(MeterKind::ProviderError, user, track, 0));
    }
    err
}

impl Live {
    /// Take one decoded segment: check its placement, decrypt, stage it
    /// (flushing the contiguous prefix into the upload) and make it the
    /// current buffer. Placement is cross-checked against the segment's
    /// declared range when ranges are in play, else against the running
    /// decode cursor (fetch order is file order on the prefix walk).
    fn ingest(&mut self, idx: usize, mut d: Decoded) -> Result<(), Failure> {
        let multi_segment = self.segments.len() > 1;
        let expected = match &self.ranges {
            Some(ranges) => ranges[idx].0,
            None => self.decoded,
        };
        let ranges_mismatch = self.ranges.as_ref().is_some_and(|ranges| {
            ranges[ranges.len() - 1].1 != self.total
                || d.data.len() as u64 != ranges[idx].1 - ranges[idx].0
        });
        let misplaced = d.offset != expected
            || d.file_size != self.total
            || ranges_mismatch
            || (multi_segment && !d.multipart);
        if misplaced {
            return Err((PlayError::Decode(YencError::BadPart), false));
        }
        // Demo cipher (see module docs): the production content key arrives
        // via the BYOP vault and the ingest-owned envelope.
        for b in d.data.iter_mut() {
            *b ^= self.cipher_key;
        }
        self.buf_start = d.offset;
        self.decoded += d.data.len() as u64;
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
                false,
            ));
        }
        Ok(())
    }

    /// Fetch + ingest segment `idx`, wherever it sits in the file.
    fn load(&mut self, idx: usize) -> Result<(), Failure> {
        let mid = self.segments[idx].clone();
        match fetch_segment(&mut self.client, &mid) {
            Ok(decoded) => self.ingest(idx, decoded),
            Err(SegmentError::Nntp(e)) => Err(nntp_failure(e, &mid)),
            Err(SegmentError::Decode(e)) => Err((PlayError::Decode(e), false)),
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
        let failure = failure.unwrap_or((PlayError::Upstream("play failed".into()), false));
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

fn respond(
    (status, content_range, start, end): Window,
    cache_hit: bool,
    user: &str,
    track: &str,
    source: Source,
    orphans: Arc<Mutex<Vec<OrphanedPlay>>>,
) -> PlayResponse {
    PlayResponse {
        status,
        content_range,
        body_len: end - start,
        cache_hit,
        stream: PlayStream {
            user_id: user.to_string(),
            track_id: track.to_string(),
            pos: start,
            end,
            served: 0,
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
        meter.emit(ev(
            MeterKind::BytesServed,
            &self.user_id,
            &self.track_id,
            self.served,
        ));
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
        match live.failed.take() {
            None => {
                let Live {
                    client,
                    lease,
                    upload,
                    ..
                } = *live;
                pool.release(lease, Some(client));
                pool.record_success();
                cache.put_track(&self.track_id, upload.complete());
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
            });
        }
        if let Some(Source::Live(live)) = source {
            let live = *live;
            let healthy = !matches!(live.failed, Some((_, true)));
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
                    live.failed = Some((PlayError::Decode(YencError::Truncated), false));
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

    #[test]
    fn wrong_track_grant_is_rejected() {
        let secret = b"s";
        let token = grant::issue("u", "track-a", 9_999_999_999, secret);
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
        };
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, secret, &input),
            Err(PlayError::WrongTrack)
        ));
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

    #[test]
    fn garbage_token_is_unauthorized_without_touching_the_pool() {
        let secret = b"s";
        let mut pool = Pool::new(1);
        let mut cache = ContentCache::new();
        let mut meter = crate::meter::MemoryMeter::default();
        let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
        let input = PlayInput {
            token: "not-a-token",
            track_id: "t",
            range: None,
            server_addr: addr,
            segments: &[],
            segment_ranges: None,
            cipher_key: 0,
            now_unix: 1,
            up_next: None,
        };
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, secret, &input),
            Err(PlayError::Unauthorized(GrantError::Malformed))
        ));
        assert_eq!(pool.connects(), 0, "rejected plays must not dial out");
    }
}
