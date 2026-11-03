//! Acceptance for the public play route (`GET /play/{track_id}`):
//!
//! - A valid `v2` grant streams the fixture track end to end on both the
//!   content-cache (R2) hit path and the NNTP miss path, with working
//!   seek/range (`206` slices byte-identical to the plaintext).
//! - An absent, forged, expired, over-long, `v1`, or wrong-track grant is
//!   refused with zero body bytes and zero provider contact (no grant →
//!   no bytes, and the source is never touched).
//! - The delivered-byte interval emitted per range response is exactly
//!   the span the response writer pulled `(play_id, [start, end))` —
//!   including a mid-range abort — while the `finalize` cache-fill drain
//!   emits nothing.
//!
//! The synthetic NNTP server and yEnc fixture mirror
//! `stream_integration.rs` (std-only threads, connection/BODY counters).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use nntune_stream::cache::ContentCache;
use nntune_stream::delivered::{
    parse_delivered_report, serve_delivered, DeliveredInterval, VecDeliveredSink,
};
use nntune_stream::grant;
use nntune_stream::meter::{MemoryMeter, MeterKind};
use nntune_stream::play::StreamPool;
use nntune_stream::play_route::{
    serve_play, ManifestStore, MemoryManifestStore, PlayRouteOutcome, PlayRouteRefusal,
    PlayRouteRequest, PlayRouteServe, TrackManifest,
};
use nntune_stream::pool::Pool;
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"play-route-acceptance-secret";
const NOW_UNIX: u64 = 1_700_000_000;
const EXP_UNIX: u64 = NOW_UNIX + 900;
const CIPHER_KEY: u8 = 0x5A;
const TRACK_ID: &str = "track-1";

type Fixture = (Vec<u8>, Vec<String>, HashMap<String, Vec<u8>>);

/// Deterministic "fixture FLAC": the `fLaC` stream marker followed by
/// pseudo-audio covering every byte value (exercises yEnc escapes),
/// posted the way Usenet does it — one yEnc *multipart* article per
/// `seg_len` bytes. The play path is byte-transparent, so the payload
/// standing in for decoded FLAC frames is what must round-trip exactly.
fn fixture_track(tag: &str, len: usize, seg_len: usize) -> Fixture {
    let mut plain = Vec::with_capacity(len);
    plain.extend_from_slice(b"fLaC");
    for i in plain.len()..len {
        plain.push(((i * 7919 + tag.len()) % 256) as u8);
    }
    let ciphered: Vec<u8> = plain.iter().map(|b| b ^ CIPHER_KEY).collect();
    let file_crc = yenc::crc32(&ciphered);
    let total = ciphered.len().div_ceil(seg_len) as u32;
    let mut articles = HashMap::new();
    let mut segments = Vec::new();
    for (i, chunk) in ciphered.chunks(seg_len).enumerate() {
        let part = i as u32 + 1;
        let mid = format!("{tag}-seg{i}@synthetic.test");
        let text = yenc::encode_part(
            chunk,
            &format!("{tag}.flac"),
            PartSpec {
                file_size: len as u64,
                part,
                total,
                offset: (i * seg_len) as u64,
                file_crc: (part == total).then_some(file_crc),
            },
        );
        articles.insert(mid.clone(), text);
        segments.push(mid);
    }
    (plain, segments, articles)
}

/// Per-segment plaintext byte ranges: `[(0, seg_len), ...]` clamped to
/// `len` — the shape ingest writes into the track manifest.
fn segment_ranges_for(len: usize, seg_len: usize) -> Vec<(u64, u64)> {
    let mut ranges = Vec::new();
    let mut start = 0u64;
    while (start as usize) < len {
        let end = ((start as usize) + seg_len).min(len) as u64;
        ranges.push((start, end));
        start = end;
    }
    ranges
}

