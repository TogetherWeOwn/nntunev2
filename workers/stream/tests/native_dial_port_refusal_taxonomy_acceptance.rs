//! Acceptance: the native-dial port refusal must surface through the
//! play error taxonomy as the stable `transport_port_not_allowed` code,
//! not as a generic I/O failure whose message callers must string-match.
//!
//! The endpoint-policy gate itself already exists on the native dial
//! (see `native_play_dial_endpoint_policy_acceptance.rs`): a non-loopback
//! dial on an off-allowlist port is refused before any TCP connection.
//! What is missing — and what this file pins — is the *taxonomy*: today
//! that refusal is a generic `NntpError::Io` carrying the code only inside
//! its message text, so `NntpError::code()` and `PlayError::code()` both
//! report the generic `request_failed`, the play envelope reports
//! `upstream_unavailable`, and the refusal is counted as a provider
//! failure that can quarantine the pool. The pinned/pool path instead
//! carries the refusal as `TransportRejection::PortNotAllowed`, whose
//! typed `code()` accessor returns `transport_port_not_allowed`.
//!
//! Contract pinned for the paired build (observable surface only, so this
//! file compiles unchanged both before and after the fix):
//!
//! - On the native client, a non-loopback off-allowlist dial fails with
//!   an error whose typed `code()` accessor returns exactly
//!   `transport_port_not_allowed` — the same value
//!   `TransportRejection::PortNotAllowed(port).code()` returns in core —
//!   for every off-allowlist port. The refusal is distinguishable without
//!   string matching: it is not the generic `NntpError::Io` variant.
//! - Through the play path, the same refusal is a dedicated play-taxonomy
//!   case (not the generic `PlayError::Upstream`): `PlayError::code()`
//!   and the user-facing envelope code (`api_error().code`) both return
//!   exactly `transport_port_not_allowed`, the envelope is non-retryable
//!   (redialing the same endpoint can never succeed), and
//!   `http_status()` agrees with the envelope status. Adding the
//!   envelope code to the stream registry and `docs/api-errors.md` is
//!   part of the paired build.
//! - A port refusal is a local policy verdict, not provider ill-health:
//!   the provider was never contacted. It must not count as a provider
//!   failure (`is_provider_failure()` is false), must not emit
//!   `ProviderError` / `QuarantineEntered` meter events, and repeated
//!   refusals must never quarantine the pool or mask the code behind a
//!   quarantined-pool error — every attempt keeps surfacing the port
//!   code (the same rationale as the stale-credentials taxonomy case).
//! - Off-allowlist ports still never reach `TcpStream::connect_timeout`:
//!   tripwire listeners on this host's non-loopback address observe no
//!   connection from either the native connect or the play path.
//! - Nothing else moves: allowlisted provider ports (`119`, `563`) are
//!   never port-refused, genuine I/O failures keep the generic
//!   `request_failed` code, and the loopback ephemeral-port harness
//!   contract (direct connect and full play) stays intact.
//!
//! Deliberately not pinned: the new variant names and payloads, the
//! exact HTTP status paired with the envelope code (only its consistency
//! with `http_status()` and its non-retryable flag), any dedicated meter
//! kind for the refusal (one may be added or not, but it may not be
//! `ProviderError` / `QuarantineEntered`), and the pool's
//! observability `pool_error_code()` for this case.
//!
//! Failing command on the base code:
//! `cargo test -p nntune-stream --test native_dial_port_refusal_taxonomy_acceptance`

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use nntune_core::pinned_transport::{TransportRejection, ALLOWED_NNTP_PORTS};
use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::{MemoryMeter, MeterKind};
use nntune_stream::nntp::{NntpClient, NntpError, UpstreamTimeouts};
use nntune_stream::play::{play_with_timeouts, PlayError, PlayInput, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"native-dial-port-refusal-taxonomy-secret";
const NOW_UNIX: u64 = 1_700_000_000;
// Within the grant TTL cap (`MAX_GRANT_TTL` = 900 s).
const EXP_UNIX: u64 = NOW_UNIX + 900;
const CIPHER_KEY: u8 = 0x5A;
const PORT_CODE: &str = "transport_port_not_allowed";
/// RFC 5737 TEST-NET-1: documentation range, never a real provider; a
/// dial to it can only fail at the network layer, so any fast, coded
/// refusal must come from endpoint policy, not the peer.
const TEST_NET_HOST: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 7);

fn short_timeouts() -> UpstreamTimeouts {
    UpstreamTimeouts::new(Duration::from_millis(600), Duration::from_millis(300))
}

