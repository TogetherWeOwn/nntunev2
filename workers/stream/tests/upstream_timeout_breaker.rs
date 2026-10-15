//! Upstream NNTP timeout and circuit breaker acceptance: 503 fallback.
//!
//! Paired acceptance tests for the `stream: upstream NNTP timeout and
//! circuit breaker with 503 fallback` build slice (roadmap item: per-upstream
//! timeouts plus a breaker that opens after N consecutive failures and serves
//! a documented 503 with `Retry-After` instead of hanging Range fetches; a
//! half-open probe recovers). The builder builds on this card's branch and
//! never edits this file: implement the missing symbols to satisfy it.
//!
//! Acceptance mapping:
//!
//! - Hung upstream fetch exceeds timeout and is abandoned:
//!   [`hung_upstream_fetch_exceeds_timeout_and_is_abandoned`].
//! - N consecutive failures open the breaker; 503 + `Retry-After` asserted
//!   on Range fetch:
//!   [`n_consecutive_failures_open_the_breaker_and_range_fetch_serves_503_with_retry_after`].
//! - Half-open probe success closes the breaker:
//!   [`half_open_probe_success_closes_the_breaker`].
//!
//! Run only this file (the rest of the workspace stays green until the build
//! lands):
//!
//! ```sh
//! cargo test -p nntune-stream --test upstream_timeout_breaker
//! ```
//!
//! Fixtures are synthetic and `std`-only (loopback TCP fakes, fake clock):
//! no real provider, no network beyond loopback, no Cloudflare bindings (the
//! crate stays dependency-free).
//!
//! # BUILDER CONTRACT (implement to make this file compile green)
//!
//! Add one module to `workers/stream/src` and re-export it from `lib.rs`:
//!
//! ```text
//! nntune_stream::breaker::CircuitBreaker
//! ```
//!
//! Minimum shapes used below (you may extend, never narrow). Document the
//! exact threshold / window / timeout numbers in `workers/stream/README.md`
//! with the same values:
//!
//! ```text
//! pub const UPSTREAM_TIMEOUT: Duration = Duration::from_millis(750);
//! pub const FAILURE_THRESHOLD: u32 = 3;
//! pub const FAILURE_WINDOW_SECS: u64 = 60;
//! pub const OPEN_SECS: u64 = 30;
//!
//! pub struct BreakerConfig {
//!     pub timeout: Duration,
//!     pub failure_threshold: u32,
//!     pub window_secs: u64,
//!     pub open_secs: u64,
//! }
//! impl Default for BreakerConfig // each field equals its const above
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq)]
//! pub enum BreakerState { Closed, Open, HalfOpen }
//!
//! pub struct BreakerHttpResponse {
//!     pub status: u16,
//!     pub retry_after_secs: Option<u64>,
//! }
//!
//! impl CircuitBreaker {
//!     pub fn new(config: BreakerConfig) -> Self;
//!     /// False while open: the caller must serve 503 + Retry-After
//!     /// without dialing upstream. While half-open the first call takes
//!     /// the single probe slot (true); further calls stay false until
//!     /// the probe resolves via record_success / record_failure.
//!     pub fn should_allow(&mut self) -> bool;
//!     /// Success resets the failure count and closes the breaker
//!     /// (including resolving a half-open probe).
//!     pub fn record_success(&mut self);
//!     /// Failure counts toward FAILURE_THRESHOLD inside the rolling
//!     /// FAILURE_WINDOW_SECS window; reaching the threshold opens the
//!     /// breaker. A failure resolving a half-open probe re-opens it
//!     /// for a fresh OPEN_SECS.
//!     pub fn record_failure(&mut self);
//!     pub fn state(&self) -> BreakerState;
//!     /// Some(503) while open, None otherwise.
//!     pub fn http_status(&self) -> Option<u16>;
//!     /// Some(OPEN_SECS) while open, None otherwise.
//!     pub fn retry_after_secs(&self) -> Option<u64>;
//!     /// The breaker-imposed answer for a Range fetch: Some(503 +
//!     /// Retry-After) while open whatever the Range value is
//!     /// (Some(valid) or None alike); None while closed / half-open
//!     /// (the fetch proceeds to the normal Range path).
//!     pub fn response_for_range(
//!         &self,
//!         range: Option<&str>,
//!     ) -> Option<BreakerHttpResponse>;
//!     /// Fake-clock advance for deterministic tests.
//!     pub fn advance(&mut self, duration: Duration);
//! }
//! ```
//!
//! Extend `workers/stream/src/nntp.rs` (existing module, no new file):
//!
//! ```text
//! pub enum NntpError {
//!     ...
//!     /// The fetch exceeded the per-upstream timeout and was abandoned.
//!     /// A timeout indicts the provider connection.
//!     Timeout(String),
//! }
//! // NntpError::Timeout(_).is_provider_failure() == true.
//!
//! impl NntpClient {
//!     /// Like article(), but abandons the fetch after `timeout`
//!     /// (message-id validation still applies first: a bad id returns
//!     /// InvalidMessageId without touching the wire).
//!     pub fn article_with_timeout(
//!         &mut self,
//!         message_id: &str,
//!         timeout: Duration,
//!     ) -> Result<Vec<u8>, NntpError>;
//! }
//! ```
//!
//! Wire the breaker into the play path so a Range fetch arriving while the
//! breaker is open fails fast with the quarantined error the HTTP layer
//! renders as 503 + `Retry-After` (no dial, no hang, pool slot freed).
//!
//! EXPECTED STATE: this file does NOT compile until the build lands
//! (unresolved `breaker` import, missing `Timeout` variant). That red
//! target is the acceptance proof; `--lib` and `stream_integration` stay
//! green around it.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use nntune_stream::breaker::{
    BreakerConfig, BreakerHttpResponse, BreakerState, CircuitBreaker, FAILURE_THRESHOLD,
    FAILURE_WINDOW_SECS, OPEN_SECS, UPSTREAM_TIMEOUT,
};
use nntune_stream::nntp::{NntpClient, NntpError};