/// Minimal NNTP server: greeting + `BODY <mid>` with dot-stuffing, with
/// connection and BODY counters so refusal tests can prove the source
/// was never touched.
struct FakeNntp {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    connects: Arc<AtomicUsize>,
    bodies: Arc<AtomicUsize>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeNntp {
    fn start(articles: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
        let addr = listener.local_addr().expect("local addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let connects = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(AtomicUsize::new(0));
        let (stop_c, connects_c, bodies_c) = (
            Arc::clone(&stop),
            Arc::clone(&connects),
            Arc::clone(&bodies),
        );
        let handle = thread::spawn(move || {
            while !stop_c.load(Ordering::SeqCst) {
                let (stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                connects_c.fetch_add(1, Ordering::SeqCst);
                Self::serve(stream, &articles, &bodies_c);
            }
        });
        Self {
            addr,
            stop,
            connects,
            bodies,
            handle: Some(handle),
        }
    }

    fn serve(stream: TcpStream, articles: &HashMap<String, Vec<u8>>, bodies: &AtomicUsize) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone conn"));
        let mut writer = stream;
        let _ = writer.write_all(b"200 synthetic NNTP ready\r\n");
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            while line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
                line.pop();
            }
            let cmd = String::from_utf8_lossy(&line).into_owned();
            if let Some(mid) = cmd
                .strip_prefix("BODY ")
                .map(|s| s.trim().trim_matches(|c| c == '<' || c == '>'))
            {
                bodies.fetch_add(1, Ordering::SeqCst);
                let full_mid = format!("<{mid}>");
                match articles.get(mid) {
                    Some(article) => {
                        let _ =
                            writer.write_all(format!("222 0 {full_mid} follows\r\n").as_bytes());
                        for raw_line in article.split(|&b| b == b'\n') {
                            let text_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
                            if text_line.first() == Some(&b'.') {
                                let _ = writer.write_all(b".");
                            }
                            let _ = writer.write_all(text_line);
                            let _ = writer.write_all(b"\r\n");
                        }
                        let _ = writer.write_all(b".\r\n");
                    }
                    None => {
                        let _ = writer.write_all(b"430 no such article\r\n");
                    }
                }
            } else if cmd == "QUIT" {
                let _ = writer.write_all(b"205 bye\r\n");
                break;
            } else {
                let _ = writer.write_all(b"500 unknown command\r\n");
            }
        }
    }

    fn connects(&self) -> usize {
        self.connects.load(Ordering::SeqCst)
    }

    fn bodies(&self) -> usize {
        self.bodies.load(Ordering::SeqCst)
    }
}

impl Drop for FakeNntp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// One route fixture: server + manifest store + the moving parts
/// `serve_play` drives. `sink` collects the delivered intervals.
struct Harness {
    server: FakeNntp,
    manifests: MemoryManifestStore,
    pool: StreamPool,
    cache: ContentCache,
    meter: MemoryMeter,
    sink: VecDeliveredSink,
    plain: Vec<u8>,
}

impl Harness {
    fn new(len: usize, seg_len: usize, with_ranges: bool) -> Self {
        let (plain, segments, articles) = fixture_track("route", len, seg_len);
        assert_eq!(&plain[..4], b"fLaC", "fixture carries the FLAC marker");
        let server = FakeNntp::start(articles);
        let mut manifests = MemoryManifestStore::default();
        manifests.insert(
            TRACK_ID,
            TrackManifest {
                segments,
                segment_ranges: with_ranges.then(|| segment_ranges_for(len, seg_len)),
                cipher_key: CIPHER_KEY,
            },
        );
        Self {
            server,
            manifests,
            pool: Pool::new(4),
            cache: ContentCache::new(),
            meter: MemoryMeter::default(),
            sink: VecDeliveredSink::default(),
            plain,
        }
    }

    fn request(&mut self, token: Option<String>, range: Option<&str>) -> PlayRouteOutcome {
        let request = PlayRouteRequest {
            track_id: TRACK_ID.to_string(),
            token,
            range: range.map(str::to_string),
            trace_id: None,
        };
        serve_play(
            &mut self.pool,
            &mut self.cache,
            &mut self.meter,
            SECRET,
            &self.manifests,
            self.server.addr,
            NOW_UNIX,
            &request,
        )
    }

    /// Pull every chunk the response writer would write, then settle
    /// with `finish` (which reports the delivered interval).
    fn serve_to_completion(&mut self, serve: PlayRouteServe) -> Vec<u8> {
        let PlayRouteServe { mut play, .. } = serve;
        let mut body = Vec::new();
        for chunk in play.by_ref() {
            body.extend_from_slice(&chunk);
        }
        assert_eq!(
            play.served_bytes() as usize,
            body.len(),
            "served_bytes must equal what the writer pulled"
        );
        play.finish(
            &mut self.pool,
            &mut self.cache,
            &mut self.meter,
            &mut self.sink,
        )
        .expect("finalize");
        body
    }
}