fn connect_err(addr: &SocketAddr) -> (NntpError, Duration) {
    let started = Instant::now();
    let err = match NntpClient::connect_with_timeouts(addr, short_timeouts()) {
        Ok(_) => panic!("dialing {addr} must fail in this harness, not connect"),
        Err(err) => err,
    };
    (err, started.elapsed())
}

/// This host's primary non-loopback IPv4 address. UDP `connect` only
/// selects a route (no traffic is sent), so this works without any
/// reachable peer.
fn nonloopback_local_ipv4() -> Ipv4Addr {
    let sock = UdpSocket::bind("0.0.0.0:0").expect("bind route-probe socket");
    sock.connect((TEST_NET_HOST, 9))
        .expect("UDP connect must select a route");
    match sock.local_addr().expect("route-probe local addr").ip() {
        IpAddr::V4(ip) => {
            assert!(
                !ip.is_loopback() && !ip.is_unspecified(),
                "route probe must yield a non-loopback local address, got {ip}"
            );
            ip
        }
        other => panic!("route probe must yield IPv4 in this harness, got {other}"),
    }
}

/// A counting NNTP-ish server: accepts, greets `200`, answers one `BODY`
/// with `430`, then closes. Used as the tripwire behind a refused dial —
/// if the dial happens at all, `accepted` moves.
struct TripwireServer {
    port: u16,
    accepted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TripwireServer {
    fn start_on_first_free(ports: &[u16]) -> Self {
        let mut last_err = None;
        for &port in ports {
            match TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)) {
                Ok(listener) => return Self::serve(listener, port),
                Err(e) => last_err = Some(e),
            }
        }
        panic!("no tripwire port bindable from {ports:?}: {last_err:?}");
    }

    fn serve(listener: TcpListener, port: u16) -> Self {
        listener
            .set_nonblocking(true)
            .expect("nonblocking tripwire");
        let accepted = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (accepted_c, stop_c) = (Arc::clone(&accepted), Arc::clone(&stop));
        let handle = thread::spawn(move || {
            while !stop_c.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                accepted_c.fetch_add(1, Ordering::SeqCst);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.write_all(b"200 tripwire ready\r\n");
                let mut reader = BufReader::new(stream);
                let mut line = Vec::new();
                if reader.read_until(b'\n', &mut line).is_ok() {
                    let mut writer = reader.into_inner();
                    let _ = writer.write_all(b"430 no such article\r\n");
                }
            }
        });
        Self {
            port,
            accepted,
            stop,
            handle: Some(handle),
        }
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for TripwireServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn play_err(
    pool: &mut StreamPool,
    cache: &mut ContentCache,
    meter: &mut MemoryMeter,
    addr: SocketAddr,
    track_id: &str,
) -> PlayError {
    let token = grant::issue("user-1", track_id, EXP_UNIX, SECRET);
    let segments = [format!("{track_id}-seg0@synthetic.test")];
    let input = PlayInput {
        trace_id: None,
        token: &token,
        track_id,
        range: None,
        server_addr: addr,
        segments: &segments,
        segment_ranges: None,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    match play_with_timeouts(pool, cache, meter, SECRET, &input, short_timeouts()) {
        Ok(_) => panic!("play to {addr} must be refused, not served"),
        Err(err) => err,
    }
}

/// The stable code must ride the typed accessor — the same value core's
/// pinned-transport rejection reports — not merely appear inside a
/// free-text message. A caller comparing `err.code()` must be able to
/// recognize the refusal without parsing debug or message text.
#[test]
fn native_connect_port_refusal_carries_transport_code_on_code_accessor() {
    for port in [80u16, 25, 8080] {
        assert!(
            !ALLOWED_NNTP_PORTS.contains(&port),
            "{port} must stay off-allowlist for this test to mean anything"
        );
        let addr = SocketAddr::from((TEST_NET_HOST, port));
        let (err, elapsed) = connect_err(&addr);
        assert_eq!(
            err.code(),
            PORT_CODE,
            "port {port}: the typed code accessor must carry the stable \
             transport code, got {err:?} with code {}",
            err.code()
        );
        assert_eq!(
            err.code(),
            TransportRejection::PortNotAllowed(port).code(),
            "port {port}: native refusal must carry the same code the \
             pinned/pool path emits"
        );
        assert!(
            !matches!(err, NntpError::Io(_)),
            "port {port}: the refusal must be distinguishable without \
             string matching, not the generic Io variant, got {err:?}"
        );
        assert!(
            !err.is_provider_failure(),
            "port {port}: a local policy refusal never contacted the \
             provider and must not count as its failure, got {err:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "port {port}: policy refusal must not wait on the network, took {elapsed:?}"
        );
    }
}

/// No-dial proof on the native funnel: the coded refusal lands before
/// `TcpStream::connect_timeout`, so a listener on that exact port (via
/// this host's non-loopback address) observes no connection.
#[test]
fn native_connect_port_refusal_never_reaches_tcp_dial() {
    let tripwire = TripwireServer::start_on_first_free(&[8080, 18080, 28080, 38080]);
    assert!(
        !ALLOWED_NNTP_PORTS.contains(&tripwire.port),
        "tripwire port must be off-allowlist"
    );
    let addr = SocketAddr::from((nonloopback_local_ipv4(), tripwire.port));
    let (err, elapsed) = connect_err(&addr);
    // Give any erroneous dial a chance to land before reading the tripwire.
    thread::sleep(Duration::from_millis(300));

    assert_eq!(
        err.code(),
        PORT_CODE,
        "tripwire refusal must carry the stable code, got {err:?}"
    );
    assert_eq!(
        tripwire.accepted(),
        0,
        "refusal must happen before any TCP dial: the listening socket \
         observed a connection"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "policy refusal must be immediate, not a dial timeout, took {elapsed:?}"
    );
}

/// Through the play path, the refusal is a first-class taxonomy case:
/// dedicated variant (name unpinned), stable code on both the typed
/// accessor and the user-facing envelope, non-retryable, with the HTTP
/// status surfaces in agreement — and still no dial.
#[test]
fn play_port_refusal_surfaces_through_play_error_taxonomy() {
    let tripwire = TripwireServer::start_on_first_free(&[8080, 18080, 28080, 38080]);
    let addr = SocketAddr::from((nonloopback_local_ipv4(), tripwire.port));

    let mut pool: StreamPool = Pool::new(2);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let started = Instant::now();
    let err = play_err(
        &mut pool,
        &mut cache,
        &mut meter,
        addr,
        "track-port-taxonomy",
    );
    let elapsed = started.elapsed();
    // Give any erroneous dial a chance to land before reading the tripwire.
    thread::sleep(Duration::from_millis(300));

    assert_eq!(
        err.code(),
        PORT_CODE,
        "play taxonomy must carry the stable transport code on its typed \
         accessor, got {err:?} with code {}",
        err.code()
    );
    assert!(
        !matches!(err, PlayError::Upstream(_)),
        "the refusal must be a dedicated play-taxonomy case, not the \
         generic upstream variant callers would have to string-match, \
         got {err:?}"
    );
    let api = err.api_error();
    assert_eq!(
        api.code, PORT_CODE,
        "the user-facing envelope must carry the stable transport code, \
         got {} for {err:?}",
        api.code
    );
    assert!(
        !api.retryable,
        "redialing a refused endpoint can never succeed: the envelope \
         must be non-retryable, got {api:?}"
    );
    assert_eq!(
        err.http_status(),
        api.status,
        "the Worker-shim status and the envelope status must agree"
    );
    assert_eq!(
        tripwire.accepted(),
        0,
        "play refusal must happen before any TCP dial: the listening \
         socket observed a connection"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "policy refusal must be immediate, not a dial timeout, took {elapsed:?}"
    );
    assert_eq!(
        pool.in_use(),
        0,
        "a refused play must not leak a pool lease"
    );
}

/// Repeated refusals keep surfacing the port code and never indict the
/// provider: no provider-error metering, no quarantine entry, and the
/// pool is never quarantined (which would mask later refusals behind a
/// quarantined-pool error).
#[test]
fn repeated_play_port_refusals_never_quarantine_or_meter_provider_error() {
    let addr = SocketAddr::from((TEST_NET_HOST, 8080));
    let mut pool: StreamPool = Pool::new(2);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    // More attempts than the breaker's consecutive-failure threshold, so
    // a build that counts refusals as provider failures trips here.
    for attempt in 0..4 {
        let err = play_err(&mut pool, &mut cache, &mut meter, addr, "track-port-repeat");
        assert_eq!(
            err.code(),
            PORT_CODE,
            "attempt {attempt}: every refusal must keep surfacing the \
             stable code, got {err:?}"
        );
        assert!(
            !matches!(err, PlayError::PoolQuarantined { .. }),
            "attempt {attempt}: refusals must never quarantine the pool, \
             got {err:?}"
        );
    }
    assert!(
        !meter.has_kind(MeterKind::ProviderError),
        "a policy refusal is not a provider error and must not meter as one"
    );
    assert!(
        !meter.has_kind(MeterKind::QuarantineEntered),
        "a policy refusal must never enter quarantine"
    );
    assert!(
        !pool.quarantined(),
        "the pool must stay healthy after repeated policy refusals"
    );
    assert_eq!(pool.in_use(), 0, "refused plays must not leak pool leases");
}

/// Allowlisted provider ports are never port-refused (kills a build that
/// refuses every non-loopback dial): the dial may fail against TEST-NET,
/// but the typed code is never the port code.
#[test]
fn allowlisted_provider_ports_are_never_port_refused_by_code() {
    for port in ALLOWED_NNTP_PORTS {
        let addr = SocketAddr::from((TEST_NET_HOST, port));
        let (err, _) = connect_err(&addr);
        assert_ne!(
            err.code(),
            PORT_CODE,
            "allowlisted port {port} must not be port-refused, got {err:?}"
        );
    }
}

/// One-shot loopback stub greeting `200`, then draining until hangup.
fn spawn_loopback_peer() -> (SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback peer");
    let addr = listener.local_addr().expect("loopback peer addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept loopback peer");
        let _ = stream.write_all(b"200 synthetic ready\r\n");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 256];
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });
    (addr, handle)
}

