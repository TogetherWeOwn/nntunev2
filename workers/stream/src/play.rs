//! Play path: authorize → cache hit → `200`/`206`; miss → fetch segments
//! via the pool → yEnc → decrypt → stream to the client while staging the
//! R2 multipart write, then prefetch the next track.
//!
//! Decryption here is a demo XOR stream cipher standing in for the
//! ingest-owned content encryption (key delivery via the BYOP vault is a
//! sibling card). The byte flow — fetch, decode, decrypt, fan out to client
//! chunks *and* the multipart upload — is the production shape.
//!
//! Cache-write discipline: on a servable miss the fetched body is staged
//! into a [`MultipartUpload`](crate::cache::MultipartUpload) owned by the
//! [`PlayStream`]; [`PlayStream::finalize`] completes it into the cache once
//! the client has been fully served (mirroring R2 multipart completion at
//! end-of-stream). A miss with an unsatisfiable range caches inline instead,
//! since there is no stream to finalize — the next play for the track still
//! hits.

use std::net::SocketAddr;

use crate::cache::{ContentCache, MultipartUpload};
use crate::grant::{self, GrantError};
use crate::meter::{Meter, MeterEvent, MeterKind};
use crate::nntp::NntpClient;
use crate::pool::{Lane, Pool, PoolError};
use crate::prefetch::should_prefetch;
use crate::range::{self, RangeOutcome};
use crate::yenc::{self, YencError};

/// Client chunk size for served streams.
const CHUNK: usize = 16 * 1024;

/// Why a play failed.
#[derive(Debug, PartialEq, Eq)]
pub enum PlayError {
    Unauthorized(GrantError),
    WrongTrack,
    RangeUnsatisfiable,
    PoolExhausted,
    PoolQuarantined { retry_after_ms: u64 },
    Upstream(String),
    Decode(YencError),
}

/// Everything the play path needs, by reference.
pub struct PlayInput<'a> {
    /// Signed grant from the `api` Worker.
    pub token: &'a str,
    pub track_id: &'a str,
    /// Value of the HTTP `Range` header, if any.
    pub range: Option<&'a str>,
    /// Provider NNTP address (one pooled connection dials this).
    pub server_addr: SocketAddr,
    /// Ordered segment message-IDs composing the track.
    pub segments: &'a [String],
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
    /// Bytes actually served.
    pub body_len: u64,
    pub cache_hit: bool,
    pub stream: PlayStream,
}

/// Chunked body. On a miss the chunks are served while the full body is
/// staged into the R2 multipart upload; [`PlayStream::finalize`] completes
/// it into the cache once the client has been fully served.
pub struct PlayStream {
    serve: Vec<Vec<u8>>,
    pos: usize,
    writeback: Option<Writeback>,
}

struct Writeback {
    track_id: String,
    upload: MultipartUpload,
}