fn grant_v2(play_id: &str) -> String {
    grant::issue_v2("user-1", TRACK_ID, play_id, 3, EXP_UNIX, SECRET)
}

fn interval(play_id: &str, start: u64, end: u64) -> DeliveredInterval {
    DeliveredInterval {
        user_id: "user-1".to_string(),
        play_id: play_id.to_string(),
        track_id: TRACK_ID.to_string(),
        start,
        end,
    }
}

fn expect_serve(outcome: PlayRouteOutcome) -> PlayRouteServe {
    match outcome {
        PlayRouteOutcome::Serve(serve) => serve,
        PlayRouteOutcome::Refuse(refusal) => panic!("expected serve, refused: {refusal:?}"),
    }
}

fn expect_refusal(outcome: PlayRouteOutcome) -> PlayRouteRefusal {
    match outcome {
        PlayRouteOutcome::Refuse(refusal) => refusal,
        PlayRouteOutcome::Serve(_) => panic!("expected refusal, got a serve"),
    }
}

#[test]
fn nntp_miss_streams_the_fixture_end_to_end_and_reports_the_full_interval() {
    let mut h = Harness::new(40_000, 20_000, true);
    let serve = expect_serve(h.request(Some(grant_v2("play-1")), None));
    assert_eq!(serve.status, 200);
    assert_eq!(serve.body_len as usize, h.plain.len());
    assert!(serve
        .headers
        .contains(&("content-type".to_string(), "audio/flac".to_string())));
    assert!(serve
        .headers
        .contains(&("accept-ranges".to_string(), "bytes".to_string())));
    assert!(serve
        .headers
        .contains(&("content-length".to_string(), h.plain.len().to_string())));

    let body = h.serve_to_completion(serve);
    assert_eq!(body, h.plain, "miss path must reproduce the fixture bytes");
    assert_eq!(h.server.bodies(), 2, "both segments fetched once");
    assert_eq!(
        h.sink.0,
        vec![interval("play-1", 0, h.plain.len() as u64)],
        "one interval, exactly the bytes written"
    );
    assert_eq!(
        h.cache.get_track(TRACK_ID).as_deref(),
        Some(h.plain.as_slice()),
        "the miss fills the popularity cache"
    );
}

#[test]
fn cache_hit_serves_a_seek_slice_without_dialing_and_reports_the_slice() {
    let mut h = Harness::new(40_000, 20_000, true);
    let seed = expect_serve(h.request(Some(grant_v2("play-1")), None));
    let body = h.serve_to_completion(seed);
    assert_eq!(body, h.plain);
    let (connects, bodies) = (h.server.connects(), h.server.bodies());

    let serve = expect_serve(h.request(Some(grant_v2("play-2")), Some("bytes=100-199")));
    assert_eq!(serve.status, 206);
    assert!(serve.headers.contains(&(
        "content-range".to_string(),
        format!("bytes 100-199/{}", h.plain.len())
    )));
    assert_eq!(serve.body_len, 100);
    let slice = h.serve_to_completion(serve);
    assert_eq!(slice, &h.plain[100..200], "seek slice is byte-identical");
    assert_eq!(
        (h.server.connects(), h.server.bodies()),
        (connects, bodies),
        "a cache hit never dials"
    );
    assert_eq!(
        h.sink.0.last(),
        Some(&interval("play-2", 100, 200)),
        "the hit reports exactly the served slice"
    );
}

#[test]
fn nntp_seek_fetches_only_the_covering_segment_and_finalize_drain_emits_nothing() {
    let mut h = Harness::new(60_000, 20_000, true);
    let serve = expect_serve(h.request(Some(grant_v2("play-seek")), Some("bytes=45000-45099")));
    assert_eq!(serve.status, 206);
    let mut play = serve.play;
    let first = play.next().expect("first seek chunk");
    assert_eq!(
        h.server.bodies(),
        1,
        "segment ranges address the covering article directly"
    );
    let mut body = first;
    for chunk in play.by_ref() {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body, &h.plain[45_000..45_100]);
    play.finish(&mut h.pool, &mut h.cache, &mut h.meter, &mut h.sink)
        .expect("finalize drains the cache fill");
    assert_eq!(
        h.cache.get_track(TRACK_ID).as_deref(),
        Some(h.plain.as_slice()),
        "the drain still caches the whole track"
    );
    assert_eq!(
        h.sink.0,
        vec![interval("play-seek", 45_000, 45_100)],
        "cache-fill bytes deliver nothing to the client and emit nothing"
    );
}

