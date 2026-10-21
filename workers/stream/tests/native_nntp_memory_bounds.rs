//! Native blocking adapter retained-byte acceptance (not the async pool BODY route).
//!
//! Policy matches nntune_core::transport's 16 KiB wire-line and 64 MiB article
//! ceilings. A wire line includes CRLF; the native article ceiling counts its
//! documented dot-unstuffed LF output, not core's CRLF output. No truncation.
//! Peers send finite fixtures, accept one connection and read at most 512 command
//! bytes. Unterminated fixtures hold EOF behind a release channel: an idle timeout
//! is NOT evidence of a byte refusal. Large cases run only on Linux, in a child
//! with a 512 MiB address-space limit and a hard 10-second wall-clock guard.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::MemoryMeter;
use nntune_stream::nntp::{NntpClient, NntpError, UpstreamTimeouts};
use nntune_stream::play::{play_with_timeouts, PlayError, PlayInput, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::yenc;

const LINE_BYTES: usize = 16 * 1024;
const ARTICLE_BYTES: usize = 64 * 1024 * 1024;
const DIAGNOSTIC_BYTES: usize = 256;
const ID: &str = "memory-bound@synthetic.invalid";
const GREETING: &[u8] = b"200 synthetic ready\r\n";
const STATUS: &[u8] = b"222 article follows\r\n";

struct Peer {
    addr: SocketAddr,
    release: Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl Peer {
    fn start(
        greeting: Vec<u8>,
        reply: impl FnOnce(&mut TcpStream) -> io::Result<()> + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind bounded peer");
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (release, released) = mpsc::channel();
        let handle = thread::spawn(move || {
            let started = Instant::now();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if started.elapsed() > Duration::from_secs(3) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => panic!("accept bounded peer: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            if stream.write_all(&greeting).is_err() {
                return; // Early close is legitimate for an oversize greeting.
            }
            let mut command = Vec::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap()).take(512);
            if let Ok(n) = reader.read_until(b'\n', &mut command) {
                if n > 0 {
                    assert_eq!(command, b"BODY <memory-bound@synthetic.invalid>\r\n");
                    // Size refusal may close the socket partway through the finite reply.
                    let _ = reply(&mut stream);
                }
            }
            // Keep the write half open until the client finishes, even for an
            // unterminated fixture. Drop releases this on assertion failure too.
            let _ = released.recv_timeout(Duration::from_secs(3));
        });
        Self {
            addr,
            release,
            handle: Some(handle),
        }
    }

    fn body(response: Vec<u8>) -> Self {
        Self::start(GREETING.to_vec(), move |stream| stream.write_all(&response))
    }

    fn connect(&self) -> Result<NntpClient, NntpError> {
        NntpClient::connect_with_timeouts(
            &self.addr,
            UpstreamTimeouts::new(Duration::from_secs(1), Duration::from_millis(300)),
        )
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.release.send(());
        if let Some(handle) = self.handle.take() {
            let result = handle.join();
            if !thread::panicking() {
                result.expect("bounded peer thread");
            }
        }
    }
}

fn status_line(code: &[u8], wire_bytes: usize) -> Vec<u8> {
    assert_eq!(code.len(), 3);
    assert!((6..=LINE_BYTES + 1).contains(&wire_bytes));
    let mut line = code.to_vec();
    line.push(b' ');
    line.resize(wire_bytes - 2, b's');
    line.extend_from_slice(b"\r\n");
    line
}

fn body_line(wire_bytes: usize, terminated: bool) -> Vec<u8> {
    assert!(wire_bytes <= LINE_BYTES + 1);
    let mut response = STATUS.to_vec();
    response.extend(std::iter::repeat_n(
        b'x',
        wire_bytes - if terminated { 2 } else { 0 },
    ));
    if terminated {
        response.extend_from_slice(b"\r\n.\r\n");
    }
    response
}

fn assert_bounded_provider_error(error: &NntpError) {
    assert!(
        error.is_provider_failure(),
        "size/protocol refusal must remain a provider failure"
    );
    assert_eq!(error.code(), "request_failed", "not an auth-expiry signal");
    let diagnostic = format!("{error:?}");
    assert!(
        diagnostic.len() <= DIAGNOSTIC_BYTES,
        "diagnostic retained {} bytes; bound is {DIAGNOSTIC_BYTES}",
        diagnostic.len()
    );
}

fn assert_size_refusal<T>(result: Result<T, NntpError>, context: &str) {
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("{context}: oversized input was accepted instead of refused"),
    };
    assert_bounded_provider_error(&error);
    let diagnostic = format!("{error:?}").to_ascii_lowercase();
    for wrong_reason in [
        "eof",
        "timed out",
        "temporarily unavailable",
        "would block",
        "deadline",
    ] {
        assert!(!diagnostic.contains(wrong_reason),
            "{context}: expected byte-ceiling refusal, not EOF or a time-budget failure: {diagnostic}");
    }
}

