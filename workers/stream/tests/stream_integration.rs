//! End-to-end play path against a synthetic NNTP server (std-only):
//! grant → pool → fetch → yEnc → decrypt → Range stream → R2 writeback.
//!
//! Acceptance probes: a warm pool performs no per-play reconnect, even across
//! two *uncached* tracks (the server counts connections); the miss path
//! streams segment by segment; `Range` yields `206` on hits and misses; a
//! gone article fails one play without quarantining the pool, while a
//! provider outage does quarantine and recovers; TTFB is measured on both
//! paths (targets documented in `workers/stream/README.md`).
//!
//! S10: a mid-track seek on a cold cache with per-segment byte ranges fetches
//! only the covering segment (a single BODY); without ranges the prefix walk
//! is preserved, and both paths serve byte-identical output.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::{MemoryMeter, MeterKind};
use nntune_stream::play::{play, play_with_timeouts, PlayError, PlayInput, PlayStream, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"stream-integration-secret";
const NOW_UNIX: u64 = 1_700_000_000;
// Within the grant TTL cap (`MAX_GRANT_TTL` = 900 s): play-path fixtures must
// carry grants `verify` accepts at `NOW_UNIX`, so the expiry sits at the cap.
const EXP_UNIX: u64 = NOW_UNIX + 900;
const CIPHER_KEY: u8 = 0x5A;

type Fixture = (Vec<u8>, Vec<String>, HashMap<String, Vec<u8>>);

/// Deterministic pseudo-audio covering every byte value (exercises yEnc
/// escapes), posted the way Usenet does it: one yEnc *multipart* article per
/// `seg_len` bytes (`=ybegin part= total=`, `=ypart`, `pcrc32=`; the
/// whole-file `crc32=` only on the last part).
fn fixture_track(tag: &str, len: usize, seg_len: usize) -> Fixture {
    let mut plain = Vec::with_capacity(len);
    for i in 0..len {
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

fn merge(mut a: HashMap<String, Vec<u8>>, b: HashMap<String, Vec<u8>>) -> HashMap<String, Vec<u8>> {
    a.extend(b);
    a
}

/// Minimal NNTP server: greeting + `BODY <mid>` with dot-stuffing.
struct FakeNntp {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    connects: Arc<AtomicUsize>,
    bodies: Arc<AtomicUsize>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeNntp {
    fn start(articles: HashMap<String, Vec<u8>>) -> Self {
        Self::start_with_options(articles, usize::MAX, None)
    }

    /// Start with one BODY ordinal receiving an NNTP status instead of an article.
    fn start_rejecting_body(
        articles: HashMap<String, Vec<u8>>,
        body_ordinal: usize,
        reply: &'static str,
    ) -> Self {
        Self::start_with_options(articles, usize::MAX, Some((body_ordinal, reply)))
    }

    /// A hung upstream: greets, reads each BODY command (counted), then never
    /// replies, so the client's read timeout — not the test — ends the play.
    fn start_hung() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind hung NNTP");
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
                // One thread per hung connection: holding the reply must not
                // block the accept loop (or test shutdown).
                let bodies_cc = Arc::clone(&bodies_c);
                let stop_cc = Arc::clone(&stop_c);
                thread::spawn(move || Self::serve_hung(stream, &bodies_cc, &stop_cc));
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

    /// Like `start`, but the server hangs up after `close_after` BODY
    /// commands on a connection (a provider reaping idle/busy sessions).
    fn start_closing_after(articles: HashMap<String, Vec<u8>>, close_after: usize) -> Self {
        Self::start_with_options(articles, close_after, None)
    }

    fn start_with_options(
        articles: HashMap<String, Vec<u8>>,
        close_after: usize,
        reject_body: Option<(usize, &'static str)>,
    ) -> Self {
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
                Self::serve(stream, &articles, close_after, reject_body, &bodies_c);
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

    fn serve(
        stream: TcpStream,
        articles: &HashMap<String, Vec<u8>>,
        close_after: usize,
        reject_body: Option<(usize, &'static str)>,
        bodies: &AtomicUsize,
    ) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone conn"));
        let mut writer = stream;
        let _ = writer.write_all(b"200 synthetic NNTP ready\r\n");
        let mut line = Vec::new();
        let mut served = 0usize;
        loop {
            if served >= close_after {
                break;
            }
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            while line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
                line.pop();
            }
            let cmd = String::from_utf8_lossy(&line).into_owned();
            if let Some(mid) = cmd
                .strip_prefix("BODY ")
                .map(|s| s.trim().trim_matches(|c| c == '<' || c == '>'))
            {
                served += 1;
                let body_ordinal = bodies.fetch_add(1, Ordering::SeqCst) + 1;
                if let Some((reject_ordinal, reply)) = reject_body {
                    if body_ordinal == reject_ordinal {
                        let _ = writer.write_all(reply.as_bytes());
                        continue;
                    }
                }
                let full_mid = format!("<{mid}>");
                match articles.get(mid) {
                    Some(article) => {
                        let _ =
                            writer.write_all(format!("222 0 {full_mid} follows\r\n").as_bytes());
                        for raw_line in article.split(|&b| b == b'\n') {
                            // Normalize to CRLF; strip a pre-existing CR first.
                            let text_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
                            // Dot-stuff per RFC 3977 §3.1.
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

    /// Greet, count each command line, and never reply: every client read
    /// blocks until its own read timeout fires. The hold loop polls `stop`
    /// so test shutdown never waits out a sleep.
    fn serve_hung(stream: TcpStream, bodies: &AtomicUsize, stop: &AtomicBool) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone conn"));
        let mut writer = stream;
        let _ = writer.write_all(b"200 synthetic NNTP ready\r\n");
        let _ = writer.flush();
        // Read the first command (counted) and never reply: the client's
        // read timeout ends the play. Each hung play dials fresh, so one
        // command per connection is the whole interaction. Hold the
        // connection open (a truly hung provider) until shutdown.
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                bodies.fetch_add(1, Ordering::SeqCst);
            }
        }
        while !stop.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn connects(&self) -> usize {
        self.connects.load(Ordering::SeqCst)
    }

    /// BODY commands received so far.
    fn bodies(&self) -> usize {
        self.bodies.load(Ordering::SeqCst)
    }
}

impl Drop for FakeNntp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop promptly.
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn drain_to_vec(mut stream: PlayStream) -> (Vec<u8>, PlayStream) {
    let mut body = Vec::new();
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    (body, stream)
}

#[allow(clippy::too_many_arguments)]
fn input<'a>(
    token: &'a str,
    track_id: &'a str,
    range: Option<&'a str>,
    server: &FakeNntp,
    segments: &'a [String],
    up_next: Option<&'a str>,
) -> PlayInput<'a> {
    PlayInput {
        token,
        track_id,
        range,
        server_addr: server.addr,
        segments,
        segment_ranges: None,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next,
        trace_id: None,
    }
}

fn grant_for(track: &str) -> String {
    grant::issue("user-1", track, EXP_UNIX, SECRET)
}

/// Per-segment plaintext byte ranges for a [`fixture_track`] of `len` bytes
/// in `seg_len`-byte articles: `[(0, seg_len), (seg_len, 2*seg_len), ...]`
/// with the last end clamped to `len`. This is the S9 NZB-metadata shape the
/// S10 play path consumes (injected via fixture; wire-up to the real S9
/// envelope is a later slice).
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

/// Like [`input`], but carries the S10 per-segment byte ranges: each
/// `(start, end_exclusive)` plaintext offset in `segments` order and length.
/// `None` means ranges are unknown and the play path must keep the current
/// prefix-walk fallback.
#[allow(clippy::too_many_arguments)]
fn input_with_ranges<'a>(
    token: &'a str,
    track_id: &'a str,
    range: Option<&'a str>,
    server: &FakeNntp,
    segments: &'a [String],
    segment_ranges: Option<&'a [(u64, u64)]>,
    up_next: Option<&'a str>,
) -> PlayInput<'a> {
    PlayInput {
        trace_id: None,
        token,
        track_id,
        range,
        server_addr: server.addr,
        segments,
        segment_ranges,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next,
    }
}

#[test]
fn stale_auth_midstream_is_dropped_on_finalize_and_abort() {
    let (_, segments, articles) = fixture_track("auth-midstream", 40_000, 20_000);
    let server = FakeNntp::start_rejecting_body(articles, 2, "481 authentication rejected\r\n");
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let token = grant_for("track-auth-finalize");
    let inp = input(
        &token,
        "track-auth-finalize",
        None,
        &server,
        &segments,
        None,
    );
    let response = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("first segment");
    assert_eq!(
        response.stream.finalize(&mut pool, &mut cache, &mut meter),
        Err(PlayError::ProviderAuthExpired)
    );
    assert_eq!((pool.idle(), pool.in_use()), (0, 0));
    assert!(!pool.quarantined());

    drop(server);
    let (_, abort_segments, abort_articles) = fixture_track("auth-abort", 40_000, 20_000);
    let abort_server =
        FakeNntp::start_rejecting_body(abort_articles, 2, "481 authentication rejected\r\n");
    let token = grant_for("track-auth-abort");
    let input = input(
        &token,
        "track-auth-abort",
        None,
        &abort_server,
        &abort_segments,
        None,
    );
    let response = play(&mut pool, &mut cache, &mut meter, SECRET, &input).expect("first segment");
    let mut stream = response.stream;
    assert!(stream.next().is_some());
    while stream.next().is_some() {}
    assert_eq!(
        abort_server.bodies(),
        2,
        "second BODY must fail with stale auth"
    );
    stream.abort(&mut pool);
    assert_eq!((pool.idle(), pool.in_use()), (0, 0));
    assert!(!pool.quarantined());
}

#[test]
fn stale_auth_on_warm_connection_redials_before_next_play() {
    let (plain, segments, articles) = fixture_track("auth-redial", 20_000, 20_000);
    let server = FakeNntp::start_rejecting_body(articles, 2, "480 authentication required\r\n");
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant_for("track-first");
    let first_input = input(&token, "track-first", None, &server, &segments, None);
    let response =
        play(&mut pool, &mut cache, &mut meter, SECRET, &first_input).expect("first play");
    let (body, stream) = drain_to_vec(response.stream);
    assert_eq!(body, plain);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("first finalize");
    assert_eq!(pool.idle(), 1);

    let token = grant_for("track-second");
    let input = input(&token, "track-second", None, &server, &segments, None);
    let response = play(&mut pool, &mut cache, &mut meter, SECRET, &input).expect("warm retry");
    let (body, stream) = drain_to_vec(response.stream);
    assert_eq!(body, plain);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("retry finalize");
    assert_eq!(server.connects(), 2);
    assert_eq!(pool.idle(), 1);
    assert!(!pool.quarantined());
}

#[test]
fn warm_pool_serves_two_uncached_tracks_on_one_connection() {
    let (plain_a, segs_a, arts_a) = fixture_track("a", 40_000, 20_000);
    let (plain_b, segs_b, arts_b) = fixture_track("b", 30_000, 10_000);
    let server = FakeNntp::start(merge(arts_a, arts_b));
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    for (track, segs, plain) in [
        ("track-a", &segs_a, &plain_a),
        ("track-b", &segs_b, &plain_b),
    ] {
        let token = grant_for(track);
        let inp = input(&token, track, None, &server, segs, None);
        let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("miss play");
        assert!(!resp.cache_hit && resp.stream.is_writeback());
        let (body, stream) = drain_to_vec(resp.stream);
        assert_eq!(&body, plain, "{track}: miss path must reproduce plaintext");
        stream
            .finalize(&mut pool, &mut cache, &mut meter)
            .expect("finalize");
    }
    assert_eq!(
        server.connects(),
        1,
        "two uncached plays share one NNTP connection"
    );
    assert_eq!((pool.connects(), pool.reuses()), (1, 1));
    assert_eq!((pool.idle(), pool.in_use()), (1, 0));
}

#[test]
fn miss_streams_segment_by_segment_and_hit_needs_no_dial() {
    let (plain, segments, articles) = fixture_track("a", 60_000, 20_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant_for("track-a");
    let inp = input(&token, "track-a", None, &server, &segments, Some("track-b"));
    let t0 = Instant::now();
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("miss play");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body_len as usize, plain.len());
    let mut stream = resp.stream;
    let first = stream.next().expect("first chunk");
    let ttfb_miss = t0.elapsed();
    assert_eq!(
        server.bodies(),
        1,
        "first byte arrives after ONE of three segments, not the whole track"
    );
    let mut body = first;
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body, plain);
    assert_eq!(server.bodies(), 3);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    println!("miss-path TTFB (dial + first segment + decode): {ttfb_miss:?}");

    // Hit: served from cache, nothing dialed.
    let token2 = grant_for("track-a");
    let inp2 = input(&token2, "track-a", None, &server, &segments, None);
    let t0 = Instant::now();
    let resp2 = play(&mut pool, &mut cache, &mut meter, SECRET, &inp2).expect("hit play");
    assert!(resp2.cache_hit && !resp2.stream.is_writeback());
    let mut stream2 = resp2.stream;
    let first2 = stream2.next().expect("first cached chunk");
    let ttfb_hit = t0.elapsed();
    let mut body2 = first2;
    for chunk in stream2.by_ref() {
        body2.extend_from_slice(&chunk);
    }
    assert_eq!(body2, plain);
    stream2
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("hit finalize");
    assert_eq!((server.connects(), server.bodies()), (1, 3));
    println!("hit-path TTFB (authorize+cache to first byte): {ttfb_hit:?}");
    assert!(
        ttfb_hit < Duration::from_millis(100),
        "local hit TTFB budget: {ttfb_hit:?}"
    );

    for kind in [
        MeterKind::PlayStart,
        MeterKind::CacheMiss,
        MeterKind::CacheHit,
        MeterKind::BytesServed,
        MeterKind::PrefetchEnqueued,
    ] {
        assert!(meter.has_kind(kind), "missing {kind:?}");
    }
    let served: u64 = meter
        .events
        .iter()
        .filter(|e| e.kind == MeterKind::BytesServed)
        .map(|e| e.bytes)
        .sum();
    assert_eq!(served as usize, 2 * plain.len());
}

#[test]
fn prefetch_is_not_enqueued_when_it_would_take_the_last_connection() {
    let (_, segments, articles) = fixture_track("a", 20_000, 10_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(1);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let token = grant_for("track-a");
    let inp = input(&token, "track-a", None, &server, &segments, Some("track-b"));
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("play");
    let (_, stream) = drain_to_vec(resp.stream);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    assert!(!meter.has_kind(MeterKind::PrefetchEnqueued));
}

#[test]
fn range_on_a_hit_returns_206_and_unsatisfiable_is_416() {
    let (plain, segments, articles) = fixture_track("a", 40_000, 20_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant_for("track-a");
    let inp = input(&token, "track-a", None, &server, &segments, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("seed play");
    let (body, stream) = drain_to_vec(resp.stream);
    assert_eq!(body, plain);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");

    let token2 = grant_for("track-a");
    let inp2 = input(
        &token2,
        "track-a",
        Some("bytes=0-99"),
        &server,
        &segments,
        None,
    );
    let resp2 = play(&mut pool, &mut cache, &mut meter, SECRET, &inp2).expect("range play");
    assert_eq!(resp2.status, 206);
    assert_eq!(
        resp2.content_range.as_deref(),
        Some(format!("bytes 0-99/{}", plain.len()).as_str())
    );
    let (slice, _) = drain_to_vec(resp2.stream);
    assert_eq!(slice, &plain[..100]);

    let token3 = grant_for("track-a");
    let inp3 = input(
        &token3,
        "track-a",
        Some("bytes=99999999-"),
        &server,
        &segments,
        None,
    );
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &inp3),
        Err(PlayError::RangeUnsatisfiable)
    ));
}

#[test]
fn seek_on_a_cold_cache_fetches_the_prefix_before_the_first_byte() {
    // Prefix-walk fallback (see README TTFB notes): without NZB byte ranges
    // the play path cannot know which article covers a mid-track offset, so
    // it walks segments in order. A seek's TTFB costs the whole prefix. S10
    // preserves this fallback when ranges are absent.
    let (plain, segments, articles) = fixture_track("a", 60_000, 20_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant_for("track-a");
    let inp = input(
        &token,
        "track-a",
        Some("bytes=45000-45099"),
        &server,
        &segments,
        None,
    );
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("seek miss");
    assert_eq!(resp.status, 206);
    let mut stream = resp.stream;
    let first = stream.next().expect("first chunk");
    assert_eq!(
        server.bodies(),
        3,
        "seek to segment 2 walks segments 0-2 first"
    );
    let mut body = first;
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body, &plain[45_000..45_100]);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
}

#[test]
fn range_on_a_miss_serves_the_slice_and_still_caches_the_whole_track() {
    let (plain, segments, articles) = fixture_track("a", 60_000, 20_000);
    let (plain_b, segs_b, arts_b) = fixture_track("b", 30_000, 10_000);
    let server = FakeNntp::start(merge(articles, arts_b));
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // Seek into the middle segment on a cold cache.
    let token = grant_for("track-a");
    let inp = input(
        &token,
        "track-a",
        Some("bytes=25000-25099"),
        &server,
        &segments,
        None,
    );
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("range miss");
    assert_eq!(resp.status, 206);
    assert_eq!(
        resp.content_range.as_deref(),
        Some(format!("bytes 25000-25099/{}", plain.len()).as_str())
    );
    let (slice, stream) = drain_to_vec(resp.stream);
    assert_eq!(slice, &plain[25_000..25_100]);
    assert!(
        cache.get_track("track-a").is_none(),
        "not cached before finalize"
    );
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize drains the rest");
    assert_eq!(
        cache.get_track("track-a").as_deref(),
        Some(plain.as_slice()),
        "the whole track is cached even though the client wanted 100 bytes"
    );

    // Suffix range on a cold cache resolves against the declared file size.
    let token_b = grant_for("track-b");
    let inp_b = input(
        &token_b,
        "track-b",
        Some("bytes=-50"),
        &server,
        &segs_b,
        None,
    );
    let resp_b = play(&mut pool, &mut cache, &mut meter, SECRET, &inp_b).expect("suffix miss");
    assert_eq!(resp_b.status, 206);
    let (tail, stream_b) = drain_to_vec(resp_b.stream);
    assert_eq!(tail, &plain_b[plain_b.len() - 50..]);
    stream_b.abort(&mut pool);
    assert!(cache.get_track("track-b").is_none(), "abort must not cache");
    assert_eq!(pool.in_use(), 0, "abort returns the lease");
}

#[test]
fn a_segment_id_with_crlf_never_reaches_the_provider() {
    let (_, _, articles) = fixture_track("ok", 20_000, 10_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // NZB-supplied id smuggling a second NNTP command.
    let evil = "x>\r\nBODY <injected@article>".to_string();
    let segs = [evil];
    let token = grant_for("track-evil");
    let inp = input(&token, "track-evil", None, &server, &segs, None);
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &inp),
        Err(PlayError::InvalidSegment(_))
    ));
    assert_eq!(server.bodies(), 0, "nothing may be sent for a bad id");
    assert_eq!(server.connects(), 0, "no dial for a bad id");
    assert!(!pool.quarantined(), "bad metadata is not an outage");
    assert_eq!(pool.in_use(), 0);
}

#[test]
fn dropping_a_stream_frees_its_slot_and_still_meters_served_bytes() {
    let (_plain_a, segs_a, arts_a) = fixture_track("a", 40_000, 20_000);
    let (plain_b, segs_b, arts_b) = fixture_track("b", 20_000, 10_000);
    let server = FakeNntp::start(merge(arts_a, arts_b));
    let mut pool: StreamPool = Pool::new(1);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // Client disconnects mid-track: no finalize, no abort.
    let token = grant_for("track-a");
    let inp = input(&token, "track-a", None, &server, &segs_a, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("miss play");
    let mut stream = resp.stream;
    let first = stream.next().expect("first chunk");
    assert!(!first.is_empty());
    let served_a = first.len() as u64;
    drop(stream);
    assert_eq!(pool.in_use(), 0, "a dropped stream must free its slot");
    assert_eq!(
        pool.idle(),
        0,
        "the half-read connection is dropped, never re-parked"
    );

    // The freed slot serves the next play, which also flushes the orphaned
    // metering from the dropped stream.
    let token_b = grant_for("track-b");
    let inp_b = input(&token_b, "track-b", None, &server, &segs_b, None);
    let resp_b =
        play(&mut pool, &mut cache, &mut meter, SECRET, &inp_b).expect("slot freed by drop");
    let (body_b, stream_b) = drain_to_vec(resp_b.stream);
    assert_eq!(body_b, plain_b);
    stream_b
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    let orphaned: u64 = meter
        .events
        .iter()
        .filter(|e| e.kind == MeterKind::BytesServed && e.track_id == "track-a")
        .map(|e| e.bytes)
        .sum();
    assert_eq!(orphaned, served_a, "dropped play must still be metered");
}

#[test]
fn aborted_miss_stream_meters_bytes_already_served() {
    let (_plain_a, segs_a, arts_a) = fixture_track("a", 40_000, 20_000);
    let (plain_b, segs_b, arts_b) = fixture_track("b", 20_000, 10_000);
    let server = FakeNntp::start(merge(arts_a, arts_b));
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // Client disconnects mid-track via the documented path: no finalize.
    let token = grant_for("track-a");
    let inp = input(&token, "track-a", None, &server, &segs_a, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("miss play");
    let mut stream = resp.stream;
    let first = stream.next().expect("first chunk");
    let served_a = first.len() as u64;
    assert!(served_a > 0);
    stream.abort(&mut pool);
    assert_eq!(pool.in_use(), 0, "abort returns the lease");
    assert_eq!(pool.idle(), 1, "healthy connection is kept warm");

    // The next play flushes the orphaned metering from the aborted stream.
    let token_b = grant_for("track-b");
    let inp_b = input(&token_b, "track-b", None, &server, &segs_b, None);
    let resp_b = play(&mut pool, &mut cache, &mut meter, SECRET, &inp_b).expect("next play");
    let (body_b, stream_b) = drain_to_vec(resp_b.stream);
    assert_eq!(body_b, plain_b);
    stream_b
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    let orphaned: u64 = meter
        .events
        .iter()
        .filter(|e| e.kind == MeterKind::BytesServed && e.track_id == "track-a")
        .map(|e| e.bytes)
        .sum();
    assert_eq!(orphaned, served_a, "aborted play must still be metered");
    assert!(cache.get_track("track-a").is_none(), "abort must not cache");
}

#[test]
fn dropped_and_aborted_hit_streams_meter_served_bytes() {
    let (plain, segments, articles) = fixture_track("a", 40_000, 20_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // Seed the cache: a full miss play meters the whole track on finalize.
    let token = grant_for("track-a");
    let inp = input(&token, "track-a", None, &server, &segments, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("seed play");
    let (body, stream) = drain_to_vec(resp.stream);
    assert_eq!(body, plain);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    let full = plain.len() as u64;

    // Hit stream dropped after one chunk: no lease, no finalize, no abort.
    let token2 = grant_for("track-a");
    let inp2 = input(&token2, "track-a", None, &server, &segments, None);
    let resp2 = play(&mut pool, &mut cache, &mut meter, SECRET, &inp2).expect("hit play");
    assert!(resp2.cache_hit);
    let mut dropped = resp2.stream;
    let c1 = dropped.next().expect("first hit chunk").len() as u64;
    drop(dropped);

    // Hit stream aborted after one chunk.
    let token3 = grant_for("track-a");
    let inp3 = input(&token3, "track-a", None, &server, &segments, None);
    let resp3 = play(&mut pool, &mut cache, &mut meter, SECRET, &inp3).expect("hit play");
    let mut aborted = resp3.stream;
    let c2 = aborted.next().expect("first hit chunk").len() as u64;
    aborted.abort(&mut pool);

    // A final full hit play flushes both orphaned records.
    let token4 = grant_for("track-a");
    let inp4 = input(&token4, "track-a", None, &server, &segments, None);
    let resp4 = play(&mut pool, &mut cache, &mut meter, SECRET, &inp4).expect("hit play");
    let (body4, stream4) = drain_to_vec(resp4.stream);
    assert_eq!(body4, plain);
    stream4
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    let total: u64 = meter
        .events
        .iter()
        .filter(|e| e.kind == MeterKind::BytesServed && e.track_id == "track-a")
        .map(|e| e.bytes)
        .sum();
    assert_eq!(
        total,
        full + c1 + c2 + full,
        "seed + dropped-hit + aborted-hit + final hit must all be metered"
    );
    assert_eq!(pool.in_use(), 0);
}

#[test]
fn a_gone_article_fails_that_play_but_never_quarantines_the_pool() {
    let (_, segs_gone, _) = fixture_track("gone", 20_000, 10_000);
    let (plain_ok, segs_ok, arts_ok) = fixture_track("ok", 20_000, 10_000);
    // "gone" articles are absent from the server: 430.
    let server = FakeNntp::start(arts_ok);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    for _ in 0..4 {
        let token = grant_for("track-gone");
        let inp = input(&token, "track-gone", None, &server, &segs_gone, None);
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, SECRET, &inp),
            Err(PlayError::ArticleMissing(_))
        ));
        assert!(!pool.quarantined(), "430 says nothing about the provider");
    }
    assert!(!meter.has_kind(MeterKind::ProviderError));
    assert_eq!(pool.in_use(), 0);

    let token = grant_for("track-ok");
    let inp = input(&token, "track-ok", None, &server, &segs_ok, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("healthy track");
    let (body, stream) = drain_to_vec(resp.stream);
    assert_eq!(body, plain_ok);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    assert_eq!(server.connects(), 1, "the same connection served all five");
}

#[test]
fn an_article_that_vanishes_mid_track_fails_the_stream_without_caching() {
    let (_, segments, mut articles) = fixture_track("a", 30_000, 10_000);
    articles.remove(&segments[1]);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant_for("track-a");
    let inp = input(&token, "track-a", None, &server, &segments, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("first segment ok");
    let (partial, stream) = drain_to_vec(resp.stream);
    assert_eq!(partial.len(), 10_000, "only segment 0 was served");
    assert!(matches!(stream.error(), Some(PlayError::ArticleMissing(_))));
    assert!(matches!(
        stream.finalize(&mut pool, &mut cache, &mut meter),
        Err(PlayError::ArticleMissing(_))
    ));
    assert!(
        cache.get_track("track-a").is_none(),
        "no partial cache entry"
    );
    assert!(!pool.quarantined());
    assert_eq!(
        (pool.in_use(), pool.idle()),
        (0, 1),
        "healthy connection kept"
    );
}

#[test]
fn a_connection_the_provider_closed_is_redialed_transparently() {
    let (plain_a, segs_a, arts_a) = fixture_track("a", 20_000, 10_000);
    let (plain_b, segs_b, arts_b) = fixture_track("b", 20_000, 10_000);
    // The server hangs up after 2 BODYs: exactly one track per connection.
    let server = FakeNntp::start_closing_after(merge(arts_a, arts_b), 2);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    for (track, segs, plain) in [
        ("track-a", &segs_a, &plain_a),
        ("track-b", &segs_b, &plain_b),
    ] {
        let token = grant_for(track);
        let inp = input(&token, track, None, &server, segs, None);
        let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("play");
        let (body, stream) = drain_to_vec(resp.stream);
        assert_eq!(&body, plain);
        stream
            .finalize(&mut pool, &mut cache, &mut meter)
            .expect("finalize");
    }
    assert_eq!(server.connects(), 2, "stale warm connection was redialed");
    assert!(
        !pool.quarantined(),
        "a stale pooled connection is not an outage"
    );
}

#[test]
fn ten_sequential_range_seeks_abort_without_wedging_the_pool() {
    let (plain, segments, articles) = fixture_track("a", 40_000, 10_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // Uncached track: each seek is a live miss holding a pool lease. A cache
    // hit holds no lease, so it could not prove that abort releases one.
    // Ten rapid seeks: each reads one chunk, then aborts (the client
    // seeking again). No seek may wedge the pool or the track.
    for i in 0..10 {
        let start = (i * 1000) as u64;
        let range = format!("bytes={}-{}", start, start + 99);
        let token = grant_for("track-a");
        let inp = input(&token, "track-a", Some(&range), &server, &segments, None);
        let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("seek play");
        assert_eq!(resp.status, 206);
        assert!(!resp.cache_hit, "seek {i}: must be a live miss");
        let mut stream = resp.stream;
        let chunk = stream.next().expect("first seek chunk");
        assert_eq!(chunk.len(), 100);
        assert_eq!(pool.in_use(), 1, "seek {i}: live seek holds one lease");
        stream.abort(&mut pool);
        assert_eq!(pool.in_use(), 0, "seek {i}: abort must free the pool");
    }
    assert!(
        cache.get_track("track-a").is_none(),
        "aborted seeks must not cache"
    );

    // The player still serves after the storm.
    let token = grant_for("track-a");
    let inp = input(&token, "track-a", None, &server, &segments, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("post-storm play");
    let (body, stream) = drain_to_vec(resp.stream);
    assert_eq!(body, plain);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    assert_eq!(pool.in_use(), 0);
}

#[test]
fn hung_upstream_trips_the_breaker_and_serves_503_then_recovers() {
    use nntune_stream::nntp::UpstreamTimeouts;
    use nntune_stream::pool::CircuitState;
    use nntune_stream::pool::{CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_OPEN_MS};

    let (plain, segments, articles) = fixture_track("a", 40_000, 20_000);
    let healthy = FakeNntp::start(articles);
    let hung = FakeNntp::start_hung();
    let quick = UpstreamTimeouts::new(Duration::from_millis(200), Duration::from_millis(200));
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // Hung plays fail fast (timeout + margin, never a hang) and count toward
    // the breaker without opening it before the threshold.
    for i in 1..=CIRCUIT_FAILURE_THRESHOLD {
        let token = grant_for("track-a");
        let inp = input(&token, "track-a", None, &hung, &segments, None);
        let t0 = Instant::now();
        assert!(
            matches!(
                play_with_timeouts(&mut pool, &mut cache, &mut meter, SECRET, &inp, quick),
                Err(PlayError::Upstream(_))
            ),
            "hung play {i} must surface as an upstream failure"
        );
        assert!(
            t0.elapsed() < quick.read + Duration::from_secs(5),
            "hung play {i} must stay within timeout + margin"
        );
        assert_eq!(pool.in_use(), 0, "failed play frees its slot");
        if i < CIRCUIT_FAILURE_THRESHOLD {
            assert!(
                !pool.quarantined(),
                "below threshold the breaker stays closed"
            );
        }
    }
    assert!(pool.quarantined(), "threshold hung plays open the breaker");
    assert_eq!(pool.circuit_state(), CircuitState::Open);

    // Open breaker: rejected without touching the hung upstream, mapped to
    // 503 with a Retry-After for the Worker shim.
    let hung_bodies = hung.bodies();
    let token = grant_for("track-a");
    let inp = input(&token, "track-a", None, &hung, &segments, None);
    let t0 = Instant::now();
    let err = match play_with_timeouts(&mut pool, &mut cache, &mut meter, SECRET, &inp, quick) {
        Err(e) => e,
        Ok(_) => panic!("open breaker must reject"),
    };
    assert!(t0.elapsed() < Duration::from_secs(2), "no dial, no wait");
    match err {
        PlayError::PoolQuarantined { retry_after_ms } => {
            assert_eq!(retry_after_ms, CIRCUIT_OPEN_MS);
            assert_eq!(err.http_status(), 503);
            assert_eq!(
                err.retry_after_header().as_deref(),
                Some("30"),
                "documented open interval surfaces as Retry-After"
            );
        }
        other => panic!("expected PoolQuarantined, got {other:?}"),
    }
    assert_eq!(
        hung.bodies(),
        hung_bodies,
        "open breaker must not dial the hung upstream"
    );

    // Open interval elapses: a half-open probe against the recovered upstream
    // closes the breaker and serves the track.
    pool.advance(CIRCUIT_OPEN_MS);
    let token2 = grant_for("track-a");
    let inp2 = input(&token2, "track-a", None, &healthy, &segments, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp2).expect("probe play");
    let (body, stream) = drain_to_vec(resp.stream);
    assert_eq!(body, plain);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    assert!(!pool.quarantined(), "probe success closes the breaker");
}

#[test]
fn provider_outage_quarantines_then_recovers() {
    let (plain, segments, articles) = fixture_track("a", 40_000, 20_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // Point at a dead port: repeated dial failures trip the breaker at its
    // documented threshold, then quarantine the pool.
    use nntune_stream::pool::CIRCUIT_FAILURE_THRESHOLD;
    let dead: SocketAddr = "127.0.0.1:1".parse().unwrap();
    for i in 1..=CIRCUIT_FAILURE_THRESHOLD {
        let token = grant_for("track-a");
        let mut inp = input(&token, "track-a", None, &server, &segments, None);
        inp.server_addr = dead;
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, SECRET, &inp),
            Err(PlayError::Upstream(_))
        ));
        assert_eq!(
            pool.quarantined(),
            i == CIRCUIT_FAILURE_THRESHOLD,
            "breaker opens exactly at the threshold"
        );
    }
    assert!(meter.has_kind(MeterKind::ProviderError));
    assert_eq!(pool.in_use(), 0);

    // Still quarantined: the next play is rejected without dialing out.
    let token2 = grant_for("track-a");
    let inp2 = input(&token2, "track-a", None, &server, &segments, None);
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &inp2),
        Err(PlayError::PoolQuarantined { .. })
    ));
    assert_eq!(server.connects(), 0, "quarantined pool must not dial out");

    // Backoff elapses: the pool recovers and the play succeeds.
    pool.advance(300_000);
    let token3 = grant_for("track-a");
    let inp3 = input(&token3, "track-a", None, &server, &segments, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp3).expect("recovered play");
    let (body, stream) = drain_to_vec(resp.stream);
    assert_eq!(body, plain);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    assert!(!pool.quarantined(), "success clears the quarantine");
}

#[test]
fn finalizing_without_serving_any_bytes_emits_no_bytes_served() {
    // Guards the `if self.served > 0` gate in `PlayStream::finalize`:
    // removing the guard emits a zero-byte `BytesServed` event, which would
    // widen the metering stream with billing-neutral noise. Finalizing an
    // unread stream must still drain and cache the track.
    let (plain, segments, articles) = fixture_track("a", 40_000, 20_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // Miss path: finalize without pulling a single chunk.
    let token = grant_for("track-a");
    let inp = input(&token, "track-a", None, &server, &segments, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("miss play");
    let body_len = resp.body_len;
    resp.stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("unread finalize");
    assert!(
        !meter
            .events
            .iter()
            .any(|e| e.kind == MeterKind::BytesServed),
        "unread miss finalize must not emit BytesServed (body_len was {body_len})"
    );
    assert_eq!(
        cache.get_track("track-a").as_deref(),
        Some(plain.as_slice()),
        "unread finalize still drains and caches the whole track"
    );

    // Hit path: the now-cached track, again finalized unread.
    let token2 = grant_for("track-a");
    let inp2 = input(&token2, "track-a", None, &server, &segments, None);
    let resp2 = play(&mut pool, &mut cache, &mut meter, SECRET, &inp2).expect("hit play");
    assert!(resp2.cache_hit);
    resp2
        .stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("unread hit finalize");
    assert!(
        !meter
            .events
            .iter()
            .any(|e| e.kind == MeterKind::BytesServed),
        "unread hit finalize must not emit BytesServed either"
    );
}

const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";
const TRACE_TS: &str = "2026-10-15T20:00:00.123Z";

fn traced_input<'a>(
    token: &'a str,
    track_id: &'a str,
    server: &FakeNntp,
    segments: &'a [String],
    trace_id: Option<&'a str>,
) -> PlayInput<'a> {
    let mut inp = input(token, track_id, None, server, segments, None);
    inp.trace_id = trace_id;
    inp
}

#[test]
fn trace_id_correlates_the_miss_path_end_to_end() {
    use nntune_stream::trace;
    let (plain, segments, articles) = fixture_track("a", 40_000, 20_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant_for("track-a");
    let inp = traced_input(&token, "track-a", &server, &segments, Some(TRACE));
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("miss play");
    assert!(!resp.cache_hit);
    assert_eq!(resp.trace_id.as_deref(), Some(TRACE));
    assert_eq!(resp.stream.trace_id(), Some(TRACE));
    let (body, stream) = drain_to_vec(resp.stream);
    assert_eq!(body, plain);
    assert_eq!(stream.trace_id(), Some(TRACE));
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    assert!(cache.get_track("track-a").is_some(), "miss must cache");

    // AE rows keep their exact shape (no trace field): correlation is proven
    // by the log envelope below sharing the same trace across events.
    for kind in [
        MeterKind::PlayStart,
        MeterKind::CacheMiss,
        MeterKind::BytesServed,
    ] {
        assert!(meter.has_kind(kind), "missing {kind:?}");
    }

    // The threaded trace renders a conforming six-key envelope for each
    // play-path event (grant authorize, pool lease, cache/miss, meter).
    for evt in ["play_start", "cache_miss", "bytes_served"] {
        let mut fields = trace::LogFields::new();
        fields.insert_str("track_id", "track-a").unwrap();
        let line = trace::render_log_line(TRACE_TS, "info", TRACE, evt, &fields)
            .expect("envelope must render");
        assert!(line.contains(&format!("\"trace_id\":\"{TRACE}\"")));
        assert!(line.contains(&format!("\"event\":\"{evt}\"")));
        assert!(line.contains("\"worker\":\"stream\""));
    }
}

#[test]
fn invalid_trace_on_a_miss_runs_untraced_without_touching_auth() {
    let (plain, segments, articles) = fixture_track("a", 20_000, 10_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant_for("track-a");
    let inp = traced_input(&token, "track-a", &server, &segments, Some("not-a-trace"));
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("miss play");
    assert_eq!(resp.trace_id, None, "invalid trace must be dropped");
    assert_eq!(resp.stream.trace_id(), None);
    let (body, stream) = drain_to_vec(resp.stream);
    assert_eq!(body, plain);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
}

#[test]
fn dropped_miss_stream_keeps_its_trace_in_the_orphan() {
    let (_plain_a, segs_a, arts_a) = fixture_track("a", 40_000, 20_000);
    let (plain_b, segs_b, arts_b) = fixture_track("b", 20_000, 10_000);
    let server = FakeNntp::start(merge(arts_a, arts_b));
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant_for("track-a");
    let inp = traced_input(&token, "track-a", &server, &segs_a, Some(TRACE));
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("miss play");
    let mut stream = resp.stream;
    assert!(stream.next().is_some());
    drop(stream);

    let token_b = grant_for("track-b");
    let inp_b = traced_input(&token_b, "track-b", &server, &segs_b, None);
    let resp_b = play(&mut pool, &mut cache, &mut meter, SECRET, &inp_b).expect("next play");
    let (body_b, stream_b) = drain_to_vec(resp_b.stream);
    assert_eq!(body_b, plain_b);
    stream_b
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    let orphaned: Vec<_> = meter
        .events
        .iter()
        .filter(|e| e.kind == MeterKind::BytesServed && e.track_id == "track-a")
        .collect();
    assert_eq!(
        orphaned.len(),
        1,
        "dropped traced play must still be metered"
    );
    assert!(orphaned[0].bytes > 0);
}

#[test]
fn corrupt_first_segment_fails_without_quarantine_and_keeps_connection_warm() {
    use nntune_stream::pool::{CircuitState, CIRCUIT_FAILURE_THRESHOLD};

    let (_, segments, mut articles) = fixture_track("corrupt-first", 20_000, 10_000);
    let (plain_ok, segs_ok, arts_ok) = fixture_track("ok", 20_000, 10_000);
    articles.insert(segments[0].clone(), b"this is not yEnc\r\n".to_vec());
    let mut both = arts_ok;
    both.extend(articles);
    let server = FakeNntp::start(both);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let dead: SocketAddr = "127.0.0.1:1".parse().unwrap();

    // Two provider failures first: a corrupt segment must not become the third.
    for _ in 0..(CIRCUIT_FAILURE_THRESHOLD - 1) {
        let token = grant_for("track-corrupt-first");
        let mut inp = input(
            &token,
            "track-corrupt-first",
            None,
            &server,
            &segments,
            None,
        );
        inp.server_addr = dead;
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, SECRET, &inp),
            Err(PlayError::Upstream(_))
        ));
    }
    assert!(!pool.quarantined());

    let token = grant_for("track-corrupt-first");
    let inp = input(
        &token,
        "track-corrupt-first",
        None,
        &server,
        &segments,
        None,
    );
    assert!(
        matches!(
            play(&mut pool, &mut cache, &mut meter, SECRET, &inp),
            Err(PlayError::Decode(_))
        ),
        "a corrupt first segment is a Decode, not an outage"
    );
    assert!(
        !pool.quarantined(),
        "a corrupt segment must not count toward the breaker"
    );
    assert_eq!(pool.circuit_state(), CircuitState::Closed);
    assert_eq!(
        (pool.in_use(), pool.idle()),
        (0, 1),
        "the healthy connection is kept warm after a Decode"
    );

    // The warm connection serves the next healthy track without redial.
    let token_ok = grant_for("track-ok");
    let inp_ok = input(&token_ok, "track-ok", None, &server, &segs_ok, None);
    let resp_ok = play(&mut pool, &mut cache, &mut meter, SECRET, &inp_ok).expect("healthy play");
    let (body_ok, stream_ok) = drain_to_vec(resp_ok.stream);
    assert_eq!(body_ok, plain_ok);
    stream_ok
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    assert_eq!(server.connects(), 1, "Decode kept the connection warm");
    assert_eq!((pool.connects(), pool.reuses()), (1, 1));
}

#[test]
fn corrupt_first_segment_as_half_open_probe_closes_the_breaker() {
    use nntune_stream::pool::{CircuitState, CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_OPEN_MS};

    let (_, segments, mut articles) = fixture_track("corrupt-probe", 20_000, 10_000);
    articles.insert(segments[0].clone(), b"corrupt\r\n".to_vec());
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let dead: SocketAddr = "127.0.0.1:1".parse().unwrap();

    for _ in 0..CIRCUIT_FAILURE_THRESHOLD {
        let token = grant_for("track-probe");
        let mut inp = input(&token, "track-probe", None, &server, &segments, None);
        inp.server_addr = dead;
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, SECRET, &inp),
            Err(PlayError::Upstream(_))
        ));
    }
    assert!(pool.quarantined());
    pool.advance(CIRCUIT_OPEN_MS);
    assert_eq!(pool.circuit_state(), CircuitState::HalfOpen);

    // A provider response — even a corrupt article — proves liveness and must
    // close a half-open probe instead of re-opening it.
    let token = grant_for("track-probe");
    let inp = input(&token, "track-probe", None, &server, &segments, None);
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &inp),
        Err(PlayError::Decode(_))
    ));
    assert_eq!(
        pool.circuit_state(),
        CircuitState::Closed,
        "a Decode probe response closes the breaker"
    );
    assert!(!pool.quarantined());
    assert_eq!(pool.idle(), 1, "probe connection kept warm");

    // Closed again: the next corrupt play is admitted (Decode, not 503).
    let token2 = grant_for("track-probe");
    let inp2 = input(&token2, "track-probe", None, &server, &segments, None);
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &inp2),
        Err(PlayError::Decode(_))
    ));
}

