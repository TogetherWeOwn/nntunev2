//! Acceptance: an over-cap article on the PLAY path must be refused as a
//! size problem, not a provider outage.
//!
//! Contract pinned for the paired build (observable surface only, so this
//! file compiles unchanged both before and after the fix):
//!
//! - the play fails with HTTP `502` in the corrupt-content class
//!   (`corrupt_content` or a dedicated size code), never
//!   `upstream_unavailable` and never a pool error;
//! - a `size_refused` meter signal is emitted (kind slug or kind name);
//! - no `ProviderError` / `QuarantineEntered` metering, and the pool is
//!   never quarantined — repeated over-cap plays must not trip the
//!   3-in-30 s breaker;
//! - the same pool serves a following in-cap track immediately.
//!
//! On the base code the over-cap arm surfaces as
//! `NntpError::UnexpectedResponse("NNTP article exceeds byte limit")`,
//! which `is_provider_failure()` counts, so the play maps to
//! `PlayError::Upstream` / `upstream_unavailable` and feeds the
//! quarantine breaker. Every assertion below fails for that reason.
//!
//! Warm-connection retention is deliberately NOT pinned: the refusal is
//! detected mid-dot-block, so the pooled connection is desynchronized at
//! that point. Dropping it (fresh dial for the next play) and draining
//! the rest of the article (warm reuse) are both acceptable builds; the
//! fake server below sends a finite article terminated by `.` so either
//! build can settle. What is pinned is that the *pool* is immediately
//! reusable by the next play.
//!
//! The cap itself is `nntune_core::transport::MAX_ARTICLE_BYTES` counted
//! on the dot-unstuffed, LF-joined body: each wire line contributes its
//! data bytes plus one LF. Lines stay well under the 16 KiB wire-line
//! ceiling so only the article cap can refuse.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use nntune_core::transport::MAX_ARTICLE_BYTES;
use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::{kind_slug, MemoryMeter, MeterKind};
use nntune_stream::play::{play, PlayError, PlayInput, PlayStream, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"play-article-cap-acceptance-secret";
const NOW_UNIX: u64 = 1_700_000_000;
// Within the grant TTL cap (`MAX_GRANT_TTL` = 900 s).
const EXP_UNIX: u64 = NOW_UNIX + 900;
const CIPHER_KEY: u8 = 0x5A;
const BIG_MID: &str = "<big-seg0@synthetic.test>";
/// Bytes per streamed line. Client-side the line counts `LINE + 1`
/// (data plus the LF it retains); wire-side it is `LINE + 2` (CRLF),
/// comfortably under the 16 KiB line ceiling.
const LINE: usize = 8192;

/// Lowercase-alphanumeric-only rendering, so `SizeRefused`,
/// `size_refused` and `size-refused` all compare equal.
fn normalized(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn size_refused_events(meter: &MemoryMeter) -> usize {
    meter
        .events
        .iter()
        .filter(|e| {
            kind_slug(e.kind) == "size_refused"
                || normalized(kind_slug(e.kind)).contains("sizerefused")
                || normalized(&format!("{:?}", e.kind)).contains("sizerefused")
        })
        .count()
}

/// A small, valid, single-segment track, posted the way Usenet does it
/// (multipart-shaped yEnc, whole-file CRC on the only part).
fn small_track(tag: &str, len: usize) -> (Vec<u8>, Vec<String>, HashMap<String, Vec<u8>>) {
    let mut plain = Vec::with_capacity(len);
    for i in 0..len {
        plain.push(((i * 7919 + tag.len()) % 256) as u8);
    }
    let ciphered: Vec<u8> = plain.iter().map(|b| b ^ CIPHER_KEY).collect();
    let mid = format!("<{tag}-seg0@synthetic.test>");
    let text = yenc::encode_part(
        &ciphered,
        &format!("{tag}.flac"),
        PartSpec {
            file_size: len as u64,
            part: 1,
            total: 1,
            offset: 0,
            file_crc: Some(yenc::crc32(&ciphered)),
        },
    );
    let mut articles = HashMap::new();
    articles.insert(mid.clone(), text);
    (plain, vec![mid], articles)
}

/// NNTP server that serves the small fixture articles normally and
/// streams one finite, over-cap dot-block for [`BIG_MID`]: `LINE`-byte
/// `A` lines until the client-side counted size passes
/// `MAX_ARTICLE_BYTES`, then the `.` terminator. A client that aborts
/// mid-block simply breaks the write side; a client that drains sees a
/// well-formed end of article.
struct CapNntp {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    big_bodies: Arc<AtomicUsize>,
    handle: Option<thread::JoinHandle<()>>,
}

impl CapNntp {
    fn start(articles: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind cap NNTP");
        let addr = listener.local_addr().expect("local addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let big_bodies = Arc::new(AtomicUsize::new(0));
        let (stop_c, big_c) = (Arc::clone(&stop), Arc::clone(&big_bodies));
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
                Self::serve(stream, &articles, &big_c);
            }
        });
        Self {
            addr,
            stop,
            big_bodies,
            handle: Some(handle),
        }
    }

    /// Stream the over-cap article. Returns `false` when the peer went
    /// away mid-block (the abort-without-drain build), so the caller
    /// stops serving this connection.
    fn serve_big(writer: &mut TcpStream) -> bool {
        if writer
            .write_all(format!("222 0 {BIG_MID} follows\r\n").as_bytes())
            .is_err()
        {
            return false;
        }
        let mut line = vec![b'A'; LINE];
        line.extend_from_slice(b"\r\n");
        // The client counts data bytes plus one retained LF per line.
        let counted_per_line = LINE + 1;
        let lines = MAX_ARTICLE_BYTES / counted_per_line + 2;
        for _ in 0..lines {
            if writer.write_all(&line).is_err() {
                return false;
            }
        }
        writer.write_all(b".\r\n").is_ok()
    }

    fn serve(stream: TcpStream, articles: &HashMap<String, Vec<u8>>, big_bodies: &AtomicUsize) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone conn"));
        let mut writer = stream;
        let _ = writer.write_all(b"200 synthetic NNTP ready\r\n");
        let mut raw = Vec::new();
        loop {
            raw.clear();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            while raw.last() == Some(&b'\n') || raw.last() == Some(&b'\r') {
                raw.pop();
            }
            let cmd = String::from_utf8_lossy(&raw).into_owned();
            let Some(mid) = cmd
                .strip_prefix("BODY ")
                .map(|s| s.trim().trim_matches(|c| c == '<' || c == '>'))
            else {
                let _ = writer.write_all(b"500 unknown command\r\n");
                continue;
            };
            let full_mid = format!("<{mid}>");
            if full_mid == BIG_MID {
                big_bodies.fetch_add(1, Ordering::SeqCst);
                if !Self::serve_big(&mut writer) {
                    break;
                }
                continue;
            }
            match articles.get(&full_mid) {
                Some(article) => {
                    let _ = writer.write_all(format!("222 0 {full_mid} follows\r\n").as_bytes());
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
        }
    }

    fn big_bodies(&self) -> usize {
        self.big_bodies.load(Ordering::SeqCst)
    }
}

impl Drop for CapNntp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn input<'a>(
    token: &'a str,
    track_id: &'a str,
    server: &CapNntp,
    segments: &'a [String],
) -> PlayInput<'a> {
    PlayInput {
        trace_id: None,
        token,
        track_id,
        range: None,
        server_addr: server.addr,
        segments,
        segment_ranges: None,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    }
}

