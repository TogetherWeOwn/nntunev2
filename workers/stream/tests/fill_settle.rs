//! Fill-phase settle: a fully-served play must not fail (or quarantine the
//! pool) because the post-range cache fill in `PlayStream::finalize` failed.
//!
//! `finalize` (`workers/stream/src/play.rs`) first serves the client's
//! range, then fetch-fills whatever segments serving skipped so the whole
//! track can be cached. That fill is background work: the client already
//! has its `206`. Today any fill failure falls into the same
//! `Err(settle(...))` as a serve-phase failure, and `settle` runs
//! provider-error accounting (`record_provider_error_for` → possible
//! `QuarantineEntered`), so a transient timeout on an *unrequested* segment
//! fails a play the client experienced as success and can quarantine the
//! breaker for unrelated plays.
//!
//! Contract pinned here:
//! - Fully-served `206` + fill failure (provider timeout, missing article,
//!   or undecodable segment) → `finalize` returns `Ok`, emits no
//!   `QuarantineEntered`, never quarantines the pool, still meters the
//!   served bytes, and leaves the incomplete track uncached with the lease
//!   returned. Fill is still attempted (the failure is observed, not
//!   skipped).
//! - A genuine serve-phase provider failure (the failure happens while the
//!   iterator is still producing the client's bytes, `stream.error()` set
//!   before `finalize`) keeps the exact current settle behavior: `Err`,
//!   breaker accounting, quarantine at the threshold. Green control.
//! - A successful fill still caches the whole track. Green control.
//!
//! Every RED test asserts, *before* `finalize`, that serving is complete
//! and error-free (`stream.error()` is `None`, all ranged bytes drained,
//! exactly one `BODY` issued), so the only failure a test can observe is
//! raised inside `finalize`'s fill loop — never during serving.
//!
//! Failing command:
//! `cargo test --locked -p nntune-stream --test fill_settle`
//!
//! Sensitivity: the provider-timeout test pre-seeds the breaker one
//! failure below its threshold, so a fix that merely swallows the `Err`
//! while still running `settle`'s provider-error accounting quarantines
//! the pool and emits `QuarantineEntered` — and fails here.

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
use nntune_stream::grant;
use nntune_stream::meter::{MemoryMeter, MeterKind};
use nntune_stream::nntp::UpstreamTimeouts;
use nntune_stream::play::{play, play_with_timeouts, PlayError, PlayInput, PlayStream, StreamPool};
use nntune_stream::pool::{CircuitState, Pool, CIRCUIT_FAILURE_THRESHOLD};
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"fill-settle-secret";
const NOW_UNIX: u64 = 1_700_000_000;
// Within the grant TTL cap (`MAX_GRANT_TTL` = 900 s).
const EXP_UNIX: u64 = NOW_UNIX + 900;
const CIPHER_KEY: u8 = 0x5A;
const TRACK_LEN: usize = 60_000;
const SEG_LEN: usize = 20_000;

type Fixture = (Vec<u8>, Vec<String>, HashMap<String, Vec<u8>>);

/// Deterministic pseudo-audio as one yEnc multipart article per segment
/// (same fixture shape as `stream_integration.rs`).
fn fixture_track(tag: &str) -> Fixture {
    let mut plain = Vec::with_capacity(TRACK_LEN);
    for i in 0..TRACK_LEN {
        plain.push(((i * 7919 + tag.len()) % 256) as u8);
    }
    let ciphered: Vec<u8> = plain.iter().map(|b| b ^ CIPHER_KEY).collect();
    let file_crc = yenc::crc32(&ciphered);
    let total = ciphered.len().div_ceil(SEG_LEN) as u32;
    let mut articles = HashMap::new();
    let mut segments = Vec::new();
    for (i, chunk) in ciphered.chunks(SEG_LEN).enumerate() {
        let part = i as u32 + 1;
        let mid = format!("{tag}-seg{i}@synthetic.test");
        let text = yenc::encode_part(
            chunk,
            &format!("{tag}.flac"),
            PartSpec {
                file_size: TRACK_LEN as u64,
                part,
                total,
                offset: (i * SEG_LEN) as u64,
                file_crc: (part == total).then_some(file_crc),
            },
        );
        articles.insert(mid.clone(), text);
        segments.push(mid);
    }
    (plain, segments, articles)
}