#[test]
fn corrupt_later_segment_fails_the_stream_without_quarantine() {
    use nntune_stream::pool::CIRCUIT_FAILURE_THRESHOLD;

    let (_, segments, mut articles) = fixture_track("corrupt-later", 30_000, 10_000);
    assert_eq!(segments.len(), 3);
    articles.insert(segments[1].clone(), b"garbage\r\n".to_vec());
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let dead: SocketAddr = "127.0.0.1:1".parse().unwrap();

    for _ in 0..(CIRCUIT_FAILURE_THRESHOLD - 1) {
        let token = grant_for("track-corrupt-later");
        let mut inp = input(
            &token,
            "track-corrupt-later",
            None,
            &server,
            &segments,
            None,
        );
        inp.server_addr = dead;
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, SECRET, &inp),
            Err(PlayError::Upstream(_))
        ));
    }

    let token = grant_for("track-corrupt-later");
    let inp = input(
        &token,
        "track-corrupt-later",
        None,
        &server,
        &segments,
        None,
    );
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("first segment ok");
    let (partial, stream) = drain_to_vec(resp.stream);
    assert_eq!(partial.len(), 10_000, "only segment 0 was served");
    assert!(
        matches!(stream.error(), Some(PlayError::Decode(_))),
        "a corrupt later segment is a Decode"
    );
    assert!(matches!(
        stream.finalize(&mut pool, &mut cache, &mut meter),
        Err(PlayError::Decode(_))
    ));
    assert!(
        !pool.quarantined(),
        "a corrupt later segment must not count toward the breaker"
    );
    assert_eq!(
        (pool.in_use(), pool.idle()),
        (0, 1),
        "the healthy connection is kept warm after a later Decode"
    );
    assert!(
        cache.get_track("track-corrupt-later").is_none(),
        "no partial cache entry"
    );
}

