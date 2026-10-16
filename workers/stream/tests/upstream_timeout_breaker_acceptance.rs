//! Acceptance contract for upstream NNTP deadlines and breaker fallback.
//!
//! Keep this test-only: product behavior belongs to the paired implementation.
//! A silent upstream must time out, repeated provider failures must open the
//! breaker and surface the 503 fallback, and a half-open probe must recover.

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
use nntune_stream::meter::MemoryMeter;
use nntune_stream::nntp::UpstreamTimeouts;
use nntune_stream::play::{play_with_timeouts, PlayError, PlayInput, StreamPool};
use nntune_stream::pool::{CircuitState, Pool, CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_OPEN_MS};
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"upstream-timeout-breaker-acceptance-secret";
const NOW_UNIX: u64 = 1_700_000_000;
const EXP_UNIX: u64 = NOW_UNIX + 900;
const CIPHER_KEY: u8 = 0x5A;
const TRACK_ID: &str = "track-timeout";
const MESSAGE_ID: &str = "<timeout-seg0@synthetic.test>";
const SILENT_SERVER_GUARD: Duration = Duration::from_secs(10);

struct FakeNntp {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
    bodies: Arc<AtomicUsize>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeNntp {
    fn start(article: Option<Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
        let addr = listener.local_addr().expect("fake NNTP address");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let stop = Arc::new(AtomicBool::new(false));
        let connections = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(AtomicUsize::new(0));
        let (stop_thread, connections_thread, bodies_thread) = (
            Arc::clone(&stop),
            Arc::clone(&connections),
            Arc::clone(&bodies),
        );
        let handle = thread::spawn(move || {
            let mut handlers = Vec::new();
            while !stop_thread.load(Ordering::SeqCst) {
                let (stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                connections_thread.fetch_add(1, Ordering::SeqCst);
                let (article, bodies, stop) = (
                    article.clone(),
                    Arc::clone(&bodies_thread),
                    Arc::clone(&stop_thread),
                );
                handlers.push(thread::spawn(move || {
                    Self::serve(stream, article.as_deref(), &bodies, &stop)
                }));
            }
            for handler in handlers {
                let _ = handler.join();
            }
        });
        Self {
            addr,
            stop,
            connections,
            bodies,
            handle: Some(handle),
        }
    }

    fn serve(stream: TcpStream, article: Option<&[u8]>, bodies: &AtomicUsize, stop: &AtomicBool) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone fake NNTP stream"));
        let mut writer = stream;
        let _ = writer.write_all(b"200 synthetic NNTP ready\r\n");
        let mut command = Vec::new();
        if reader.read_until(b'\n', &mut command).unwrap_or(0) == 0 {
            return;
        }
        let command = String::from_utf8_lossy(&command);
        if command.trim_end_matches(['\r', '\n']) != format!("BODY {MESSAGE_ID}") {
            let _ = writer.write_all(b"500 expected BODY command\r\n");
            return;
        }
        bodies.fetch_add(1, Ordering::SeqCst);

        if let Some(article) = article {
            let _ = writer.write_all(format!("222 0 {MESSAGE_ID} follows\r\n").as_bytes());
            for line in article.split(|byte| *byte == b'\n') {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let _ = writer.write_all(line);
                let _ = writer.write_all(b"\r\n");
            }
            let _ = writer.write_all(b".\r\n");
        } else {
            // Do not reply to BODY. Keep the socket open beyond the configured
            // client deadline so an implementation without that deadline fails
            // the latency assertion instead of passing on an early EOF.
            let guard_ends_at = Instant::now() + SILENT_SERVER_GUARD;
            let _ = reader
                .get_ref()
                .set_read_timeout(Some(Duration::from_millis(20)));
            while !stop.load(Ordering::SeqCst) && Instant::now() < guard_ends_at {
                match reader.fill_buf() {
                    Ok([]) => break,
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(_) => break,
                }
            }
        }
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    fn bodies(&self) -> usize {
        self.bodies.load(Ordering::SeqCst)
    }

    fn wait_for_counts(&self, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while (self.connections() < expected || self.bodies() < expected)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(self.connections(), expected, "accepted NNTP connections");
        assert_eq!(self.bodies(), expected, "valid NNTP BODY requests");
    }
}

impl Drop for FakeNntp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn timeout_input<'a>(
    token: &'a str,
    server: &'a FakeNntp,
    segments: &'a [String],
) -> PlayInput<'a> {
    PlayInput {
        token,
        track_id: TRACK_ID,
        range: None,
        server_addr: server.addr,
        segments,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    }
}

fn grant_token() -> String {
    grant::issue("user-timeout", TRACK_ID, EXP_UNIX, SECRET)
}

fn healthy_article() -> Vec<u8> {
    yenc::encode_part(
        &[0xA5],
        "timeout.flac",
        PartSpec {
            file_size: 1,
            part: 1,
            total: 1,
            offset: 0,
            file_crc: Some(yenc::crc32(&[0xA5])),
        },
    )
}

