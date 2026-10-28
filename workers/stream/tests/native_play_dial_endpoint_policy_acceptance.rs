//! Acceptance: the native play dial must be gated on the pinned NNTP
//! endpoint policy before any TCP dial.
//!
//! Contract pinned for the paired build (observable surface only, so this
//! file compiles unchanged both before and after the fix):
//!
//! - A provider (non-loopback) endpoint on an off-allowlist port (`80`,
//!   `25`, `8080`, …) is refused with the stable code
//!   `transport_port_not_allowed` *before* any TCP dial: a listening
//!   socket on that exact port must observe no connection attempt.
//! - The refusal is observable on both public shapes of the one native
//!   dial funnel: through `play_with_timeouts` (the play path, which
//!   dials via `NntpClient::connect_with_deadline`) and directly through
//!   `NntpClient::connect_with_timeouts` (which delegates to the same
//!   funnel). The code may surface in any public rendering of the error
//!   (debug text, `code()`, or the API envelope code) — variant and type
//!   names are deliberately not pinned.
//! - Allowlisted provider ports (`119`, `563`) are never port-refused:
//!   dialing one may fail for ordinary network reasons, but never with
//!   `transport_port_not_allowed` (a build that refuses everything fails
//!   here).
//! - The native dial must route through the shared core policy
//!   (`validate_nntp_endpoint` / `pinned_rejection_message` in
//!   `nntune-core::pinned_transport`), not a hand-rolled port list in the
//!   stream crate. That delegation is what keeps the vault-ack cohort
//!   semantics intact — acknowledged plaintext lifts only the TLS
//!   requirement, the port allowlist still applies — and those semantics
//!   are re-pinned against the core policy below so the build cannot
//!   weaken core to pass.
//! - The existing local contract is unchanged: loopback harness dials
//!   (ephemeral ports used by every synthetic-NNTP fixture) still dial,
//!   connect/read deadlines still bound a silent peer, and a warm pooled
//!   loopback connection is still reused across plays.
//!
//! Why loopback is the boundary: the native client is a plaintext TCP
//! client whose dial seam carries only a `SocketAddr` — no TLS or
//! acknowledgment input exists on `PlayInput` / `connect_with_timeouts`,
//! and their signatures must stay source-compatible (this file and every
//! existing fixture construct them literally). Provider endpoints are
//! non-loopback by definition; loopback is the local harness. On this
//! seam an off-allowlist provider port must surface the *port* code, so
//! the build must not let a TLS-first check mask it into
//! `transport_tls_required` here.
//!
//! Deliberately not pinned: whether a port refusal quarantines/meters
//! the pool (either is acceptable), and the exact error variant/type the
//! refusal is carried in.
//!
//! Failing command on the base code:
//! `cargo test -p nntune-stream --test native_play_dial_endpoint_policy_acceptance`

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use nntune_core::nntp::NntpConfig;
use nntune_core::pinned_transport::{
    pinned_rejection_message, validate_nntp_endpoint, TransportRejection, ALLOWED_NNTP_PORTS,
};
use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::MemoryMeter;
use nntune_stream::nntp::{NntpClient, NntpError, UpstreamTimeouts};
use nntune_stream::play::{play_with_timeouts, PlayError, PlayInput, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"native-dial-endpoint-policy-secret";
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

/// Every public rendering of a native NNTP failure, for code checks that
/// do not pin which surface carries the stable code.
fn nntp_rendering(err: &NntpError) -> String {
    format!("{err:?} {}", err.code())
}

/// Every public rendering of a play failure, same idea.
fn play_rendering(err: &PlayError) -> String {
    format!(
        "{err:?} {} {} {:?}",
        err.code(),
        err.api_error().code,
        err.pool_error_code()
    )
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

/// The play path must refuse an off-allowlist provider port before any
/// dial: coded error, and the tripwire listening on that exact port (via
/// this host's non-loopback address) observes no connection.
#[test]
fn play_refuses_off_allowlist_provider_port_before_any_dial() {
    let tripwire = TripwireServer::start_on_first_free(&[8080, 18080, 28080, 38080]);
    assert!(
        !ALLOWED_NNTP_PORTS.contains(&tripwire.port),
        "tripwire port must be off-allowlist"
    );
    let addr = SocketAddr::from((nonloopback_local_ipv4(), tripwire.port));

    let token = grant::issue("user-1", "track-dial-gate", EXP_UNIX, SECRET);
    let mut pool: StreamPool = Pool::new(2);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let segments = ["dial-gate-seg0@synthetic.test".to_string()];
    let input = PlayInput {
        trace_id: None,
        token: &token,
        track_id: "track-dial-gate",
        range: None,
        server_addr: addr,
        segments: &segments,
        segment_ranges: None,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let started = Instant::now();
    let err = match play_with_timeouts(
        &mut pool,
        &mut cache,
        &mut meter,
        SECRET,
        &input,
        short_timeouts(),
    ) {
        Ok(_) => panic!("play to an off-allowlist provider port must be refused, not served"),
        Err(err) => err,
    };
    let elapsed = started.elapsed();
    // Give any erroneous dial a chance to land before reading the tripwire.
    thread::sleep(Duration::from_millis(300));

    let rendering = play_rendering(&err);
    assert!(
        rendering.contains(PORT_CODE),
        "off-allowlist provider port must surface the stable {PORT_CODE} code, got {rendering}"
    );
    assert_eq!(
        tripwire.accepted(),
        0,
        "refusal must happen before any TCP dial: the listening socket observed a connection"
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

/// The same refusal directly on the native client funnel, for each
/// off-allowlist example port.
#[test]
fn native_connect_refuses_off_allowlist_provider_ports_with_stable_code() {
    for port in [80u16, 25, 8080] {
        assert!(
            !ALLOWED_NNTP_PORTS.contains(&port),
            "{port} must stay off-allowlist for this test to mean anything"
        );
        let addr = SocketAddr::from((TEST_NET_HOST, port));
        let (err, elapsed) = connect_err(&addr);
        let rendering = nntp_rendering(&err);
        assert!(
            rendering.contains(PORT_CODE),
            "port {port}: refusal must carry the stable {PORT_CODE} code, got {rendering}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "port {port}: policy refusal must not wait on the network, took {elapsed:?}"
        );
    }
}

/// Allowlisted provider ports are never port-refused (kills a build that
/// refuses every non-loopback dial): the dial may fail against TEST-NET,
/// but not with the port code.
#[test]
fn native_connect_does_not_port_refuse_allowlisted_provider_ports() {
    for port in ALLOWED_NNTP_PORTS {
        let addr = SocketAddr::from((TEST_NET_HOST, port));
        let (err, _) = connect_err(&addr);
        let rendering = nntp_rendering(&err);
        assert!(
            !rendering.contains(PORT_CODE),
            "allowlisted port {port} must not be port-refused, got {rendering}"
        );
    }
}

/// Vault-ack cohort semantics on the shared policy the native dial must
/// delegate to: acknowledgment lifts TLS only; the port allowlist still
/// applies, and an unacknowledged plaintext config is a TLS refusal on
/// any port (TLS checked first, so it never launders into a port code).
#[test]
fn acknowledged_plaintext_lifts_tls_only_port_allowlist_still_applies() {
    fn config(port: u16, tls: bool, ack: bool) -> NntpConfig {
        NntpConfig {
            host: "news.example".to_owned(),
            port,
            tls,
            allow_insecure_plaintext: ack,
            username: Some("policy-sentinel-user".to_owned()),
            password: Some("policy-sentinel-pass".to_owned()),
            timeout: Duration::from_millis(200),
            max_pipelined: 1,
            max_article_bytes: nntune_core::transport::MAX_ARTICLE_BYTES,
        }
    }

    // Acknowledged plaintext passes on allowlisted ports.
    for port in ALLOWED_NNTP_PORTS {
        let endpoint = validate_nntp_endpoint(&config(port, false, true))
            .expect("acknowledged plaintext on an allowlisted port must pass");
        assert_eq!(endpoint.port, port);
        assert!(pinned_rejection_message(&config(port, false, true)).is_none());
    }

    // …but the acknowledgment never lifts the port allowlist.
    for port in [80u16, 25, 8080] {
        assert_eq!(
            validate_nntp_endpoint(&config(port, false, true)),
            Err(TransportRejection::PortNotAllowed(port)),
            "acknowledged plaintext on port {port} must still be a port refusal"
        );
        let msg = pinned_rejection_message(&config(port, false, true))
            .expect("acknowledged off-allowlist config must render a rejection");
        assert!(msg.contains(PORT_CODE), "got {msg}");
        assert!(
            !msg.contains("policy-sentinel-user") && !msg.contains("policy-sentinel-pass"),
            "rejection must stay names-only, got {msg}"
        );
    }

    // Unacknowledged plaintext is a TLS refusal whatever the port.
    for port in [119u16, 80] {
        assert_eq!(
            validate_nntp_endpoint(&config(port, false, false)),
            Err(TransportRejection::TlsRequired),
            "unacknowledged plaintext on port {port} must be a TLS refusal"
        );
    }
}

/// Delegation pin: the native dial site must consult the shared core
/// policy (whose cohort semantics are pinned above), not a stream-local
/// port list that could drift from it. Comment-only lines are stripped
/// so a mention cannot satisfy the pin; pre-dial ordering is proven
/// behaviorally by the tripwire test, not by text position.
#[test]
fn native_dial_routes_through_shared_pinned_policy() {
    let source = include_str!("../src/nntp.rs");
    let code = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        code.contains("validate_nntp_endpoint") || code.contains("pinned_rejection_message"),
        "workers/stream/src/nntp.rs must gate its dial through nntune-core's \
         validate_nntp_endpoint / pinned_rejection_message"
    );
}

/// One-shot loopback stub: `greeting` decides what (if anything) the
/// peer hears on connect. Accepts exactly one connection.
fn spawn_loopback_peer(greeting: Option<&'static [u8]>) -> (SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback peer");
    let addr = listener.local_addr().expect("loopback peer addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept loopback peer");
        if let Some(bytes) = greeting {
            let _ = stream.write_all(bytes);
        }
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

/// Local contract: an ephemeral-port loopback harness dial still
/// connects and consumes the greeting.
#[test]
fn loopback_harness_dial_still_connects() {
    let (addr, peer) = spawn_loopback_peer(Some(b"200 synthetic ready\r\n"));
    assert!(
        !ALLOWED_NNTP_PORTS.contains(&addr.port()),
        "fixture must prove the ephemeral-port harness case"
    );
    let client = NntpClient::connect_with_timeouts(&addr, short_timeouts())
        .expect("loopback harness dial must still connect");
    drop(client);
    let _ = peer.join();
}

/// Local contract: loopback dials are still attempted — a closed
/// ephemeral loopback port yields the OS refusal, never the port code.
#[test]
fn loopback_closed_port_still_dials_without_port_code() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind temporary port");
    let addr = listener.local_addr().expect("temporary addr");
    drop(listener);
    let (err, elapsed) = connect_err(&addr);
    let rendering = nntp_rendering(&err);
    assert!(
        !rendering.contains(PORT_CODE),
        "loopback harness dial must not be port-refused, got {rendering}"
    );
    assert!(
        rendering.to_lowercase().contains("refused"),
        "loopback dial must reach the closed port (connection refused), got {rendering}"
    );
    assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
}

/// Timeout contract: a loopback peer that accepts but never greets is
/// bounded by the read/connect deadlines, not left hanging, and the
/// failure is not a port refusal.
#[test]
fn loopback_silent_peer_still_bounded_by_deadlines() {
    let (addr, peer) = spawn_loopback_peer(None);
    let (err, elapsed) = connect_err(&addr);
    let rendering = nntp_rendering(&err);
    assert!(
        !rendering.contains(PORT_CODE),
        "silent loopback peer must fail on deadlines, not port policy, got {rendering}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "deadlines must bound a silent peer, took {elapsed:?}"
    );
    drop(peer);
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

/// Loopback NNTP stub serving `BODY` articles, counting accepts so warm
/// reuse is observable.
struct CountingNntp {
    addr: SocketAddr,
    accepted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl CountingNntp {
    fn start(articles: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind counting NNTP");
        let addr = listener.local_addr().expect("counting NNTP addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let accepted = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (accepted_c, stop_c) = (Arc::clone(&accepted), Arc::clone(&stop));
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
                accepted_c.fetch_add(1, Ordering::SeqCst);
                Self::serve(stream, &articles);
            }
        });
        Self {
            addr,
            accepted,
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

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for CountingNntp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn play_ok(
    pool: &mut StreamPool,
    cache: &mut ContentCache,
    meter: &mut MemoryMeter,
    server: &CountingNntp,
    track_id: &str,
    segments: &[String],
) -> Vec<u8> {
    let token = grant::issue("user-1", track_id, EXP_UNIX, SECRET);
    let inp = PlayInput {
        trace_id: None,
        token: &token,
        track_id,
        range: None,
        server_addr: server.addr,
        segments,
        segment_ranges: None,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let response = play_with_timeouts(
        pool,
        cache,
        meter,
        SECRET,
        &inp,
        UpstreamTimeouts::new(Duration::from_secs(2), Duration::from_secs(2)),
    )
    .unwrap_or_else(|e| panic!("play of {track_id} must serve: {}", play_rendering(&e)));
    let mut stream = response.stream;
    let mut body = Vec::new();
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    stream
        .finalize(pool, cache, meter)
        .expect("finalize must succeed");
    body
}

/// Warm-reuse contract: two cache-miss plays for two tracks over the
/// same pool and the same (allowed, loopback) endpoint reuse one dialed
/// connection — the endpoint gate must not force a redial per play.
#[test]
fn warm_reuse_serves_second_track_over_one_loopback_connection() {
    let (plain_a, segs_a, mut articles) = small_track("warma", 20_000);
    let (plain_b, segs_b, articles_b) = small_track("warmb", 18_000);
    articles.extend(articles_b);
    let server = CountingNntp::start(articles);
    let mut pool: StreamPool = Pool::new(2);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();

    let body_a = play_ok(
        &mut pool,
        &mut cache,
        &mut meter,
        &server,
        "track-warm-a",
        &segs_a,
    );
    assert_eq!(body_a, plain_a);
    let body_b = play_ok(
        &mut pool,
        &mut cache,
        &mut meter,
        &server,
        "track-warm-b",
        &segs_b,
    );
    assert_eq!(body_b, plain_b);

    assert_eq!(
        server.accepted(),
        1,
        "second play must reuse the warm pooled connection, not redial"
    );
}
