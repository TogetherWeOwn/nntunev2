//! Pin the cached-play inverted-range full-body fallback: `Range:
//! bytes=90-10` (`start > end`) is malformed per the stream worker's range
//! policy and falls back to `200 OK` with the full body, while the adjacent
//! single-byte `bytes=10-10` still serves `206`.
//!
//! Do not edit these tests to make them pass: they pin production behavior
//! in `workers/stream/src/range.rs:90-92`. These files are owned by the test
//! author; test corrections stay with them.
//!
//! All fixtures are native and offline: the cached-play cases seed
//! `ContentCache` directly, so no NNTP upstream, metering store or R2 is
//! contacted (the server address is a dummy a cache hit never dials).

use std::net::SocketAddr;

use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::MemoryMeter;
use nntune_stream::play::{play, PlayError, PlayInput, PlayResponse, PlayStream, StreamPool};
use nntune_stream::pool::Pool;

const SECRET: &[u8] = b"inverted-range-fallback-secret";
const NOW_UNIX: u64 = 1_700_000_000;
// Within the grant TTL cap (`MAX_GRANT_TTL` = 900 s).
const EXP_UNIX: u64 = NOW_UNIX + 900;
const BODY_LEN: usize = 100;

/// Deterministic 100-byte cached plaintext (`0, 1, 2, ... 99`).
fn body() -> Vec<u8> {
    (0..BODY_LEN as u8).collect()
}

struct Rig {
    pool: StreamPool,
    cache: ContentCache,
    meter: MemoryMeter,
}

impl Rig {
    /// A rig whose cache already holds `plaintext` for `track`: every play
    /// below is a hit and never dials the dummy server address.
    fn seeded(track: &str, plaintext: Vec<u8>) -> Self {
        let mut cache = ContentCache::new();
        cache.put_track(track, plaintext);
        Self {
            pool: Pool::new(1),
            cache,
            meter: MemoryMeter::default(),
        }
    }

    fn play_range(&mut self, track: &str, range: Option<&str>) -> Result<PlayResponse, PlayError> {
        let token = grant::issue("u1", track, EXP_UNIX, SECRET);
        let addr: SocketAddr = "127.0.0.1:119".parse().expect("dummy addr");
        let input = PlayInput {
            trace_id: None,
            token: &token,
            track_id: track,
            range,
            server_addr: addr,
            segments: &[],
            segment_ranges: None,
            cipher_key: 0,
            now_unix: NOW_UNIX,
            up_next: None,
        };
        play(
            &mut self.pool,
            &mut self.cache,
            &mut self.meter,
            SECRET,
            &input,
        )
    }

    fn drain(&mut self, mut stream: PlayStream) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in stream.by_ref() {
            out.extend_from_slice(&chunk);
        }
        stream
            .finalize(&mut self.pool, &mut self.cache, &mut self.meter)
            .expect("finalize cached play");
        out
    }
}

#[test]
fn cached_play_inverted_range_serves_full_body_200() {
    let plain = body();
    let mut rig = Rig::seeded("track-inv", plain.clone());
    let resp = rig
        .play_range("track-inv", Some("bytes=90-10"))
        .expect("inverted range falls back to full body");
    assert_eq!(resp.status, 200, "start > end serves the full body");
    assert_eq!(resp.content_range, None, "200 carries no content-range");
    assert_eq!(resp.body_len, BODY_LEN as u64);
    assert!(resp.cache_hit, "seeded track must serve from cache");
    let served = rig.drain(resp.stream);
    assert_eq!(served, plain, "inverted range serves every byte");
}

#[test]
fn cached_play_single_byte_range_still_serves_206() {
    let plain = body();
    let mut rig = Rig::seeded("track-inv", plain.clone());
    let resp = rig
        .play_range("track-inv", Some("bytes=10-10"))
        .expect("single-byte range serves");
    assert_eq!(resp.status, 206);
    assert_eq!(
        resp.content_range.as_deref(),
        Some(format!("bytes 10-10/{BODY_LEN}").as_str())
    );
    assert_eq!(resp.body_len, 1);
    assert!(resp.cache_hit, "seeded track must serve from cache");
    let served = rig.drain(resp.stream);
    assert_eq!(served, &plain[10..11]);
}