fn segment_ranges() -> Vec<(u64, u64)> {
    vec![
        (0u64, 20_000u64),
        (20_000, 40_000),
        (40_000, TRACK_LEN as u64),
    ]
}

/// How the synthetic provider misbehaves, if at all.
#[derive(Clone, Copy)]
enum Fault {
    None,
    /// Never reply to the `BODY` with this 1-based ordinal: the client's
    /// own read timeout ends the fetch (a provider failure).
    HangOnBody(usize),
    /// Hang up the connection after serving this many `BODY` commands.
    CloseAfter(usize),
}

/// Minimal NNTP server: greeting + `BODY <mid>` with dot-stuffing, `430`
/// for unknown articles, plus one optional injected fault.
struct FakeNntp {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    bodies: Arc<AtomicUsize>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeNntp {
    fn start(articles: HashMap<String, Vec<u8>>, fault: Fault) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
        let addr = listener.local_addr().expect("local addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let bodies = Arc::new(AtomicUsize::new(0));
        let (stop_c, bodies_c) = (Arc::clone(&stop), Arc::clone(&bodies));
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
                Self::serve(stream, &articles, fault, &bodies_c, &stop_c);
            }
        });
        Self {
            addr,
            stop,
            bodies,
            handle: Some(handle),
        }
    }

    fn serve(
        stream: TcpStream,
        articles: &HashMap<String, Vec<u8>>,
        fault: Fault,
        bodies: &AtomicUsize,
        stop: &AtomicBool,
    ) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone conn"));
        let mut writer = stream;
        let _ = writer.write_all(b"200 synthetic NNTP ready\r\n");
        let mut served = 0usize;
        loop {
            if matches!(fault, Fault::CloseAfter(limit) if served >= limit) {
                break;
            }
            let mut line = Vec::new();
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
                served += 1;
                let ordinal = bodies.fetch_add(1, Ordering::SeqCst) + 1;
                if matches!(fault, Fault::HangOnBody(target) if ordinal == target) {
                    // Hold the reply forever (until test shutdown): the
                    // client's read timeout, not this thread, ends the fetch.
                    while !stop.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(5));
                    }
                    break;
                }
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

fn input<'a>(
    token: &'a str,
    track_id: &'a str,
    range: Option<&'a str>,
    server: &FakeNntp,
    segments: &'a [String],
    ranges: Option<&'a [(u64, u64)]>,
) -> PlayInput<'a> {
    PlayInput {
        trace_id: None,
        token,
        track_id,
        range,
        server_addr: server.addr,
        segments,
        segment_ranges: ranges,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    }
}

fn drain(mut stream: PlayStream) -> (Vec<u8>, PlayStream) {
    let mut body = Vec::new();
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    (body, stream)
}

fn kind_count(meter: &MemoryMeter, kind: MeterKind) -> usize {
    meter.events.iter().filter(|e| e.kind == kind).count()
}

fn bytes_served(meter: &MemoryMeter) -> u64 {
    meter
        .events
        .iter()
        .filter(|e| e.kind == MeterKind::BytesServed)
        .map(|e| e.bytes)
        .sum()
}

/// One provider failure short of opening the breaker: exactly one more
/// recorded provider failure quarantines the pool.
fn seed_one_below_threshold(pool: &mut StreamPool) {
    for _ in 0..(CIRCUIT_FAILURE_THRESHOLD - 1) {
        pool.record_provider_error();
    }
    assert!(
        !pool.quarantined(),
        "seeded streak stays below the threshold"
    );
}