fn play_err(
    pool: &mut StreamPool,
    cache: &mut ContentCache,
    meter: &mut MemoryMeter,
    server: &CapNntp,
    track_id: &str,
    segments: &[String],
) -> PlayError {
    let token = grant::issue("user-1", track_id, EXP_UNIX, SECRET);
    let inp = input(&token, track_id, server, segments);
    // PlayResponse is not Debug, so no expect_err here.
    match play(pool, cache, meter, SECRET, &inp) {
        Ok(_) => panic!("play of {track_id} must fail"),
        Err(e) => e,
    }
}

fn play_ok(
    pool: &mut StreamPool,
    cache: &mut ContentCache,
    meter: &mut MemoryMeter,
    server: &CapNntp,
    track_id: &str,
    segments: &[String],
) -> Vec<u8> {
    let token = grant::issue("user-1", track_id, EXP_UNIX, SECRET);
    let inp = input(&token, track_id, server, segments);
    let response = play(pool, cache, meter, SECRET, &inp)
        .unwrap_or_else(|e| panic!("play of {track_id} must serve: {e:?}"));
    fn drain(mut stream: PlayStream) -> (Vec<u8>, PlayStream) {
        let mut body = Vec::new();
        for chunk in stream.by_ref() {
            body.extend_from_slice(&chunk);
        }
        (body, stream)
    }
    let (body, stream) = drain(response.stream);
    stream
        .finalize(pool, cache, meter)
        .expect("finalize must succeed");
    body
}