#[test]
fn clean_abort_clears_the_streak_after_first_segment() {
    use nntune_stream::pool::{CircuitState, CIRCUIT_FAILURE_THRESHOLD};

    let (_, segments, articles) = fixture_track("abort-clear", 20_000, 10_000);
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let dead: SocketAddr = "127.0.0.1:1".parse().unwrap();

    for _ in 0..(CIRCUIT_FAILURE_THRESHOLD - 1) {
        let token = grant_for("track-abort");
        let mut inp = input(&token, "track-abort", None, &server, &segments, None);
        inp.server_addr = dead;
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, SECRET, &inp),
            Err(PlayError::Upstream(_))
        ));
    }
    assert!(!pool.quarantined());

    // The first segment settles only a half-open probe, so it leaves the
    // closed-state streak alone; the clean abort (no failure recorded) is
    // what clears the streak.
    let token = grant_for("track-abort");
    let inp = input(&token, "track-abort", None, &server, &segments, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("first segment ok");
    let mut stream = resp.stream;
    let first = stream.next().expect("first chunk");
    assert!(!first.is_empty());
    stream.abort(&mut pool);
    assert_eq!(
        pool.circuit_state(),
        CircuitState::Closed,
        "a clean abort must clear the streak after the first segment"
    );
    assert_eq!((pool.in_use(), pool.idle()), (0, 1));
    assert!(
        cache.get_track("track-abort").is_none(),
        "abort must not cache"
    );

    // One more provider failure is only the first of a fresh streak.
    let token2 = grant_for("track-abort");
    let mut inp2 = input(&token2, "track-abort", None, &server, &segments, None);
    inp2.server_addr = dead;
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &inp2),
        Err(PlayError::Upstream(_))
    ));
    assert!(
        !pool.quarantined(),
        "2 failures + success/abort + 1 failure must not open the breaker"
    );
    assert_eq!(pool.circuit_state(), CircuitState::Closed);
}

