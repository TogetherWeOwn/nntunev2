//! Acceptance: the native (blocking) NNTP adapter must validate a
//! three-ASCII-digit status token plus a permitted separator/framing
//! (end-of-line or a single space before reply text) BEFORE classifying a
//! greeting or `BODY` reply.
//!
//! Malformed prefixes that merely *start with* a known code — `2000`,
//! `201x`, `2220`, `4800`, `481x`, `4230`, `430x` — are protocol failures
//! (`UnexpectedResponse`, provider-failure), never a successful greeting,
//! a successful body, a stale-credential signal, or a missing article.
//! Malformed diagnostics must stay bounded and generic: no echo of the
//! untrusted reply text and no credential-like material.
//!
//! Legitimate replies keep working: `200`/`201` greetings (with text and
//! bare), `222` body success, `480`/`481` stale credentials
//! (`AuthExpired`, not a provider failure, `provider_auth_expired` code),
//! `423`/`430` missing article (`NoSuchArticle`, not a provider failure),
//! and `482` on `BODY` as a generic protocol error.
//!
//! Scripted bounded loopback peers only; no external network, no real
//! credentials. Public seam: `nntune_stream::nntp::NntpClient`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use nntune_stream::nntp::{NntpClient, NntpError};

/// Untrusted reply text that malformed diagnostics must never echo.
const SENTINEL: &str = "UNTRUSTED-SENTINEL-9f3a";
/// Synthetic credential-like text that must never surface in diagnostics.
const FAKE_CREDENTIAL: &str = "synthetic-credential-hunter2";

fn malformed(line_prefix: &str) -> Vec<u8> {
    format!("{line_prefix} {SENTINEL} {FAKE_CREDENTIAL}\r\n").into_bytes()
}

/// Connect against a peer that sends `greeting` and then only drains.
/// Returns the connect outcome (`Ok(())` after dropping the client).
fn connect_with_greeting(greeting: &[u8]) -> Result<(), NntpError> {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
    let addr = listener.local_addr().expect("local addr");
    let greeting = greeting.to_vec();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        stream.write_all(&greeting).expect("greeting");
        stream.flush().expect("flush greeting");
        let mut buf = [0u8; 1024];
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
    });

    let outcome = NntpClient::connect(&addr).map(drop);
    server.join().expect("server thread");
    outcome
}

/// One `BODY` round-trip: peer greets `200`, reads the command, answers
/// with `reply`. Returns the client outcome and the command line seen.
fn fetch_with_reply(reply: &[u8]) -> (Result<Vec<u8>, NntpError>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
    let addr = listener.local_addr().expect("local addr");
    let reply = reply.to_vec();
    let (tx, rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept client");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeouts");
        let mut reader = BufReader::new(stream.try_clone().expect("clone conn"));
        let mut writer = stream;
        writer
            .write_all(b"200 synthetic NNTP ready\r\n")
            .expect("greeting");
        let mut command = String::new();
        reader.read_line(&mut command).expect("BODY command");
        writer.write_all(&reply).expect("scripted reply");
        writer.flush().expect("flush");
        // Drain until the client hangs up (bounded by the read timeout) so
        // a client that wrongly continues a body read cannot deadlock us.
        let mut sink = [0u8; 1024];
        loop {
            match reader.read(&mut sink) {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        tx.send(command).expect("report command");
    });

    let mut client = NntpClient::connect(&addr).expect("greeting arrives");
    let outcome = client.article("seg0@synthetic.test");
    drop(client);
    let command = rx.recv_timeout(Duration::from_secs(5)).expect("command");
    server.join().expect("server thread");
    (outcome, command)
}

fn assert_protocol_failure(err: &NntpError, ctx: &str) {
    assert!(
        matches!(err, NntpError::UnexpectedResponse(_)),
        "{ctx}: malformed status must be a protocol failure (UnexpectedResponse), got {err:?}"
    );
    assert!(
        err.is_provider_failure(),
        "{ctx}: a protocol failure is a provider failure, got {err:?}"
    );
    assert_eq!(
        err.code(),
        "request_failed",
        "{ctx}: malformed status must not carry the provider_auth_expired code"
    );
}

fn assert_no_untrusted_echo(err: &NntpError, ctx: &str) {
    let rendered = format!("{err:?}");
    assert!(
        !rendered.contains(SENTINEL),
        "{ctx}: diagnostic echoes untrusted reply text: {rendered}"
    );
    assert!(
        !rendered.contains(FAKE_CREDENTIAL),
        "{ctx}: diagnostic echoes credential-like material: {rendered}"
    );
    assert!(
        rendered.len() < 256,
        "{ctx}: diagnostic must stay bounded, got {} chars: {rendered}",
        rendered.len()
    );
}

#[test]
fn legitimate_greetings_connect() {
    for greeting in [
        &b"200 synthetic NNTP ready\r\n"[..],
        &b"201 synthetic NNTP ready, no posting\r\n"[..],
        &b"200\r\n"[..],
        &b"201\r\n"[..],
    ] {
        let outcome = connect_with_greeting(greeting);
        assert!(
            outcome.is_ok(),
            "legitimate greeting {greeting:?} must connect, got {outcome:?}"
        );
    }
}