/// Core RED: suffix-range play fully served as `206`, then the first
/// post-range fill fetch hangs and hits the client's read timeout (a
/// provider failure). The served play must survive: `Ok`, no quarantine.
#[test]
fn served_206_fill_timeout_finalizes_ok_without_quarantine() {
    let (plain, segments, articles) = fixture_track("fill-timeout");
    assert_eq!(segments.len(), 3);
    // BODY 1 is the covering (last) segment that serves the suffix range;
    // BODY 2 is the first fill fetch inside `finalize` and never replies.
    let server = FakeNntp::start(articles, Fault::HangOnBody(2));
    let ranges = segment_ranges();
    let quick = UpstreamTimeouts::new(Duration::from_millis(200), Duration::from_millis(200));

    let mut pool: StreamPool = Pool::new(4);
    seed_one_below_threshold(&mut pool);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant::issue("user-1", "track-fill-timeout", EXP_UNIX, SECRET);
    let inp = input(
        &token,
        "track-fill-timeout",
        Some("bytes=-100"),
        &server,
        &segments,
        Some(&ranges),
    );
    let resp = play_with_timeouts(&mut pool, &mut cache, &mut meter, SECRET, &inp, quick)
        .expect("suffix play must start");
    assert_eq!(resp.status, 206);
    assert_eq!(
        resp.content_range.as_deref(),
        Some(format!("bytes {}-{}/{}", TRACK_LEN - 100, TRACK_LEN - 1, TRACK_LEN).as_str())
    );
    let (body, stream) = drain(resp.stream);
    assert_eq!(body, &plain[TRACK_LEN - 100..], "client got its full range");
    assert_eq!(stream.served_bytes(), 100);
    assert!(
        stream.error().is_none(),
        "serving completed without error; got {:?}",
        stream.error()
    );
    assert_eq!(
        server.bodies(),
        1,
        "only the covering segment was fetched to serve; fill has not run yet"
    );

    let result = stream.finalize(&mut pool, &mut cache, &mut meter);
    assert!(
        server.bodies() >= 2,
        "finalize must still attempt the post-range fill"
    );
    assert_eq!(
        result,
        Ok(()),
        "a fill-phase timeout must not fail a fully-served play"
    );
    assert!(
        !pool.quarantined(),
        "background fill failure must not quarantine the pool"
    );
    assert_eq!(pool.circuit_state(), CircuitState::Closed);
    assert_eq!(
        kind_count(&meter, MeterKind::QuarantineEntered),
        0,
        "no QuarantineEntered may be emitted for a fill failure"
    );
    assert_eq!(
        bytes_served(&meter),
        100,
        "the served bytes are still metered"
    );
    assert!(
        cache.get_track("track-fill-timeout").is_none(),
        "an incomplete fill must not cache a partial track"
    );
    assert_eq!(pool.in_use(), 0, "the lease is returned");
    assert_eq!(
        pool.idle(),
        0,
        "a connection that timed out mid-BODY is desynchronized and must not be parked warm"
    );
}

/// RED: the fill fetch hits a gone article (`430`). Non-provider, but
/// still background work — the served play must finalize `Ok`.
#[test]
fn served_206_fill_missing_article_finalizes_ok_without_quarantine() {
    let (plain, segments, mut articles) = fixture_track("fill-missing");
    // Segment 1 is gone from the provider; serving bytes 0-99 never needs it.
    articles.remove(&segments[1]);
    let server = FakeNntp::start(articles, Fault::None);

    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant::issue("user-1", "track-fill-missing", EXP_UNIX, SECRET);
    let inp = input(
        &token,
        "track-fill-missing",
        Some("bytes=0-99"),
        &server,
        &segments,
        None,
    );
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("play must start");
    assert_eq!(resp.status, 206);
    let (body, stream) = drain(resp.stream);
    assert_eq!(body, &plain[..100]);
    assert!(stream.error().is_none(), "serving completed without error");
    assert_eq!(server.bodies(), 1, "fill has not run yet");

    let result = stream.finalize(&mut pool, &mut cache, &mut meter);
    assert!(
        server.bodies() >= 2,
        "finalize must still attempt the post-range fill"
    );
    assert_eq!(
        result,
        Ok(()),
        "a fill-phase missing article must not fail a fully-served play"
    );
    assert!(!pool.quarantined());
    assert_eq!(kind_count(&meter, MeterKind::QuarantineEntered), 0);
    assert_eq!(bytes_served(&meter), 100);
    assert!(
        cache.get_track("track-fill-missing").is_none(),
        "an incomplete fill must not cache a partial track"
    );
    assert_eq!(pool.in_use(), 0, "the lease is returned");
}