/// Timeout plus test margin: the hung fetch must be abandoned well inside
/// this bound (the pre-build client blocks ~10 s on its fixed read timeout).
const ABANDON_MARGIN: Duration = Duration::from_secs(2);

/// Pinned contract numbers (mirrored in the worker README by the build).
#[test]
fn breaker_contract_numbers_are_pinned() {
    assert_eq!(UPSTREAM_TIMEOUT, Duration::from_millis(750));
    assert_eq!(FAILURE_THRESHOLD, 3);
    assert_eq!(FAILURE_WINDOW_SECS, 60);
    assert_eq!(OPEN_SECS, 30);
    let defaults = BreakerConfig::default();
    assert_eq!(defaults.timeout, UPSTREAM_TIMEOUT);
    assert_eq!(defaults.failure_threshold, FAILURE_THRESHOLD);
    assert_eq!(defaults.window_secs, FAILURE_WINDOW_SECS);
    assert_eq!(defaults.open_secs, OPEN_SECS);
}

/// A provider that greets then never answers `BODY`: the fetch must be
/// abandoned at the per-upstream timeout, classified as a provider failure
/// (so the breaker counts it), and never hang the caller.
#[test]
fn hung_upstream_fetch_exceeds_timeout_and_is_abandoned() {
    let server = HungNntp::start();
    let mut client = NntpClient::connect(&server.addr).expect("greeted hung upstream");

    let t0 = Instant::now();
    let err = client
        .article_with_timeout("<seg0@synthetic.test>", UPSTREAM_TIMEOUT)
        .expect_err("hung fetch must be abandoned, not served");
    let elapsed = t0.elapsed();

    assert!(
        matches!(err, NntpError::Timeout(_)),
        "hung fetch must surface as a timeout, got {err:?}"
    );
    assert!(
        err.is_provider_failure(),
        "a hung upstream indicts the provider connection"
    );
    assert!(
        elapsed < UPSTREAM_TIMEOUT + ABANDON_MARGIN,
        "abandoned fetch must return near the timeout, took {elapsed:?}"
    );
}