/// Serve `track_id` to its granted owner.
pub fn play(
    pool: &mut Pool,
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
    meter.emit(ev(MeterKind::PlayStart, &user, input.track_id, 0));

    if let Some(full) = cache.get_track(input.track_id) {
        meter.emit(ev(MeterKind::CacheHit, &user, input.track_id, 0));
        let (status, content_range, slice) = resolve_range(&full, input.range)?;
        meter.emit(ev(
            MeterKind::BytesServed,
            &user,
            input.track_id,
            slice.len() as u64,
        ));
        return Ok(respond(status, content_range, slice, true, None));
    }

    meter.emit(ev(MeterKind::CacheMiss, &user, input.track_id, 0));
    let lease = pool.try_acquire(Lane::Playback).map_err(|e| match e {
        PoolError::Quarantined { retry_after_ms } => PlayError::PoolQuarantined { retry_after_ms },
        PoolError::Exhausted => PlayError::PoolExhausted,
    })?;
    let fetched = fetch_track(input);
    match fetched {
        Ok(plain) => {
            pool.release(lease);
            pool.record_success();
            let (status, content_range, slice) = match resolve_range(&plain, input.range) {
                Ok(resolved) => resolved,
                Err(e) => {
                    // No stream will exist to finalize; cache inline so the
                    // next play still hits.
                    cache.put_track(input.track_id, plain);
                    return Err(e);
                }
            };
            meter.emit(ev(
                MeterKind::BytesServed,
                &user,
                input.track_id,
                slice.len() as u64,
            ));
            let mut upload = MultipartUpload::begin(input.track_id);
            for part in plain.chunks(CHUNK) {
                upload.part(part);
            }
            if let Some(next) = input.up_next {
                if should_prefetch(pool.in_use(), pool.capacity()) {
                    meter.emit(ev(MeterKind::PrefetchEnqueued, &user, next, 0));
                }
            }
            Ok(respond(
                status,
                content_range,
                slice,
                false,
                Some(Writeback {
                    track_id: input.track_id.to_string(),
                    upload,
                }),
            ))
        }
        Err(e) => {
            pool.release(lease);
            pool.record_provider_error();
            meter.emit(ev(MeterKind::ProviderError, &user, input.track_id, 0));
            Err(e)
        }
    }
}

/// Fetch, yEnc-decode and decrypt every segment over one pooled connection.
fn fetch_track(input: &PlayInput<'_>) -> Result<Vec<u8>, PlayError> {
    let mut client = NntpClient::connect(&input.server_addr)
        .map_err(|e| PlayError::Upstream(format!("{e:?}")))?;
    let mut plain = Vec::new();
    for mid in input.segments {
        let raw = client
            .article(mid)
            .map_err(|e| PlayError::Upstream(format!("{e:?}")))?;
        let mut decoded = yenc::decode(&raw).map_err(PlayError::Decode)?;
        // Demo cipher (see module docs): the production content key arrives
        // via the BYOP vault and the ingest-owned envelope.
        for b in decoded.iter_mut() {
            *b ^= input.cipher_key;
        }
        plain.extend_from_slice(&decoded);
    }
    Ok(plain)
}

fn resolve_range(
    full: &[u8],
    range: Option<&str>,
) -> Result<(u16, Option<String>, Vec<u8>), PlayError> {
    match range::parse(range, full.len() as u64) {
        RangeOutcome::Full => Ok((200, None, full.to_vec())),
        RangeOutcome::Partial(r) => {
            let slice = full[r.start as usize..=r.end as usize].to_vec();
            let content_range = range::content_range_value(r, full.len() as u64);
            Ok((206, Some(content_range), slice))
        }
        RangeOutcome::Unsatisfiable => Err(PlayError::RangeUnsatisfiable),
    }
}

fn respond(
    status: u16,
    content_range: Option<String>,
    slice: Vec<u8>,
    cache_hit: bool,
    writeback: Option<Writeback>,
) -> PlayResponse {
    let body_len = slice.len() as u64;
    let serve: Vec<Vec<u8>> = slice.chunks(CHUNK).map(|c| c.to_vec()).collect();
    PlayResponse {
        status,
        content_range,
        body_len,
        cache_hit,
        stream: PlayStream {
            serve,
            pos: 0,
            writeback,
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

impl PlayStream {
    /// True on the miss path (chunks feed a staged multipart upload).
    pub fn is_writeback(&self) -> bool {
        self.writeback.is_some()
    }

    /// Complete the staged multipart write into `cache` (no-op on hits).
    pub fn finalize(self, cache: &mut ContentCache) {
        if let Some(writeback) = self.writeback {
            let bytes = writeback.upload.complete();
            cache.put_track(&writeback.track_id, bytes);
        }
    }
}

impl Iterator for PlayStream {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Vec<u8>> {
        if self.pos < self.serve.len() {
            let chunk = self.serve[self.pos].clone();
            self.pos += 1;
            Some(chunk)
        } else {
            None
        }
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
