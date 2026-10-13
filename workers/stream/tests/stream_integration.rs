//! End-to-end play path against a synthetic NNTP server (std-only):
//! grant → pool → fetch → yEnc → decrypt → Range stream → R2 writeback.
//!
//! Acceptance probes: warm pool performs no per-play reconnect; `Range`
//! yields `206`; provider outage quarantines and recovers; TTFB is measured
//! on both paths (targets documented in `workers/stream/README.md`).

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
use nntune_stream::play::{play, PlayError, PlayInput};
use nntune_stream::pool::Pool;
use nntune_stream::yenc;

const SECRET: &[u8] = b"stream-integration-secret";
const NOW_UNIX: u64 = 1_700_000_000;
const EXP_UNIX: u64 = 9_999_999_999;
const CIPHER_KEY: u8 = 0x5A;

/// Deterministic pseudo-audio covering every byte value (exercises yEnc
/// escapes), split into two NNTP segments.
fn fixture_track() -> (Vec<u8>, Vec<String>, HashMap<String, Vec<u8>>) {
    let mut plain = Vec::with_capacity(40_000);
    for i in 0..40_000usize {
        plain.push(((i * 7919) % 256) as u8);
    }
    let ciphered: Vec<u8> = plain.iter().map(|b| b ^ CIPHER_KEY).collect();
    let mut articles = HashMap::new();
    let mut segments = Vec::new();
    for (i, chunk) in ciphered.chunks(20_000).enumerate() {
        let mid = format!("<seg{i}@synthetic.test>");
        articles.insert(mid.clone(), yenc::encode(chunk, &format!("seg{i}.bin")));
        segments.push(mid);
    }
    (plain, segments, articles)
}