#[test]
fn greeting_at_exact_wire_line_limit_connects() {
    let peer = Peer::start(status_line(b"200", LINE_BYTES), |_| Ok(()));
    assert!(
        peer.connect().is_ok(),
        "exact-bound greeting must not be truncated or refused"
    );
}

#[test]
fn greeting_one_byte_over_wire_line_limit_refuses() {
    let peer = Peer::start(status_line(b"200", LINE_BYTES + 1), |_| Ok(()));
    assert_size_refusal(peer.connect(), "greeting at line cap + 1");
}

#[test]
fn unterminated_greeting_refuses_at_line_cap_without_waiting_for_eof() {
    let mut greeting = b"200 ".to_vec();
    greeting.resize(LINE_BYTES + 1, b's');
    let peer = Peer::start(greeting, |_| Ok(()));
    assert_size_refusal(peer.connect(), "unterminated greeting at cap + 1");
}

#[test]
fn body_status_at_exact_wire_line_limit_returns_complete_body() {
    let mut response = status_line(b"222", LINE_BYTES);
    response.extend_from_slice(b"complete\r\n.\r\n");
    let peer = Peer::body(response);
    let mut client = peer.connect().expect("greeting");
    assert_eq!(
        client.article(ID).expect("exact-bound status"),
        b"complete\n"
    );
}

#[test]
fn body_status_one_byte_over_wire_line_limit_refuses() {
    let mut response = status_line(b"222", LINE_BYTES + 1);
    response.extend_from_slice(b"must not return\r\n.\r\n");
    let peer = Peer::body(response);
    let mut client = peer.connect().expect("greeting");
    assert_size_refusal(client.article(ID), "BODY status at line cap + 1");
}

#[test]
fn body_line_at_exact_wire_limit_returns_every_byte() {
    let peer = Peer::body(body_line(LINE_BYTES, true));
    let mut client = peer.connect().expect("greeting");
    let body = client.article(ID).expect("exact-bound BODY line");
    assert_eq!(body.len(), LINE_BYTES - 1, "CRLF becomes one LF");
    assert!(body[..LINE_BYTES - 2].iter().all(|b| *b == b'x'));
    assert_eq!(body.last(), Some(&b'\n'));
}

#[test]
fn body_line_one_byte_over_wire_limit_refuses_instead_of_returning_partial_data() {
    let peer = Peer::body(body_line(LINE_BYTES + 1, true));
    let mut client = peer.connect().expect("greeting");
    assert_size_refusal(client.article(ID), "BODY data line at cap + 1");
}

#[test]
fn unterminated_body_line_refuses_at_cap_without_waiting_for_eof() {
    let peer = Peer::body(body_line(LINE_BYTES + 1, false));
    let mut client = peer.connect().expect("greeting");
    assert_size_refusal(client.article(ID), "unterminated BODY data line at cap + 1");
}

#[test]
fn exact_wire_limit_counts_stuffing_but_preserves_unstuffed_bytes() {
    let mut response = STATUS.to_vec();
    response.extend_from_slice(b"..");
    response.extend(std::iter::repeat_n(b'a', LINE_BYTES - 4));
    response.extend_from_slice(b"\r\n.\r\n");
    let peer = Peer::body(response);
    let mut client = peer.connect().expect("greeting");
    let body = client.article(ID).expect("exact-bound stuffed line");
    assert_eq!(body.len(), LINE_BYTES - 2);
    assert_eq!(body[0], b'.', "strip exactly one stuffing dot");
    assert!(body[1..body.len() - 1].iter().all(|b| *b == b'a'));
    assert_eq!(body.last(), Some(&b'\n'));
}