fn expect_play_error(
    pool: &mut StreamPool,
    cache: &mut ContentCache,
    meter: &mut MemoryMeter,
    input: &PlayInput<'_>,
    timeouts: UpstreamTimeouts,
) -> PlayError {
    match play_with_timeouts(pool, cache, meter, SECRET, input, timeouts) {
        Err(error) => error,
        Ok(_) => panic!("expected upstream/breaker failure"),
    }
}

#[test]
fn nntp_timeout_opens_breaker_returns_503_and_half_open_probe_recovers() {
    assert_eq!(
        CIRCUIT_FAILURE_THRESHOLD, 3,
        "three failures open the breaker"
    );
    assert_eq!(CIRCUIT_OPEN_MS, 30_000, "breaker stays open for 30 seconds");

    let hung = FakeNntp::start(None);
    let mut pool: StreamPool = Pool::new(2);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let quick_timeouts =
        UpstreamTimeouts::new(Duration::from_millis(100), Duration::from_millis(100));
    let healthy_timeouts = UpstreamTimeouts::new(Duration::from_secs(1), Duration::from_secs(1));
    let segments = vec![MESSAGE_ID.to_string()];

    for failure in 1..=CIRCUIT_FAILURE_THRESHOLD {
        let token = grant_token();
        let input = timeout_input(&token, &hung, &segments);
        let started = Instant::now();
        let error = expect_play_error(&mut pool, &mut cache, &mut meter, &input, quick_timeouts);
        let elapsed = started.elapsed();
        assert!(
            matches!(error, PlayError::Upstream(_)),
            "silent NNTP response must fail upstream, got {error:?}"
        );
        assert!(
            elapsed >= Duration::from_millis(50),
            "proper BODY request should wait for the configured read deadline, took {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "upstream timeout must beat the fake server's 10s safety close, took {elapsed:?}"
        );
        hung.wait_for_counts(failure as usize);
        assert_eq!(pool.in_use(), 0, "timeout must release its pool lease");
        assert_eq!(
            pool.quarantined(),
            failure == CIRCUIT_FAILURE_THRESHOLD,
            "breaker opens exactly at the configured failure threshold"
        );
    }

    assert_eq!(pool.circuit_state(), CircuitState::Open);
    let connections_before_rejection = hung.connections();
    let pool_connects_before_rejection = pool.connects();
    let token = grant_token();
    let input = timeout_input(&token, &hung, &segments);
    let fallback = expect_play_error(&mut pool, &mut cache, &mut meter, &input, quick_timeouts);
    assert_eq!(fallback.http_status(), 503, "breaker fallback is HTTP 503");
    assert_eq!(
        pool.connects(),
        pool_connects_before_rejection,
        "open breaker must reject before dialing"
    );
    assert_eq!(
        fallback.retry_after_header().as_deref(),
        Some((CIRCUIT_OPEN_MS.div_ceil(1_000)).to_string().as_str())
    );
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        hung.connections(),
        connections_before_rejection,
        "open breaker must reject before dialing"
    );

    pool.advance(CIRCUIT_OPEN_MS - 1);
    assert_eq!(pool.circuit_state(), CircuitState::Open);
    let token = grant_token();
    let input = timeout_input(&token, &hung, &segments);
    let near_expiry = expect_play_error(&mut pool, &mut cache, &mut meter, &input, quick_timeouts);
    assert_eq!(near_expiry.http_status(), 503);
    assert_eq!(
        pool.connects(),
        pool_connects_before_rejection,
        "breaker remains closed to dials until its open interval expires"
    );
    assert_eq!(near_expiry.retry_after_header().as_deref(), Some("1"));
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        hung.connections(),
        connections_before_rejection,
        "breaker stays open immediately before the deadline"
    );

    pool.advance(1);
    assert_eq!(pool.circuit_state(), CircuitState::HalfOpen);
    let token = grant_token();
    let input = timeout_input(&token, &hung, &segments);
    let probe_error = expect_play_error(&mut pool, &mut cache, &mut meter, &input, quick_timeouts);
    assert!(matches!(probe_error, PlayError::Upstream(_)));
    assert_eq!(
        pool.circuit_state(),
        CircuitState::Open,
        "failed probe re-opens breaker"
    );

    pool.advance(CIRCUIT_OPEN_MS);
    assert_eq!(pool.circuit_state(), CircuitState::HalfOpen);
    let healthy = FakeNntp::start(Some(healthy_article()));
    let token = grant_token();
    let input = timeout_input(&token, &healthy, &segments);
    let response = play_with_timeouts(
        &mut pool,
        &mut cache,
        &mut meter,
        SECRET,
        &input,
        healthy_timeouts,
    )
    .expect("half-open probe must reach the recovered upstream");
    assert!(
        !response.cache_hit,
        "recovery must fetch from the healthy upstream"
    );
    let mut stream = response.stream;
    let body = stream.next().expect("one article response chunk");
    assert_eq!(body, vec![0xFF], "healthy response is decrypted and served");
    assert!(
        stream.next().is_none(),
        "single-segment response must be exhausted"
    );
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("successful probe finalizes");
    assert_eq!(healthy.connections(), 1);
    assert_eq!(healthy.bodies(), 1);
    assert_eq!(pool.circuit_state(), CircuitState::Closed);
    assert!(!pool.quarantined());
}