#[test]
fn abort_after_a_failed_segment_records_the_provider_failure() {
    use nntune_stream::pool::{CircuitState, CIRCUIT_FAILURE_THRESHOLD};

    let (_, segments, articles) = fixture_track("abort-fail", 30_000, 10_000);
    assert_eq!(segments.len(), 3);
    // The provider serves the first segment, then hangs up mid-track.
    let server = FakeNntp::start_closing_after(articles, 1);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let dead: SocketAddr = "127.0.0.1:1".parse().unwrap();

    for _ in 0..(CIRCUIT_FAILURE_THRESHOLD - 1) {
        let token = grant_for("track-abort-fail");
        let mut inp = input(&token, "track-abort-fail", None, &server, &segments, None);
        inp.server_addr = dead;
        assert!(matches!(
            play(&mut pool, &mut cache, &mut meter, SECRET, &inp),
            Err(PlayError::Upstream(_))
        ));
    }
    assert!(!pool.quarantined());

    let token = grant_for("track-abort-fail");
    let inp = input(&token, "track-abort-fail", None, &server, &segments, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("first segment ok");
    let (partial, stream) = drain_to_vec(resp.stream);
    assert!(!partial.is_empty(), "segment 0 streams before the failure");
    assert!(
        matches!(stream.error(), Some(PlayError::Upstream(_))),
        "the dropped connection is a provider failure"
    );
    stream.abort(&mut pool);
    // The first segment no longer clears the pre-seeded streak, so the
    // aborted mid-track failure is the third consecutive provider failure
    // and opens the breaker immediately.
    assert!(
        pool.quarantined(),
        "aborting after a provider failure must count toward the breaker"
    );
    assert_eq!(pool.circuit_state(), CircuitState::Open);
    // Further plays fail fast without dialing the dead upstream.
    let token = grant_for("track-abort-fail");
    let mut inp = input(&token, "track-abort-fail", None, &server, &segments, None);
    inp.server_addr = dead;
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &inp),
        Err(PlayError::PoolQuarantined { .. })
    ));
    assert_eq!((pool.in_use(), pool.idle()), (0, 0));
    assert!(
        cache.get_track("track-abort-fail").is_none(),
        "abort must not cache"
    );
}