#[test]
fn ordinary_binary_dot_stuffing_and_empty_lines_are_preserved() {
    let peer = Peer::body(b"222 body follows\r\n\0\xff\r\n..dot\r\n...two\r\n\r\n.\r\n".to_vec());
    let mut client = peer.connect().expect("greeting");
    assert_eq!(client.article(ID).unwrap(), b"\0\xff\n.dot\n..two\n\n");
}

#[test]
fn malformed_greeting_diagnostic_is_bounded_even_below_line_limit() {
    let peer = Peer::start(status_line(b"500", 1024), |_| Ok(()));
    let error = match peer.connect() {
        Err(error) => error,
        Ok(_) => panic!("500 greeting must refuse"),
    };
    assert_bounded_provider_error(&error);
}

#[test]
fn malformed_body_status_diagnostic_is_bounded_even_below_line_limit() {
    let peer = Peer::body(status_line(b"500", 1024));
    let mut client = peer.connect().expect("greeting");
    let error = client.article(ID).expect_err("500 BODY status must refuse");
    assert_bounded_provider_error(&error);
}

#[test]
fn ordinary_missing_auth_and_provider_classifications_survive_the_limits() {
    for (reply, missing, auth) in [
        (b"430 not found\r\n".as_slice(), true, false),
        (b"423 not found\r\n".as_slice(), true, false),
        (b"480 authenticate\r\n".as_slice(), false, true),
        (b"481 rejected\r\n".as_slice(), false, true),
        (b"482 out of sequence\r\n".as_slice(), false, false),
        (b"500 failure\r\n".as_slice(), false, false),
    ] {
        let peer = Peer::body(reply.to_vec());
        let mut client = peer.connect().expect("greeting");
        let error = client.article(ID).expect_err("non-222 reply");
        assert_eq!(matches!(error, NntpError::NoSuchArticle(_)), missing);
        assert_eq!(matches!(error, NntpError::AuthExpired), auth);
        assert_eq!(error.is_provider_failure(), !missing && !auth);
    }
}

#[test]
fn oversized_body_status_is_not_success_cached_or_parked_warm() {
    // The body itself is valid yEnc. Ignoring the oversized status would make
    // the complete play succeed and park this connection warm (not Decode).
    let mut response = status_line(b"222", LINE_BYTES + 1);
    response.extend_from_slice(&yenc::encode(b"synthetic-audio", "fixture.flac"));
    response.extend_from_slice(b".\r\n");
    let peer = Peer::body(response);
    let mut pool: StreamPool = Pool::new(1);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let now = 1_700_000_000;
    let key = b"synthetic-memory-test-key";
    let token = grant::issue_at("memory-user", "memory-track", now, 60, key);
    let segments = vec![ID.to_string()];
    let input = PlayInput {
        trace_id: None,
        token: &token,
        track_id: "memory-track",
        range: None,
        server_addr: peer.addr,
        segments: &segments,
        segment_ranges: None,
        cipher_key: 0,
        now_unix: now,
        up_next: None,
    };
    let result = match play_with_timeouts(
        &mut pool,
        &mut cache,
        &mut meter,
        key,
        &input,
        UpstreamTimeouts::new(Duration::from_secs(1), Duration::from_millis(300)),
    ) {
        Ok(response) => {
            let mut stream = response.stream;
            for _ in stream.by_ref() {}
            stream.finalize(&mut pool, &mut cache, &mut meter)
        }
        Err(error) => Err(error),
    };
    assert_eq!(
        (pool.in_use(), pool.idle()),
        (0, 0),
        "an oversize/desynchronized connection must not be retained for warm reuse"
    );
    assert!(matches!(result, Err(PlayError::Upstream(_))),
        "oversized status must fail the play as a provider error, not success or Decode: {result:?}");
    assert!(
        cache.get_track("memory-track").is_none(),
        "no partial/success cache entry"
    );
}

