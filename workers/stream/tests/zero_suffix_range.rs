//! Acceptance tests (failing-first) for the zero-length suffix range
//! contract: `Range: bytes=-0` asks for the last zero bytes and is
//! unsatisfiable, never a full-body or partial serve.
//!
//! Do not edit these tests to make them pass: implement the contract in
//! production code. These files are owned by the test author; test
//! corrections stay with them.
//!
//! Contracts to implement:
//!
//! 1. **Parser** (`range::parse`): `bytes=-0` resolves to
//!    `RangeOutcome::Unsatisfiable` for nonempty *and* empty bodies.
//!    Today a nonempty body falls back to `Full`, so a client asking for
//!    zero bytes receives the entire track with `200`.
//! 2. **Cached play** (`play` on a cache hit): that outcome surfaces the
//!    existing `416` path — `PlayError::RangeUnsatisfiable`, HTTP `416`,
//!    envelope code `range_not_satisfiable` (not retryable) — with no
//!    successful stream response. No new error envelope.
//! 3. **No collateral change**: normal suffixes, larger-than-body and
//!    exact-body suffixes, closed/open ranges, and the malformed /
//!    multi-range full-body fallback keep their current behavior.
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
use nntune_stream::range::{parse, ByteRange, RangeOutcome};

const SECRET: &[u8] = b"zero-suffix-range-secret";
const NOW_UNIX: u64 = 1_700_000_000;
// Within the grant TTL cap (`MAX_GRANT_TTL` = 900 s).
const EXP_UNIX: u64 = NOW_UNIX + 900;
const BODY_LEN: usize = 100;

/// Deterministic 100-byte cached plaintext (`0, 1, 2, ... 99`).
fn body() -> Vec<u8> {
    (0..BODY_LEN as u8).collect()
}

// --- Parser contract --------------------------------------------------------

#[test]
fn zero_suffix_on_nonempty_body_is_unsatisfiable() {
    assert_eq!(
        parse(Some("bytes=-0"), BODY_LEN as u64),
        RangeOutcome::Unsatisfiable,
        "bytes=-0 on a nonempty body must not select full-body or partial delivery"
    );
    // Boundary: the smallest nonempty body.
    assert_eq!(parse(Some("bytes=-0"), 1), RangeOutcome::Unsatisfiable);
}

#[test]
fn zero_suffix_on_empty_body_is_unsatisfiable() {
    assert_eq!(parse(Some("bytes=-0"), 0), RangeOutcome::Unsatisfiable);
}

#[test]
fn zero_suffix_with_leading_zeros_is_unsatisfiable() {
    // The suffix value is numeric: `-00` is still a zero-length suffix,
    // not a distinct satisfiable form.
    assert_eq!(
        parse(Some("bytes=-00"), BODY_LEN as u64),
        RangeOutcome::Unsatisfiable
    );
    assert_eq!(parse(Some("bytes=-00"), 0), RangeOutcome::Unsatisfiable);
}

#[test]
fn normal_and_boundary_suffixes_retain_partial_delivery() {
    assert_eq!(
        parse(Some("bytes=-1"), BODY_LEN as u64),
        RangeOutcome::Partial(ByteRange { start: 99, end: 99 })
    );
    assert_eq!(
        parse(Some("bytes=-20"), BODY_LEN as u64),
        RangeOutcome::Partial(ByteRange { start: 80, end: 99 })
    );
    // Suffix exactly the body, and larger than the body: whole body as a
    // partial, as today.
    assert_eq!(
        parse(Some("bytes=-100"), BODY_LEN as u64),
        RangeOutcome::Partial(ByteRange { start: 0, end: 99 })
    );
    assert_eq!(
        parse(Some("bytes=-500"), BODY_LEN as u64),
        RangeOutcome::Partial(ByteRange { start: 0, end: 99 })
    );
}