/// The full refusal contract for one over-cap play. `context` labels the
/// failing assertion output.
fn assert_size_refusal(err: &PlayError, pool: &StreamPool, meter: &MemoryMeter, context: &str) {
    assert!(
        !matches!(
            err,
            PlayError::Upstream(_) | PlayError::PoolQuarantined { .. } | PlayError::PoolExhausted
        ),
        "{context}: over-cap must not be an upstream/pool failure, got {err:?}"
    );
    assert_eq!(
        err.http_status(),
        502,
        "{context}: corrupt-content class status"
    );
    assert_eq!(
        err.pool_error_code(),
        None,
        "{context}: no pool error code may be reported"
    );
    let api = err.api_error();
    assert_eq!(api.status, 502, "{context}: envelope status");
    assert_ne!(
        api.code, "upstream_unavailable",
        "{context}: an over-cap article is not an upstream outage"
    );
    assert_ne!(
        api.code, "upstream_circuit_open",
        "{context}: an over-cap article must not open the circuit"
    );
    let code = normalized(api.code);
    assert!(
        api.code == "corrupt_content"
            || code.contains("sizerefused")
            || code.contains("toolarge")
            || code.contains("toobig")
            || code.contains("oversize"),
        "{context}: code must be corrupt_content or a dedicated size code, got {:?}",
        api.code
    );
    assert!(
        !api.retryable,
        "{context}: retrying the same over-cap article cannot succeed"
    );
    assert!(
        size_refused_events(meter) >= 1,
        "{context}: a size_refused meter signal must be emitted, got {:?}",
        meter
            .events
            .iter()
            .map(|e| kind_slug(e.kind))
            .collect::<Vec<_>>()
    );
    assert!(
        !meter.has_kind(MeterKind::ProviderError),
        "{context}: over-cap is not a provider error"
    );
    assert!(
        !meter.has_kind(MeterKind::QuarantineEntered),
        "{context}: over-cap must not enter quarantine"
    );
    assert!(
        !pool.quarantined(),
        "{context}: pool must not be quarantined"
    );
    assert_eq!(pool.in_use(), 0, "{context}: no lease may be held");
}

#[test]
fn over_cap_play_article_is_size_refused_without_quarantine_and_pool_stays_usable() {
    let (plain, small_segments, articles) = small_track("ok", 20_000);
    let server = CapNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let big_segments = vec![BIG_MID.to_string()];

    let err = play_err(
        &mut pool,
        &mut cache,
        &mut meter,
        &server,
        "track-big",
        &big_segments,
    );
    assert!(
        server.big_bodies() >= 1,
        "the over-cap article must actually have been fetched"
    );
    assert_size_refusal(&err, &pool, &meter, "first over-cap play");

    // Immediately reusable: the same pool serves an in-cap track on the
    // next play, byte-identical, still unquarantined.
    let body = play_ok(
        &mut pool,
        &mut cache,
        &mut meter,
        &server,
        "track-ok",
        &small_segments,
    );
    assert_eq!(body, plain);
    assert!(!pool.quarantined(), "pool must stay healthy after reuse");
}

#[test]
fn repeated_over_cap_plays_never_trip_the_quarantine_breaker() {
    // Three provider failures inside 30 s open the breaker for true
    // upstream trouble. Size refusals must not feed that counter: the
    // third refusal — and a good play after it — must still go through.
    let (plain, small_segments, articles) = small_track("ok2", 20_000);
    let server = CapNntp::start(articles);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let big_segments = vec![BIG_MID.to_string()];

    let mut signals_before = 0usize;
    for attempt in 1..=3 {
        let track = format!("track-big-{attempt}");
        let err = play_err(
            &mut pool,
            &mut cache,
            &mut meter,
            &server,
            &track,
            &big_segments,
        );
        assert_size_refusal(&err, &pool, &meter, &format!("over-cap play {attempt}"));
        let signals = size_refused_events(&meter);
        assert!(
            signals > signals_before,
            "over-cap play {attempt}: each refusal must emit its own size_refused signal \
             ({signals_before} -> {signals})"
        );
        signals_before = signals;
    }
    assert!(
        server.big_bodies() >= 3,
        "each play must reach the upstream, got {}",
        server.big_bodies()
    );

    // A quarantined pool would refuse here with 503 without dialing.
    let body = play_ok(
        &mut pool,
        &mut cache,
        &mut meter,
        &server,
        "track-ok2",
        &small_segments,
    );
    assert_eq!(body, plain);
    assert!(
        !pool.quarantined(),
        "pool must never quarantine on size refusals"
    );
}