#[test]
fn mid_range_abort_reports_only_the_bytes_actually_written() {
    let mut h = Harness::new(60_000, 20_000, true);
    let serve = expect_serve(h.request(Some(grant_v2("play-abort")), None));
    let mut play = serve.play;
    let first = play.next().expect("first chunk");
    let written = first.len() as u64;
    assert!(written > 0 && written < h.plain.len() as u64);
    assert_eq!(play.served_bytes(), written);
    play.abort(&mut h.pool, &mut h.sink);
    assert_eq!(
        h.sink.0,
        vec![interval("play-abort", 0, written)],
        "abort truncates the interval to what was written"
    );
    assert!(
        h.cache.get_track(TRACK_ID).is_none(),
        "abort must not cache"
    );
    assert_eq!(h.pool.in_use(), 0, "abort returns the lease");
}

#[test]
fn bad_grants_are_refused_with_no_bytes_and_no_provider_contact() {
    let mut h = Harness::new(40_000, 20_000, true);
    let forged = grant::issue_v2(
        "user-1",
        TRACK_ID,
        "play-x",
        3,
        EXP_UNIX,
        b"the-wrong-secret",
    );
    let expired = grant::issue_v2("user-1", TRACK_ID, "play-x", 3, NOW_UNIX - 1, SECRET);
    let over_long = grant::issue_v2("user-1", TRACK_ID, "play-x", 3, NOW_UNIX + 5_000, SECRET);
    let v1 = grant::issue("user-1", TRACK_ID, EXP_UNIX, SECRET);
    let wrong_track = grant::issue_v2("user-1", "track-other", "play-x", 3, EXP_UNIX, SECRET);

    let cases: [(Option<String>, u16, &str); 7] = [
        (None, 401, "unauthorized"),
        (Some("not-a-grant".to_string()), 401, "unauthorized"),
        (Some(forged), 401, "unauthorized"),
        (Some(expired), 401, "unauthorized"),
        (Some(over_long), 401, "unauthorized"),
        (Some(v1), 401, "unauthorized"),
        (Some(wrong_track), 403, "forbidden"),
    ];
    for (token, status, code) in cases {
        let refusal = expect_refusal(h.request(token, None));
        assert_eq!(refusal.status, status, "code {code}");
        assert_eq!(refusal.api_error.code, code);
        assert_eq!(refusal.api_error.status, status);
    }
    assert!(h.sink.0.is_empty(), "refusals emit no delivered intervals");
    assert_eq!(
        (h.server.connects(), h.server.bodies()),
        (0, 0),
        "authorization happens before any source is touched"
    );
    assert!(h.cache.get_track(TRACK_ID).is_none());

    // The grant-rejection meter signals fire exactly once each (the route
    // pre-verifies only to learn the play id / refuse `v1`; the play core
    // owns the signal), and a `v1` grant emits none.
    for kind in [
        MeterKind::GrantRejectedExpired,
        MeterKind::GrantRejectedExcessiveExpiry,
        MeterKind::GrantRejectedWrongTrack,
    ] {
        let count = h.meter.events.iter().filter(|e| e.kind == kind).count();
        assert_eq!(count, 1, "{kind:?} must fire exactly once");
    }
}

/// Manifest store that counts lookups, so refusal tests can prove the
/// route consults no source — not even segment metadata — until the
/// grant has verified and bound the requested track.
struct CountingManifestStore {
    inner: MemoryManifestStore,
    lookups: AtomicUsize,
}

impl ManifestStore for CountingManifestStore {
    fn manifest(&self, track_id: &str) -> Option<TrackManifest> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        self.inner.manifest(track_id)
    }
}