#[test]
fn ordinary_and_malformed_ranges_retain_current_behavior() {
    // Closed and open-ended ranges.
    assert_eq!(
        parse(Some("bytes=0-49"), BODY_LEN as u64),
        RangeOutcome::Partial(ByteRange { start: 0, end: 49 })
    );
    assert_eq!(
        parse(Some("bytes=50-9999"), BODY_LEN as u64),
        RangeOutcome::Partial(ByteRange { start: 50, end: 99 })
    );
    assert_eq!(
        parse(Some("bytes=10-"), BODY_LEN as u64),
        RangeOutcome::Partial(ByteRange { start: 10, end: 99 })
    );
    // Malformed, multi-range, unparseable-suffix and missing headers keep
    // the full-body fallback (`200`), per RFC 9110 §14.2.
    assert_eq!(parse(None, BODY_LEN as u64), RangeOutcome::Full);
    assert_eq!(
        parse(Some("bytes=abc-def"), BODY_LEN as u64),
        RangeOutcome::Full
    );
    assert_eq!(
        parse(Some("bytes=0-10,20-30"), BODY_LEN as u64),
        RangeOutcome::Full
    );
    assert_eq!(parse(Some("bytes=-"), BODY_LEN as u64), RangeOutcome::Full);
    assert_eq!(parse(Some("bytes=-x"), BODY_LEN as u64), RangeOutcome::Full);
}

// --- Cached-play contract ----------------------------------------------------

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
fn cached_play_zero_suffix_surfaces_existing_416() {
    let mut rig = Rig::seeded("track-zs", body());
    let err = match rig.play_range("track-zs", Some("bytes=-0")) {
        Ok(resp) => panic!(
            "bytes=-0 on a cached nonempty track must not produce a successful \
             stream response (got status {}, body_len {})",
            resp.status, resp.body_len
        ),
        Err(err) => err,
    };
    assert_eq!(err, PlayError::RangeUnsatisfiable);
    assert_eq!(err.http_status(), 416);
    // Preserve the existing shared envelope rather than a new one.
    let api = err.api_error();
    assert_eq!(api.code, "range_not_satisfiable");
    assert_eq!(api.status, 416);
    assert!(!api.retryable, "a 416 is not retryable");
}

#[test]
fn cached_play_zero_suffix_on_empty_track_surfaces_416() {
    let mut rig = Rig::seeded("track-empty", Vec::new());
    assert!(matches!(
        rig.play_range("track-empty", Some("bytes=-0")),
        Err(PlayError::RangeUnsatisfiable)
    ));
}

#[test]
fn cached_play_normal_suffix_still_serves_206_tail() {
    let plain = body();
    let mut rig = Rig::seeded("track-zs", plain.clone());
    let resp = rig
        .play_range("track-zs", Some("bytes=-20"))
        .expect("normal suffix serves");
    assert_eq!(resp.status, 206);
    assert_eq!(
        resp.content_range.as_deref(),
        Some(format!("bytes 80-99/{BODY_LEN}").as_str())
    );
    assert_eq!(resp.body_len, 20);
    assert!(resp.cache_hit, "seeded track must serve from cache");
    let served = rig.drain(resp.stream);
    assert_eq!(served, &plain[BODY_LEN - 20..]);
}

#[test]
fn cached_play_larger_than_body_suffix_still_serves_whole_body() {
    let plain = body();
    let mut rig = Rig::seeded("track-zs", plain.clone());
    let resp = rig
        .play_range("track-zs", Some("bytes=-500"))
        .expect("larger-than-body suffix serves");
    assert_eq!(resp.status, 206);
    assert_eq!(
        resp.content_range.as_deref(),
        Some(format!("bytes 0-99/{BODY_LEN}").as_str())
    );
    assert_eq!(resp.body_len, BODY_LEN as u64);
    let served = rig.drain(resp.stream);
    assert_eq!(served, plain);
}

#[test]
fn cached_play_open_range_and_malformed_fallback_unchanged() {
    let plain = body();
    let mut rig = Rig::seeded("track-zs", plain.clone());

    let resp = rig
        .play_range("track-zs", Some("bytes=10-"))
        .expect("open range serves");
    assert_eq!(resp.status, 206);
    assert_eq!(
        resp.content_range.as_deref(),
        Some(format!("bytes 10-99/{BODY_LEN}").as_str())
    );
    let served = rig.drain(resp.stream);
    assert_eq!(served, &plain[10..]);

    // Malformed and multi-range headers keep the 200 full-body fallback.
    for header in ["bytes=abc-def", "bytes=0-10,20-30"] {
        let resp = rig
            .play_range("track-zs", Some(header))
            .unwrap_or_else(|e| panic!("{header} keeps full-body fallback, got {e:?}"));
        assert_eq!(resp.status, 200, "{header} serves the full body");
        assert_eq!(resp.content_range, None);
        assert_eq!(resp.body_len, BODY_LEN as u64);
        let served = rig.drain(resp.stream);
        assert_eq!(served, plain, "{header} serves every byte");
    }
}