#[test]
fn repeated_mid_track_failures_open_the_breaker() {
    use nntune_stream::pool::{CircuitState, CIRCUIT_FAILURE_THRESHOLD};

    let (_, segments, articles) = fixture_track("mid-track", 30_000, 10_000);
    assert_eq!(segments.len(), 3);
    // The provider serves the first segment, then hangs up mid-track.
    let server = FakeNntp::start_closing_after(articles, 1);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    for i in 1..=CIRCUIT_FAILURE_THRESHOLD {
        let token = grant_for("track-mid");
        let inp = input(&token, "track-mid", None, &server, &segments, None);
        let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp)
            .expect("segment 0 serves before the mid-track failure");
        let (partial, stream) = drain_to_vec(resp.stream);
        assert!(!partial.is_empty(), "segment 0 streams before the failure");
        assert!(
            matches!(stream.error(), Some(PlayError::Upstream(_))),
            "the dropped connection is a provider failure"
        );
        assert!(
            matches!(
                stream.finalize(&mut pool, &mut cache, &mut meter),
                Err(PlayError::Upstream(_))
            ),
            "mid-track provider failure surfaces on finalize"
        );
        assert_eq!(
            pool.quarantined(),
            i == CIRCUIT_FAILURE_THRESHOLD,
            "the breaker opens exactly at the threshold of mid-track failures"
        );
    }
    assert_eq!(pool.circuit_state(), CircuitState::Open);
    assert!(
        cache.get_track("track-mid").is_none(),
        "failed plays must not cache"
    );
}

