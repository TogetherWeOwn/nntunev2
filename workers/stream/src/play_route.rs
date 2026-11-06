//! The public play route: `GET /play/{track_id}` (see
//! [`crate::routes::PLAY_PATH_PREFIX`]).
//!
//! Portable core over the existing [`crate::play`] orchestration, so
//! native `cargo test` pins the exact behavior the wasm32 adapter
//! renders: verify the signed `v2` grant from the `api` Worker **before
//! any source is touched**, serve the range from the content cache (R2
//! popularity cache) or the NNTP miss path, and report the
//! client-written byte interval of each completed range response to the
//! meter sink ([`crate::delivered`]).
//!
//! Contract points fixed by the replan spec §3.1:
//!
//! - **No grant, no bytes.** The grant travels in the `Authorization:
//!   Bearer` header, never the query string (threat model). An absent,
//!   forged, expired, over-long, wrong-track, or `v1` grant is refused
//!   with the shared error envelope and zero body bytes; rejections that
//!   carry a verified user keep emitting exactly one grant-rejection
//!   meter signal, inside [`crate::play`] as before. A well-formed `v1`
//!   grant carries no `play_id` to meter against, so the route refuses
//!   it `401` before calling the play core (no meter signal: no
//!   rejection kind covers it, and the api Worker only mints `v2` for
//!   this route).
//! - **No speculative preload.** The route always passes
//!   `up_next: None`: delivered-bytes metering would bill unplayed
//!   audio, so the M1 player never prefetches next-track bytes here.
//! - **Delivered = written.** The interval is the span the response
//!   writer actually pulled ([`RoutePlay::finish`] /
//!   [`RoutePlay::abort`] read it from the stream before settling); the
//!   `finalize` cache-fill drain and prefetch deliver nothing to the
//!   client and emit nothing.
//! - **No database.** Everything the route needs beyond the grant comes
//!   from a [`ManifestStore`] (segment metadata; an R2 object in
//!   production, `track/<track_id>/manifest.json` — see
//!   [`TRACK_MANIFEST_SUFFIX`]) so the stream Worker keeps no
//!   Hyperdrive/DB binding; plan and subscription decisions stay at
//!   grant issuance in the `api` Worker.

use std::collections::HashMap;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::api_error::ApiError;
use crate::cache::ContentCache;
use crate::delivered::{DeliveredInterval, DeliveredSink};
use crate::grant;
use crate::meter::Meter;
use crate::play::{self, PlayError, PlayInput, PlayStream, StreamPool};

/// R2 object suffix for a track's segment manifest:
/// `track/<track_id>/manifest.json`, next to the `track/<track_id>`
/// cache pointer. Written by ingest when a collection track becomes
/// playable; this route only reads it.
pub const TRACK_MANIFEST_SUFFIX: &str = "/manifest.json";

/// Content type for collection audio (the catalogue is FLAC).
pub const PLAY_CONTENT_TYPE: &str = "audio/flac";

/// Segment metadata for one playable track: everything the miss path
/// needs that the grant does not carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackManifest {
    /// Ordered segment message-IDs composing the track.
    pub segments: Vec<String>,
    /// Per-segment plaintext byte ranges `(start, end_exclusive)` when
    /// known; lets a mid-track seek fetch its covering segment first.
    pub segment_ranges: Option<Vec<(u64, u64)>>,
    /// Demo cipher key (see [`crate::play`] module docs).
    pub cipher_key: u8,
}

impl TrackManifest {
    /// Parse a manifest from its JSON object bytes; `None` on any
    /// malformed input (the caller treats it as "no manifest").
    pub fn from_json(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// Where the route resolves a track's manifest. Production reads the R2
/// object at `track/<track_id>/manifest.json`; tests use
/// [`MemoryManifestStore`].
pub trait ManifestStore {
    fn manifest(&self, track_id: &str) -> Option<TrackManifest>;
}

/// In-memory manifest store for tests and native harnesses.
#[derive(Debug, Default, Clone)]
pub struct MemoryManifestStore {
    manifests: HashMap<String, TrackManifest>,
}

impl MemoryManifestStore {
    pub fn insert(&mut self, track_id: impl Into<String>, manifest: TrackManifest) {
        self.manifests.insert(track_id.into(), manifest);
    }
}

impl ManifestStore for MemoryManifestStore {
    fn manifest(&self, track_id: &str) -> Option<TrackManifest> {
        self.manifests.get(track_id).cloned()
    }
}

/// One parsed public play request. Header parsing is the adapter's job
/// (see [`bearer_token`]); the track id comes from
/// [`crate::routes::parse_play_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayRouteRequest {
    pub track_id: String,
    /// The bearer grant token, if the header carried one.
    pub token: Option<String>,
    /// Value of the HTTP `Range` header, if any.
    pub range: Option<String>,
    /// Value of the `x-trace-id` header, if any (validated by the play
    /// core; never affects authorization).
    pub trace_id: Option<String>,
}

/// Extract the token from an `Authorization` header value: `Bearer
/// <token>` with a case-insensitive scheme and a single space-separated
/// token. Anything else (missing header, other schemes, empty or
/// space-containing tokens) yields `None` — refused as no-grant by the
/// route, never a parse error leaking shape.
pub fn bearer_token(header: Option<&str>) -> Option<&str> {
    let header = header?;
    let (scheme, token) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() || token.contains(' ') {
        return None;
    }
    Some(token)
}

/// A refused play: render `api_error.body_json()` with `status` (and
/// `Retry-After` / `Content-Range` when carried). No media bytes are served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayRouteRefusal {
    pub status: u16,
    pub api_error: ApiError,
    pub retry_after: Option<String>,
    /// For a 416 with a known file size: `bytes */<size>`.
    pub content_range: Option<String>,
}