/// Loopback harness contract: an ephemeral-port loopback dial still
/// connects, and a genuine loopback I/O failure (closed port) keeps the
/// generic code — the builder must not rebrand every I/O error with the
/// transport code.
#[test]
fn loopback_harness_still_connects_and_generic_io_code_is_unchanged() {
    let (addr, peer) = spawn_loopback_peer();
    assert!(
        !ALLOWED_NNTP_PORTS.contains(&addr.port()),
        "fixture must prove the ephemeral-port harness case"
    );
    let client = NntpClient::connect_with_timeouts(&addr, short_timeouts())
        .expect("loopback harness dial must still connect");
    drop(client);
    let _ = peer.join();

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind temporary port");
    let closed = listener.local_addr().expect("temporary addr");
    drop(listener);
    let (err, _) = connect_err(&closed);
    assert_eq!(
        err.code(),
        "request_failed",
        "a genuine I/O failure keeps the generic code, got {err:?}"
    );
    assert_ne!(
        err.code(),
        PORT_CODE,
        "loopback dials are never port-refused"
    );
}

/// A small valid single-segment track (multipart-shaped yEnc, whole-file
/// CRC on the only part), mirroring the other stream fixtures.
fn small_track(tag: &str, len: usize) -> (Vec<u8>, Vec<String>, HashMap<String, Vec<u8>>) {
    let mut plain = Vec::with_capacity(len);
    for i in 0..len {
        plain.push(((i * 7919 + tag.len()) % 256) as u8);
    }
    let ciphered: Vec<u8> = plain.iter().map(|b| b ^ CIPHER_KEY).collect();
    let mid = format!("{tag}-seg0@synthetic.test");
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

/// Loopback NNTP stub serving `BODY` articles until stopped.
struct LoopbackNntp {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl LoopbackNntp {
    fn start(articles: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback NNTP");
        let addr = listener.local_addr().expect("loopback NNTP addr");
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
                Self::serve(stream, &articles);
            }
        });
        Self {
            addr,
            stop,
            handle: Some(handle),
        }
    }

    fn serve(stream: TcpStream, articles: &HashMap<String, Vec<u8>>) {
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
            match articles.get(mid) {
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
}

impl Drop for LoopbackNntp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Loopback harness through the full play path: an ephemeral-port
/// loopback endpoint still serves a track byte-identically — the
/// taxonomy work must not disturb the harness exemption.
#[test]
fn loopback_play_still_serves_track_bytes() {
    let (plain, segments, articles) = small_track("looptax", 20_000);
    let server = LoopbackNntp::start(articles);
    assert!(
        !ALLOWED_NNTP_PORTS.contains(&server.addr.port()),
        "fixture must prove the ephemeral-port harness case"
    );
    let mut pool: StreamPool = Pool::new(2);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let token = grant::issue("user-1", "track-loop-taxonomy", EXP_UNIX, SECRET);
    let input = PlayInput {
        trace_id: None,
        token: &token,
        track_id: "track-loop-taxonomy",
        range: None,
        server_addr: server.addr,
        segments: &segments,
        segment_ranges: None,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let response = play_with_timeouts(
        &mut pool,
        &mut cache,
        &mut meter,
        SECRET,
        &input,
        UpstreamTimeouts::new(Duration::from_secs(2), Duration::from_secs(2)),
    )
    .expect("loopback play must still serve");
    let mut stream = response.stream;
    let mut body = Vec::new();
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize must succeed");
    assert_eq!(
        body, plain,
        "loopback play must serve the track byte-identically"
    );
}