#[test]
fn seek_with_ranges_fetches_only_the_covering_segment() {
    // S10 covering-segment fetch: byte ranges address the covering article
    // directly, so a mid-track seek costs one BODY instead of the prefix.
    let (plain, segments, articles) = fixture_track("a", 60_000, 20_000);
    let ranges = segment_ranges_for(60_000, 20_000);
    assert_eq!(
        ranges,
        [(0u64, 20_000u64), (20_000, 40_000), (40_000, 60_000)]
    );
    let server = FakeNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant_for("track-a");
    let inp = input_with_ranges(
        &token,
        "track-a",
        Some("bytes=45000-45099"),
        &server,
        &segments,
        Some(&ranges),
        None,
    );
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("seek with ranges");
    assert_eq!(resp.status, 206);
    assert_eq!(
        resp.content_range.as_deref(),
        Some(format!("bytes 45000-45099/{}", plain.len()).as_str())
    );
    let mut stream = resp.stream;
    let first = stream.next().expect("first chunk");
    assert_eq!(
        server.bodies(),
        1,
        "covering segment 2 addressed directly: no prefix walk"
    );
    let mut body = first;
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body, &plain[45_000..45_100]);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    assert_eq!(
        cache.get_track("track-a").as_deref(),
        Some(plain.as_slice()),
        "covering fetch still caches the whole track"
    );
}