#[test]
fn malformed_greetings_are_protocol_failures_not_connections() {
    // `2000` / `201x` (and their symmetric forms) only *start with* a
    // greeting code; the fourth byte is neither end-of-line nor a space.
    for prefix in ["2000", "200x", "2010", "201x"] {
        let outcome = connect_with_greeting(&malformed(prefix));
        let err = outcome.expect_err(&format!("malformed greeting {prefix} must not connect"));
        assert_protocol_failure(&err, &format!("greeting {prefix}"));
        assert_no_untrusted_echo(&err, &format!("greeting {prefix}"));
    }
}

#[test]
fn legitimate_222_returns_body() {
    for reply in [
        &b"222 article follows\r\nHello\r\n.\r\n"[..],
        &b"222\r\nHello\r\n.\r\n"[..],
    ] {
        let (outcome, command) = fetch_with_reply(reply);
        assert_eq!(command, "BODY <seg0@synthetic.test>\r\n");
        assert_eq!(
            outcome.expect("legitimate 222 must fetch"),
            b"Hello\n",
            "222 body must be returned dot-unstuffed"
        );
    }
}

#[test]
fn malformed_222_is_not_a_successful_body() {
    // A scripted body block follows the malformed status: treating the
    // prefix as `222` would consume it and wrongly succeed.
    for prefix in ["2220", "222x"] {
        let mut reply = malformed(prefix);
        reply.extend_from_slice(b"Hello\r\n.\r\n");
        let (outcome, _) = fetch_with_reply(&reply);
        let err = outcome.expect_err(&format!(
            "malformed BODY status {prefix} must not return a body"
        ));
        assert_protocol_failure(&err, &format!("BODY {prefix}"));
        assert_no_untrusted_echo(&err, &format!("BODY {prefix}"));
    }
}

#[test]
fn legitimate_480_481_are_stale_credentials_not_outages() {
    for reply in [
        &b"480 authentication required\r\n"[..],
        &b"481 authentication rejected\r\n"[..],
        &b"480\r\n"[..],
        &b"481\r\n"[..],
    ] {
        let (outcome, _) = fetch_with_reply(reply);
        let err = outcome.expect_err("480/481 must fail the fetch");
        assert!(
            matches!(err, NntpError::AuthExpired),
            "reply {reply:?} must be the stale-credential signal, got {err:?}"
        );
        assert!(
            !err.is_provider_failure(),
            "reply {reply:?}: stale credentials are not a provider failure"
        );
        assert_eq!(err.code(), "provider_auth_expired");
    }
}

#[test]
fn malformed_480_481_do_not_prompt_for_credentials() {
    for prefix in ["4800", "480x", "4810", "481x"] {
        let (outcome, _) = fetch_with_reply(&malformed(prefix));
        let err = outcome.expect_err(&format!("malformed BODY status {prefix} must fail"));
        assert!(
            !matches!(err, NntpError::AuthExpired),
            "BODY {prefix}: malformed prefix must not prompt credential re-entry, got {err:?}"
        );
        assert_protocol_failure(&err, &format!("BODY {prefix}"));
        assert_no_untrusted_echo(&err, &format!("BODY {prefix}"));
    }
}

#[test]
fn legitimate_423_430_are_missing_articles_not_outages() {
    for reply in [
        &b"423 no such article in group\r\n"[..],
        &b"430 no such article\r\n"[..],
        &b"423\r\n"[..],
        &b"430\r\n"[..],
    ] {
        let (outcome, _) = fetch_with_reply(reply);
        let err = outcome.expect_err("423/430 must fail the fetch");
        assert!(
            matches!(err, NntpError::NoSuchArticle(_)),
            "reply {reply:?} must be a missing article, got {err:?}"
        );
        assert!(
            !err.is_provider_failure(),
            "reply {reply:?}: a missing article is not a provider failure"
        );
    }
}

#[test]
fn malformed_423_430_are_not_missing_articles() {
    for prefix in ["4230", "423x", "4300", "430x"] {
        let (outcome, _) = fetch_with_reply(&malformed(prefix));
        let err = outcome.expect_err(&format!("malformed BODY status {prefix} must fail"));
        assert!(
            !matches!(err, NntpError::NoSuchArticle(_)),
            "BODY {prefix}: malformed prefix must not be read as a missing article, got {err:?}"
        );
        assert_protocol_failure(&err, &format!("BODY {prefix}"));
        assert_no_untrusted_echo(&err, &format!("BODY {prefix}"));
    }
}

#[test]
fn legitimate_482_on_body_remains_generic_protocol_error() {
    // RFC 4643 reserves 482 for AUTHINFO; on BODY it is out-of-sequence
    // and deliberately not the stale-credential signal.
    let (outcome, _) = fetch_with_reply(b"482 authentication out of sequence\r\n");
    let err = outcome.expect_err("482 on BODY must fail the fetch");
    assert!(
        !matches!(err, NntpError::AuthExpired),
        "482 on BODY must not prompt credential re-entry, got {err:?}"
    );
    assert_protocol_failure(&err, "BODY 482");
}

#[test]
fn well_formed_unexpected_status_remains_protocol_failure() {
    // Control: a well-formed but unhandled code keeps its existing
    // classification — token validation must not turn it into success,
    // stale credentials, or a missing article.
    let (outcome, _) = fetch_with_reply(b"500 unknown command\r\n");
    let err = outcome.expect_err("500 on BODY must fail the fetch");
    assert_protocol_failure(&err, "BODY 500");
}