#[cfg(target_os = "linux")]
fn guarded_article_case(name: &str, case: impl FnOnce()) {
    const CHILD: &str = "NATIVE_NNTP_MEMORY_CASE";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        case();
        return;
    }
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg("ulimit -v 524288 || exit 125; exec timeout --signal=KILL 10s \"$1\" --exact \"$2\" --test-threads=1 --nocapture")
        .arg("native-nntp-memory-guard")
        .arg(std::env::current_exe().expect("test executable"))
        .arg(name)
        .env(CHILD, name)
        .output()
        .expect("launch resource-bounded article case");
    assert!(
        matches!(output.status.code(), Some(0 | 101)),
        "resource guard/infrastructure termination is NOT assertion-level RED: {:?}, {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "guarded article assertion failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
fn short_line_article(one_byte_over: bool) -> Peer {
    Peer::start(GREETING.to_vec(), move |stream| {
        stream.write_all(STATUS)?;
        // Each 1026-byte wire line retains 1024 bytes: one dot + 1022 'a'
        // bytes + LF. 64 lines/block * 1024 blocks = exactly 64 MiB retained.
        let mut line = b"..".to_vec();
        line.extend(std::iter::repeat_n(b'a', 1022));
        line.extend_from_slice(b"\r\n");
        let block = line.repeat(64);
        for _ in 0..1024 {
            stream.write_all(&block)?;
        }
        if one_byte_over {
            stream.write_all(b"\r\n")?; // Empty data line retains exactly one LF.
        }
        stream.write_all(b".\r\n")
    })
}

#[cfg(target_os = "linux")]
#[test]
fn many_short_body_lines_at_exact_article_cap_return_every_unstuffed_byte() {
    guarded_article_case(
        "many_short_body_lines_at_exact_article_cap_return_every_unstuffed_byte",
        || {
            let peer = short_line_article(false);
            let mut client = NntpClient::connect_with_timeouts(
                &peer.addr,
                UpstreamTimeouts::new(Duration::from_secs(1), Duration::from_secs(5)),
            )
            .expect("greeting");
            let body = client.article(ID).expect("exact 64 MiB article");
            assert_eq!(
                body.len(),
                ARTICLE_BYTES,
                "no truncation at article boundary"
            );
            for line in body.as_chunks::<1024>().0 {
                assert_eq!(line[0], b'.');
                assert!(line[1..1023].iter().all(|b| *b == b'a'));
                assert_eq!(line[1023], b'\n');
            }
        },
    );
}

#[cfg(target_os = "linux")]
#[test]
fn many_short_body_lines_one_byte_over_article_cap_refuse() {
    guarded_article_case(
        "many_short_body_lines_one_byte_over_article_cap_refuse",
        || {
            let peer = short_line_article(true);
            let mut client = NntpClient::connect_with_timeouts(
                &peer.addr,
                UpstreamTimeouts::new(Duration::from_secs(1), Duration::from_secs(5)),
            )
            .expect("greeting");
            // Do not use expect_err: its Debug of an accidental Ok(Vec) would dump
            // 64 MiB into the assertion log instead of reporting the missing bound.
            let result = client.article(ID);
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("many short BODY lines at 64 MiB + 1: oversized input was accepted instead of refused"),
            };
            // An oversize article is content-local (one article too big says
            // nothing about provider health), so it refuses with the dedicated
            // non-provider error — never a provider failure — while staying
            // bounded and distinct from EOF/timeout failures.
            assert!(
                matches!(error, NntpError::ArticleTooLarge),
                "many short BODY lines at 64 MiB + 1: expected the dedicated article-cap refusal, got {error:?}"
            );
            assert!(
                !error.is_provider_failure(),
                "many short BODY lines at 64 MiB + 1: an oversize article must not indict the provider"
            );
            assert_eq!(error.code(), "request_failed", "not an auth-expiry signal");
            let diagnostic = format!("{error:?}");
            assert!(
                diagnostic.len() <= DIAGNOSTIC_BYTES,
                "diagnostic retained {} bytes; bound is {DIAGNOSTIC_BYTES}",
                diagnostic.len()
            );
            for wrong_reason in [
                "eof",
                "timed out",
                "temporarily unavailable",
                "would block",
                "deadline",
            ] {
                let lower = diagnostic.to_ascii_lowercase();
                assert!(!lower.contains(wrong_reason),
                    "many short BODY lines at 64 MiB + 1: expected byte-ceiling refusal, not EOF or a time-budget failure: {lower}");
            }
        },
    );
}