/// RED: the fill fetch returns an article that cannot be placed (segment 1
/// duplicates segment 0 → decode/`BadPart` during fill ingest). Background
/// work again — the served covering range must finalize `Ok`.
#[test]
fn served_206_fill_decode_failure_finalizes_ok_without_quarantine() {
    let (plain, mut segments, articles) = fixture_track("fill-decode");
    segments[1] = segments[0].clone();
    let server = FakeNntp::start(articles, Fault::None);
    let ranges = segment_ranges();

    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant::issue("user-1", "track-fill-decode", EXP_UNIX, SECRET);
    let inp = input(
        &token,
        "track-fill-decode",
        Some("bytes=45000-45099"),
        &server,
        &segments,
        Some(&ranges),
    );
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("play must start");
    assert_eq!(resp.status, 206);
    let (body, stream) = drain(resp.stream);
    assert_eq!(body, &plain[45_000..45_100]);
    assert!(stream.error().is_none(), "serving completed without error");
    assert_eq!(
        server.bodies(),
        1,
        "only the covering segment was fetched to serve; fill has not run yet"
    );

    let result = stream.finalize(&mut pool, &mut cache, &mut meter);
    assert!(
        server.bodies() >= 3,
        "fill must fetch the skipped prefix (segment 0, then the duplicate)"
    );
    assert_eq!(
        result,
        Ok(()),
        "a fill-phase decode failure must not fail a fully-served play"
    );
    assert!(!pool.quarantined());
    assert_eq!(kind_count(&meter, MeterKind::QuarantineEntered), 0);
    assert_eq!(bytes_served(&meter), 100);
    assert!(
        cache.get_track("track-fill-decode").is_none(),
        "an incomplete fill must not cache a partial track"
    );
    assert_eq!(pool.in_use(), 0, "the lease is returned");
}

/// Green control: a provider failure *during serving* (the iterator still
/// owes the client bytes when the connection drops) keeps the exact
/// current settle behavior — `Err` from `finalize`, breaker accounting,
/// quarantine at the threshold. The fill-phase fix must not widen into
/// swallowing serve-phase failures.
#[test]
fn serve_phase_provider_failure_still_errors_and_quarantines() {
    let (_plain, segments, articles) = fixture_track("serve-phase");
    // Serves segment 0, then hangs up: the failure lands mid-range.
    let server = FakeNntp::start(articles, Fault::CloseAfter(1));

    let mut pool: StreamPool = Pool::new(4);
    seed_one_below_threshold(&mut pool);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant::issue("user-1", "track-serve-phase", EXP_UNIX, SECRET);
    let inp = input(&token, "track-serve-phase", None, &server, &segments, None);
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("first segment ok");
    let (partial, stream) = drain(resp.stream);
    assert!(!partial.is_empty(), "segment 0 streams before the failure");
    assert!(
        (partial.len() as u64) < TRACK_LEN as u64,
        "the client's bytes were NOT fully served"
    );
    assert!(
        matches!(stream.error(), Some(PlayError::Upstream(_))),
        "serve-phase failure is recorded on the stream; got {:?}",
        stream.error()
    );

    let result = stream.finalize(&mut pool, &mut cache, &mut meter);
    assert!(
        matches!(result, Err(PlayError::Upstream(_))),
        "serve-phase provider failure must still surface on finalize; got {result:?}"
    );
    assert!(
        pool.quarantined(),
        "serve-phase failure at the threshold must still quarantine"
    );
    assert_eq!(pool.circuit_state(), CircuitState::Open);
    assert_eq!(kind_count(&meter, MeterKind::QuarantineEntered), 1);
    assert!(cache.get_track("track-serve-phase").is_none());
    assert_eq!(pool.in_use(), 0);
}

/// Green control: fill still runs on the happy path — a covering-range
/// play caches the whole track in `finalize`, fetching every segment
/// exactly once.
#[test]
fn served_206_successful_fill_still_caches_whole_track() {
    let (plain, segments, articles) = fixture_track("fill-ok");
    let server = FakeNntp::start(articles, Fault::None);
    let ranges = segment_ranges();

    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant::issue("user-1", "track-fill-ok", EXP_UNIX, SECRET);
    let inp = input(
        &token,
        "track-fill-ok",
        Some("bytes=-100"),
        &server,
        &segments,
        Some(&ranges),
    );
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("play must start");
    assert_eq!(resp.status, 206);
    let (body, stream) = drain(resp.stream);
    assert_eq!(body, &plain[TRACK_LEN - 100..]);
    assert_eq!(server.bodies(), 1, "covering segment only, before fill");

    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("successful fill finalizes Ok");
    assert_eq!(server.bodies(), 3, "fill fetched the two skipped segments");
    assert_eq!(
        cache.get_track("track-fill-ok").as_deref(),
        Some(plain.as_slice()),
        "successful fill caches the whole track"
    );
    assert_eq!(bytes_served(&meter), 100);
    assert!(!pool.quarantined());
    assert_eq!(pool.in_use(), 0);
}