#[test]
fn seek_with_and_without_ranges_yields_byte_identical_output() {
    // S10 parity: the covering fetch and the prefix-walk fallback serve the
    // same bytes; only the BODY count differs (1 vs the whole prefix).
    let (plain, segments, articles) = fixture_track("a", 60_000, 20_000);
    let ranges = segment_ranges_for(60_000, 20_000);
    let covering_server = FakeNntp::start(articles.clone());
    let fallback_server = FakeNntp::start(articles);

    // Covering path: ranges present.
    let mut pool_c: StreamPool = Pool::new(4);
    let mut cache_c = ContentCache::new();
    let mut meter_c = MemoryMeter::default();
    let token_c = grant_for("track-a");
    let inp_c = input_with_ranges(
        &token_c,
        "track-a",
        Some("bytes=45000-45099"),
        &covering_server,
        &segments,
        Some(&ranges),
        None,
    );
    let resp_c =
        play(&mut pool_c, &mut cache_c, &mut meter_c, SECRET, &inp_c).expect("covering seek");
    assert_eq!(resp_c.status, 206);
    let mut stream_c = resp_c.stream;
    let first_c = stream_c.next().expect("first covering chunk");
    assert_eq!(
        covering_server.bodies(),
        1,
        "ranges present: only the covering segment is fetched"
    );
    let mut covering = first_c;
    for chunk in stream_c.by_ref() {
        covering.extend_from_slice(&chunk);
    }
    stream_c
        .finalize(&mut pool_c, &mut cache_c, &mut meter_c)
        .expect("covering finalize");

    // Fallback path: the same seek with ranges absent.
    let mut pool_f: StreamPool = Pool::new(4);
    let mut cache_f = ContentCache::new();
    let mut meter_f = MemoryMeter::default();
    let token_f = grant_for("track-a");
    let inp_f = input(
        &token_f,
        "track-a",
        Some("bytes=45000-45099"),
        &fallback_server,
        &segments,
        None,
    );
    let resp_f =
        play(&mut pool_f, &mut cache_f, &mut meter_f, SECRET, &inp_f).expect("fallback seek");
    assert_eq!(resp_f.status, 206);
    let mut stream_f = resp_f.stream;
    let first_f = stream_f.next().expect("first fallback chunk");
    assert_eq!(
        fallback_server.bodies(),
        3,
        "ranges absent: the prefix walk is preserved"
    );
    let mut fallback = first_f;
    for chunk in stream_f.by_ref() {
        fallback.extend_from_slice(&chunk);
    }
    stream_f
        .finalize(&mut pool_f, &mut cache_f, &mut meter_f)
        .expect("fallback finalize");

    assert_eq!(
        resp_c.content_range, resp_f.content_range,
        "both paths answer the same range"
    );
    assert_eq!(
        covering, fallback,
        "covering fetch and prefix walk serve identical bytes"
    );
    assert_eq!(covering, &plain[45_000..45_100]);
}