impl From<&PlayError> for PlayRouteRefusal {
    fn from(error: &PlayError) -> Self {
        PlayRouteRefusal {
            status: error.http_status(),
            api_error: error.api_error(),
            retry_after: error.retry_after_header(),
            content_range: None,
        }
    }
}

/// A servable play: status + headers for the adapter, and the body
/// stream. The adapter pulls chunks, writes them to the client, then
/// settles with [`RoutePlay::finish`] (or [`RoutePlay::abort`] when the
/// client went away) so the delivered interval is reported.
pub struct PlayRouteServe {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// Bytes this response will serve when fully pulled.
    pub body_len: u64,
    /// A stale cache entry served and this response claimed its one
    /// background revalidation (see `PlayResponse::needs_revalidate`);
    /// acting on it is the embedding runtime's job.
    pub needs_revalidate: bool,
    pub play: RoutePlay,
}

/// The outcome of one play-route request.
pub enum PlayRouteOutcome {
    Serve(PlayRouteServe),
    Refuse(PlayRouteRefusal),
}

/// A play in flight on the public route: pull body chunks with
/// [`Iterator::next`], then settle exactly once with `finish`/`abort`.
pub struct RoutePlay {
    stream: PlayStream,
    user_id: String,
    play_id: String,
    track_id: String,
}

impl Iterator for RoutePlay {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Vec<u8>> {
        self.stream.next()
    }
}

impl RoutePlay {
    /// Bytes handed to the response writer so far.
    pub fn served_bytes(&self) -> u64 {
        self.stream.served_bytes()
    }

    /// Why the stream stopped early, if it did (miss path only).
    pub fn error(&self) -> Option<&PlayError> {
        self.stream.error()
    }

    /// The interval this response has written so far, if any bytes went
    /// out: `(play_id, [start, end))` plus its owning ids.
    fn interval(&self) -> Option<DeliveredInterval> {
        let (start, end) = self.stream.delivered_span();
        if end <= start {
            return None;
        }
        Some(DeliveredInterval {
            user_id: self.user_id.clone(),
            play_id: self.play_id.clone(),
            track_id: self.track_id.clone(),
            start,
            end,
            // The portable core serves slices without a confirmed file
            // total; the wasm route (which reads the blob length) is
            // the path that supplies it. The api derivation resolves
            // its own metadata when this is absent.
            file_bytes: None,
        })
    }

    /// Settle a completed (or writer-stopped) response: run the play
    /// core's `finalize` (cache write-back on a miss; its drain delivers
    /// nothing to the client) and report the written interval. The
    /// interval is reported even when `finalize` surfaces a serve-phase
    /// error — the bytes were already delivered.
    pub fn finish(
        self,
        pool: &mut StreamPool,
        cache: &mut ContentCache,
        meter: &mut dyn Meter,
        sink: &mut dyn DeliveredSink,
    ) -> Result<(), PlayError> {
        let interval = self.interval();
        let result = self.stream.finalize(pool, cache, meter);
        if let Some(interval) = interval {
            sink.delivered(interval);
        }
        result
    }

    /// Settle a response whose client went away mid-range: return the
    /// connection without caching and report the partial interval that
    /// was actually written.
    pub fn abort(self, pool: &mut StreamPool, sink: &mut dyn DeliveredSink) {
        let interval = self.interval();
        self.stream.abort(pool);
        if let Some(interval) = interval {
            sink.delivered(interval);
        }
    }
}