/// N consecutive failures inside the window open the breaker; while open,
/// every Range fetch (valid Range or not) is answered 503 + `Retry-After`
/// without touching the provider.
#[test]
fn n_consecutive_failures_open_the_breaker_and_range_fetch_serves_503_with_retry_after() {
    let mut breaker = CircuitBreaker::new(BreakerConfig::default());

    // Below threshold: still closed, fetches proceed.
    for _ in 0..FAILURE_THRESHOLD - 1 {
        breaker.record_failure();
        assert_eq!(breaker.state(), BreakerState::Closed);
        assert!(breaker.should_allow(), "below threshold stays closed");
    }

    // The Nth consecutive failure opens the breaker.
    breaker.record_failure();
    assert_eq!(breaker.state(), BreakerState::Open);
    assert!(
        !breaker.should_allow(),
        "open breaker must refuse the fetch without dialing"
    );
    assert_eq!(breaker.http_status(), Some(503));
    assert_eq!(breaker.retry_after_secs(), Some(OPEN_SECS));

    // 503 + Retry-After on Range fetch: pinned for a valid Range and for
    // a missing one alike (the breaker answers before Range resolution).
    for range in [Some("bytes=0-99"), None] {
        let resp: Option<BreakerHttpResponse> = breaker.response_for_range(range);
        let resp = resp.expect("open breaker must answer the Range fetch");
        assert_eq!(resp.status, 503);
        assert_eq!(resp.retry_after_secs, Some(OPEN_SECS));
    }

    // Failures spaced past the window do not accumulate: a fresh breaker
    // with two failures, a quiet window, then one more stays closed.
    let mut windowed = CircuitBreaker::new(BreakerConfig::default());
    windowed.record_failure();
    windowed.record_failure();
    windowed.advance(Duration::from_secs(FAILURE_WINDOW_SECS + 1));
    windowed.record_failure();
    assert_eq!(
        windowed.state(),
        BreakerState::Closed,
        "failures outside the window must not trip the breaker"
    );

    // A success resets the count: threshold-1 failures, a success, then
    // threshold-1 more failures still leave the breaker closed.
    let mut reset = CircuitBreaker::new(BreakerConfig::default());
    for _ in 0..FAILURE_THRESHOLD - 1 {
        reset.record_failure();
    }
    reset.record_success();
    for _ in 0..FAILURE_THRESHOLD - 1 {
        reset.record_failure();
    }
    assert_eq!(
        reset.state(),
        BreakerState::Closed,
        "success must reset the consecutive-failure count"
    );
}

/// After the open period elapses the breaker admits exactly one half-open
/// probe; a probe success closes it and Range fetches flow again, while a
/// probe failure re-opens it for a fresh period.
#[test]
fn half_open_probe_success_closes_the_breaker() {
    let mut breaker = CircuitBreaker::new(BreakerConfig::default());
    for _ in 0..FAILURE_THRESHOLD {
        breaker.record_failure();
    }
    assert_eq!(breaker.state(), BreakerState::Open);

    breaker.advance(Duration::from_secs(OPEN_SECS));
    assert_eq!(breaker.state(), BreakerState::HalfOpen);
    assert!(
        breaker.should_allow(),
        "half-open must admit exactly one probe"
    );
    assert!(
        !breaker.should_allow(),
        "a second caller while the probe is outstanding still gets 503"
    );

    breaker.record_success();
    assert_eq!(breaker.state(), BreakerState::Closed);
    assert!(breaker.should_allow(), "closed breaker admits fetches");
    assert_eq!(breaker.http_status(), None);
    assert_eq!(breaker.retry_after_secs(), None);
    assert!(
        breaker.response_for_range(Some("bytes=0-99")).is_none(),
        "closed breaker lets the Range fetch through to the normal path"
    );

    // A failed probe re-opens for a fresh period with a fresh Retry-After.
    for _ in 0..FAILURE_THRESHOLD {
        breaker.record_failure();
    }
    assert_eq!(breaker.state(), BreakerState::Open);
    breaker.advance(Duration::from_secs(OPEN_SECS));
    assert_eq!(breaker.state(), BreakerState::HalfOpen);
    assert!(breaker.should_allow(), "re-opened breaker probes again");
    breaker.record_failure();
    assert_eq!(breaker.state(), BreakerState::Open);
    assert_eq!(breaker.retry_after_secs(), Some(OPEN_SECS));
    let resp = breaker
        .response_for_range(Some("bytes=0-99"))
        .expect("re-opened breaker must answer 503");
    assert_eq!((resp.status, resp.retry_after_secs), (503, Some(OPEN_SECS)));
}

/// Loopback fake that greets new connections and then never answers `BODY`
/// (the hung-upstream fixture). Per-connection work runs on a thread that
/// idles on the stop flag so dropping the server never blocks the test.
struct HungNntp {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl HungNntp {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind hung NNTP");
        let addr = listener.local_addr().expect("local addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = Arc::clone(&stop);
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
                let stop_conn = Arc::clone(&stop_c);
                thread::spawn(move || {
                    let mut writer = stream.try_clone().expect("clone hung conn");
                    let _ = writer.write_all(b"200 hung upstream ready\r\n");
                    let _ = writer.flush();
                    let mut reader = BufReader::new(stream);
                    let mut line = Vec::new();
                    // Read one command (the BODY), then hang: idle until the
                    // test ends instead of sleeping a fixed span, so server
                    // shutdown is always prompt.
                    let _ = reader.read_until(b'\n', &mut line);
                    while !stop_conn.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(50));
                    }
                });
            }
        });
        Self {
            addr,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for HungNntp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop promptly.
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