/// Minimal NNTP server: greeting + `BODY <mid>` with dot-stuffing.
struct FakeNntp {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    connects: Arc<AtomicUsize>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeNntp {
    fn start(articles: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
        let addr = listener.local_addr().expect("local addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let connects = Arc::new(AtomicUsize::new(0));
        let (stop_c, connects_c) = (Arc::clone(&stop), Arc::clone(&connects));
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
                Self::serve(stream, &articles);
            }
        });
        Self {
            addr,
            stop,
            connects,
            handle: Some(handle),
        }
    }

    fn serve(stream: TcpStream, articles: &HashMap<String, Vec<u8>>) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone conn"));
        let mut writer = stream;
        let _ = writer.write_all(b"200 synthetic NNTP ready\r\n");
        let mut line = Vec::new();
        loop {
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
                let full_mid = format!("<{mid}>");
                match articles.get(&full_mid) {
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

    fn connects(&self) -> usize {
        self.connects.load(Ordering::SeqCst)
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

fn drain_to_vec(
    stream: nntune_stream::play::PlayStream,
) -> (Vec<u8>, nntune_stream::play::PlayStream) {
    // Collect one chunk at a time to prove chunked streaming, keeping the
    // stream alive for finalize.
    let mut stream = stream;
    let mut body = Vec::new();
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    (body, stream)
}

#[test]
fn warm_pool_serves_second_play_without_reconnect() {
    let (plain, segments, articles) = fixture_track();
    let server = FakeNntp::start(articles);
    let mut pool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // Miss: fetch via the pool, decode, decrypt, stream + stage writeback.
    let token = grant::issue("user-1", "track-a", EXP_UNIX, SECRET);
    let input = PlayInput {
        token: &token,
        track_id: "track-a",
        range: None,
        server_addr: server.addr,
        segments: &segments,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: Some("track-b"),
    };
    let t0 = Instant::now();
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &input).expect("miss play");
    assert_eq!(resp.status, 200);
    assert!(!resp.cache_hit);
    assert_eq!(resp.body_len as usize, plain.len());
    assert!(resp.stream.is_writeback());
    let mut stream = resp.stream;
    let first = stream.next().expect("first chunk");
    let ttfb_miss = t0.elapsed();
    let mut body = first;
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body, plain, "miss path must reproduce the plaintext");
    stream.finalize(&mut cache);
    assert_eq!(server.connects(), 1, "one pooled connection for the fetch");
    println!("miss-path TTFB (fetch+decode+decrypt to first byte): {ttfb_miss:?}");

    // Hit: served from cache on a warm connection, no reconnect.
    let token2 = grant::issue("user-1", "track-a", EXP_UNIX, SECRET);
    let input2 = PlayInput {
        token: &token2,
        track_id: "track-a",
        range: None,
        server_addr: server.addr,
        segments: &segments,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let t0 = Instant::now();
    let resp2 = play(&mut pool, &mut cache, &mut meter, SECRET, &input2).expect("hit play");
    assert!(resp2.cache_hit);
    assert!(!resp2.stream.is_writeback());
    let mut stream2 = resp2.stream;
    let first2 = stream2.next().expect("first cached chunk");
    let ttfb_hit = t0.elapsed();
    let mut body2 = first2;
    for chunk in stream2.by_ref() {
        body2.extend_from_slice(&chunk);
    }
    assert_eq!(body2, plain, "hit path must reproduce the plaintext");
    stream2.finalize(&mut cache);
    assert_eq!(
        server.connects(),
        1,
        "no per-play reconnect when pool is warm"
    );
    println!("hit-path TTFB (authorize+cache to first byte): {ttfb_hit:?}");
    assert!(
        ttfb_hit < Duration::from_millis(100),
        "local hit TTFB budget: {ttfb_hit:?}"
    );

    // Metering saw the whole story.
    assert!(meter.has_kind(MeterKind::PlayStart));
    assert!(meter.has_kind(MeterKind::CacheMiss));
    assert!(meter.has_kind(MeterKind::CacheHit));
    assert!(meter.has_kind(MeterKind::BytesServed));
    assert!(meter.has_kind(MeterKind::PrefetchEnqueued));
}

#[test]
fn range_request_returns_206_slice() {
    let (plain, segments, articles) = fixture_track();
    let server = FakeNntp::start(articles);
    let mut pool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // Populate the cache with a full play.
    let token = grant::issue("user-1", "track-a", EXP_UNIX, SECRET);
    let input = PlayInput {
        token: &token,
        track_id: "track-a",
        range: None,
        server_addr: server.addr,
        segments: &segments,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &input).expect("seed play");
    let (body, stream) = drain_to_vec(resp.stream);
    assert_eq!(body, plain);
    stream.finalize(&mut cache);

    // Seek: first 100 bytes.
    let token2 = grant::issue("user-1", "track-a", EXP_UNIX, SECRET);
    let input2 = PlayInput {
        token: &token2,
        track_id: "track-a",
        range: Some("bytes=0-99"),
        server_addr: server.addr,
        segments: &segments,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let resp2 = play(&mut pool, &mut cache, &mut meter, SECRET, &input2).expect("range play");
    assert_eq!(resp2.status, 206);
    assert_eq!(
        resp2.content_range.as_deref(),
        Some(format!("bytes 0-99/{}", plain.len()).as_str())
    );
    let (slice, stream2) = drain_to_vec(resp2.stream);
    assert_eq!(slice, &plain[..100]);
    stream2.finalize(&mut cache);

    // Unsatisfiable range.
    let token3 = grant::issue("user-1", "track-a", EXP_UNIX, SECRET);
    let input3 = PlayInput {
        token: &token3,
        track_id: "track-a",
        range: Some("bytes=99999999-"),
        server_addr: server.addr,
        segments: &segments,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &input3),
        Err(PlayError::RangeUnsatisfiable)
    ));
}

#[test]
fn provider_outage_quarantines_then_recovers() {
    let (plain, segments, articles) = fixture_track();
    let server = FakeNntp::start(articles);
    let mut pool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // Point at a dead port: the fetch fails and quarantines the pool.
    let dead: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let token = grant::issue("user-1", "track-a", EXP_UNIX, SECRET);
    let input = PlayInput {
        token: &token,
        track_id: "track-a",
        range: None,
        server_addr: dead,
        segments: &segments,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &input),
        Err(PlayError::Upstream(_))
    ));
    assert!(pool.quarantined());
    assert!(meter.has_kind(MeterKind::ProviderError));

    // Still quarantined: the next play is rejected without dialing out.
    let token2 = grant::issue("user-1", "track-a", EXP_UNIX, SECRET);
    let input2 = PlayInput {
        token: &token2,
        track_id: "track-a",
        range: None,
        server_addr: server.addr,
        segments: &segments,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &input2),
        Err(PlayError::PoolQuarantined { .. })
    ));
    assert_eq!(server.connects(), 0, "quarantined pool must not dial out");

    // Backoff elapses: the pool recovers and the play succeeds.
    pool.advance(300_000);
    let token3 = grant::issue("user-1", "track-a", EXP_UNIX, SECRET);
    let input3 = PlayInput {
        token: &token3,
        track_id: "track-a",
        range: None,
        server_addr: server.addr,
        segments: &segments,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &input3).expect("recovered play");
    let (body, stream) = drain_to_vec(resp.stream);
    assert_eq!(body, plain);
    stream.finalize(&mut cache);
    assert!(!pool.quarantined(), "success clears the quarantine");
}