/// Run one public play request end to end up to the first body byte:
/// grant gate, then the play core (cache hit or NNTP miss).
#[allow(clippy::too_many_arguments)]
pub fn serve_play(
    pool: &mut StreamPool,
    cache: &mut ContentCache,
    meter: &mut dyn Meter,
    secret: &[u8],
    manifests: &dyn ManifestStore,
    server_addr: SocketAddr,
    now_unix: u64,
    request: &PlayRouteRequest,
) -> PlayRouteOutcome {
    let token = request.token.as_deref().unwrap_or("");
    // Pre-verify only to learn the play id (and to refuse a well-formed
    // `v1` grant, which has none, before any play starts). Every other
    // rejection is reproduced by the play core below so its
    // grant-rejection meter signals fire exactly once, as before.
    let mut play_id: Option<String> = None;
    let mut user_id: Option<String> = None;
    // Authorization definitively completes only when the grant verifies
    // *and* binds this track. Until then no source is touched — not even
    // the manifest store: a failed or wrong-track pre-verify falls
    // through to the play core with no manifest, and the core
    // authorizes before it looks at segments, so the rejection (and its
    // meter signal) is unchanged.
    let mut authorized = false;
    if let Ok(grant) = grant::verify(token, secret, now_unix) {
        match grant.play_id {
            Some(id) => {
                authorized = grant.track_id == request.track_id;
                play_id = Some(id);
                user_id = Some(grant.user_id);
            }
            None => {
                return PlayRouteOutcome::Refuse(PlayRouteRefusal {
                    status: 401,
                    api_error: ApiError::unauthorized(),
                    retry_after: None,
                    content_range: None,
                });
            }
        }
    }

    let manifest = if authorized {
        manifests.manifest(&request.track_id)
    } else {
        None
    };
    let (segments, segment_ranges, cipher_key) = match &manifest {
        Some(manifest) => (
            manifest.segments.clone(),
            manifest.segment_ranges.clone(),
            manifest.cipher_key,
        ),
        None => (Vec::new(), None, 0),
    };
    let input = PlayInput {
        token,
        track_id: &request.track_id,
        range: request.range.as_deref(),
        server_addr,
        segments: &segments,
        segment_ranges: segment_ranges.as_deref(),
        cipher_key,
        now_unix,
        // M1: no speculative next-track preload (see module docs).
        up_next: None,
        trace_id: request.trace_id.as_deref(),
    };
    match play::play(pool, cache, meter, secret, &input) {
        Ok(response) => {
            let (play_id, user_id) = match (play_id, user_id) {
                (Some(play_id), Some(user_id)) => (play_id, user_id),
                // `play` authorizes with the same verify at the same
                // instant, so success implies the pre-verify succeeded.
                _ => unreachable!("play authorized a grant the route could not verify"),
            };
            let mut headers = vec![
                ("content-type".to_string(), PLAY_CONTENT_TYPE.to_string()),
                ("accept-ranges".to_string(), "bytes".to_string()),
                ("content-length".to_string(), response.body_len.to_string()),
            ];
            if let Some(content_range) = &response.content_range {
                headers.push(("content-range".to_string(), content_range.clone()));
            }
            headers.extend(response.cache_headers());
            PlayRouteOutcome::Serve(PlayRouteServe {
                status: response.status,
                headers,
                body_len: response.body_len,
                needs_revalidate: response.needs_revalidate,
                play: RoutePlay {
                    stream: response.stream,
                    user_id,
                    play_id,
                    track_id: request.track_id.clone(),
                },
            })
        }
        Err(error) => {
            // No manifest and no cache entry: the play core's only
            // verdict for an empty segment list is `InvalidSegment`,
            // but for the route this track is simply unknown.
            if manifest.is_none() && matches!(error, PlayError::InvalidSegment(_)) {
                return PlayRouteOutcome::Refuse(PlayRouteRefusal {
                    status: 404,
                    api_error: ApiError::not_found(),
                    retry_after: None,
                    content_range: None,
                });
            }
            PlayRouteOutcome::Refuse(PlayRouteRefusal::from(&error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_token_accepts_only_the_bearer_scheme() {
        assert_eq!(bearer_token(Some("Bearer v2.a.b")), Some("v2.a.b"));
        assert_eq!(bearer_token(Some("bearer v2.a.b")), Some("v2.a.b"));
        assert_eq!(bearer_token(Some("BEARER v2.a.b")), Some("v2.a.b"));
        for bad in [
            None,
            Some(""),
            Some("Bearer"),
            Some("Bearer "),
            Some("Basic v2.a.b"),
            Some("v2.a.b"),
            Some("Bearer two tokens"),
        ] {
            assert_eq!(bearer_token(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn manifest_json_round_trips_and_rejects_garbage() {
        let manifest = TrackManifest {
            segments: vec!["a@x".to_string(), "b@x".to_string()],
            segment_ranges: Some(vec![(0, 10), (10, 25)]),
            cipher_key: 7,
        };
        let json = serde_json::to_vec(&manifest).expect("serialize");
        assert_eq!(TrackManifest::from_json(&json), Some(manifest));
        assert_eq!(TrackManifest::from_json(b"[]"), None);
        assert_eq!(TrackManifest::from_json(b""), None);
    }
}