#[test]
fn manifest_store_is_never_consulted_before_authorization_completes() {
    let mut h = Harness::new(40_000, 20_000, true);
    let store = CountingManifestStore {
        inner: h.manifests.clone(),
        lookups: AtomicUsize::new(0),
    };
    let forged = grant::issue_v2("user-1", TRACK_ID, "play-x", 3, EXP_UNIX, b"wrong-secret");
    let expired = grant::issue_v2("user-1", TRACK_ID, "play-x", 3, NOW_UNIX - 1, SECRET);
    let v1 = grant::issue("user-1", TRACK_ID, EXP_UNIX, SECRET);
    let v1_wrong_track = grant::issue("user-1", "track-other", EXP_UNIX, SECRET);
    let wrong_track = grant::issue_v2("user-1", "track-other", "play-x", 3, EXP_UNIX, SECRET);
    let unmeterable = grant::issue_v2("user:1", TRACK_ID, "play-x", 3, EXP_UNIX, SECRET);

    let cases: [(Option<String>, u16); 7] = [
        (None, 401),
        (Some(forged), 401),
        (Some(expired), 401),
        (Some(v1), 401),
        // The `v2` requirement precedes the track binding in both
        // adapters: a `v1` grant for another track is a 401, not a 403.
        (Some(v1_wrong_track), 401),
        (Some(wrong_track), 403),
        (Some(unmeterable), 401),
    ];
    for (token, status) in cases {
        let request = PlayRouteRequest {
            track_id: TRACK_ID.to_string(),
            token,
            range: None,
            trace_id: None,
        };
        let refusal = expect_refusal(serve_play(
            &mut h.pool,
            &mut h.cache,
            &mut h.meter,
            SECRET,
            &store,
            h.server.addr,
            NOW_UNIX,
            &request,
        ));
        assert_eq!(refusal.status, status);
    }
    assert_eq!(
        store.lookups.load(Ordering::SeqCst),
        0,
        "no manifest lookup before the grant verifies and binds the track"
    );

    // An authorized request consults the store exactly once.
    let request = PlayRouteRequest {
        track_id: TRACK_ID.to_string(),
        token: Some(grant_v2("play-counted")),
        range: None,
        trace_id: None,
    };
    let serve = expect_serve(serve_play(
        &mut h.pool,
        &mut h.cache,
        &mut h.meter,
        SECRET,
        &store,
        h.server.addr,
        NOW_UNIX,
        &request,
    ));
    assert_eq!(store.lookups.load(Ordering::SeqCst), 1);
    let body = h.serve_to_completion(serve);
    assert_eq!(body, h.plain);
}

#[test]
fn unknown_track_is_404_and_unsatisfiable_range_is_416() {
    let mut h = Harness::new(40_000, 20_000, true);

    let ghost_token = grant::issue_v2("user-1", "ghost-track", "play-g", 1, EXP_UNIX, SECRET);
    let ghost_request = PlayRouteRequest {
        track_id: "ghost-track".to_string(),
        token: Some(ghost_token),
        range: None,
        trace_id: None,
    };
    let refusal = expect_refusal(serve_play(
        &mut h.pool,
        &mut h.cache,
        &mut h.meter,
        SECRET,
        &h.manifests,
        h.server.addr,
        NOW_UNIX,
        &ghost_request,
    ));
    assert_eq!((refusal.status, refusal.api_error.code), (404, "not_found"));
    assert_eq!(h.server.connects(), 0, "an unknown track never dials");

    let refusal = expect_refusal(h.request(Some(grant_v2("play-416")), Some("bytes=99999999-")));
    assert_eq!(
        (refusal.status, refusal.api_error.code),
        (416, "range_not_satisfiable")
    );
    assert!(h.sink.0.is_empty(), "no interval for refused responses");
}

#[test]
fn a_route_interval_round_trips_through_the_pool_do_boundary() {
    let mut h = Harness::new(40_000, 20_000, true);
    let serve = expect_serve(h.request(Some(grant_v2("play-rt")), Some("bytes=0-99")));
    let body = h.serve_to_completion(serve);
    assert_eq!(body, &h.plain[..100]);

    let emitted = h.sink.0.clone();
    assert_eq!(emitted, vec![interval("play-rt", 0, 100)]);
    let wire = serde_json::to_vec(&emitted[0]).expect("serialize report");
    let parsed = parse_delivered_report(&wire, "user-1").expect("owner accepts its own report");
    assert_eq!(parsed, emitted[0]);
    assert_eq!(serve_delivered("user-1", &wire).status, 202);
    assert_eq!(
        serve_delivered("user-2", &wire).status,
        400,
        "a report naming another user is refused at the boundary"
    );
}

#[test]
fn the_route_never_enqueues_speculative_prefetch() {
    let mut h = Harness::new(40_000, 20_000, true);
    let serve = expect_serve(h.request(Some(grant_v2("play-np")), None));
    let _ = h.serve_to_completion(serve);
    assert!(
        !h.meter.has_kind(MeterKind::PrefetchEnqueued),
        "M1: delivered-bytes metering forbids next-track preload (up_next is always None)"
    );
}
