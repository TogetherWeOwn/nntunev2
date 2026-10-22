//! HTTP framing pins: chunked request bodies (with extensions),
//! missing framing (411), and oversize JSON declarations and streams
//! (413). The encoder never runs here: PUT staging and JSON
//! pre-checks reject before any encode could start.

mod common;

use common::*;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

/// Read to clean EOF, not merely Content-Length: a reset after a valid
/// response is still a teardown regression and must fail this assertion.
fn closed_response(stream: &mut TcpStream) -> (u16, serde_json::Value) {
    let mut out = Vec::new();
    stream
        .read_to_end(&mut out)
        .expect("response must end without reset or timeout");
    let split = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = std::str::from_utf8(&out[..split]).unwrap();
    let status = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers: Vec<_> = head
        .lines()
        .skip(1)
        .map(|line| line.split_once(':').unwrap())
        .collect();
    let content_length: usize = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .unwrap()
        .1
        .trim()
        .parse()
        .unwrap();
    assert!(headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("connection")
            && value.trim().eq_ignore_ascii_case("close")));
    let body = &out[split + 4..];
    assert_eq!(body.len(), content_length, "response must not be truncated");
    (status, serde_json::from_slice(body).unwrap())
}

#[test]
fn chunked_put_with_empty_trailers_stages() {
    let h = Harness::start();
    let path = format!("/jobs/job_emptytrail01/t/{HEX_A}/in/{}", input_key());
    let (status, bytes) = h.raw(
        &format!("PUT {path} HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"),
        b"5\r\nhello\r\n0\r\n\r\n",
    );
    assert_eq!(status, 201);
    let reply: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(reply["key"], input_key());
    assert_eq!(reply["size"], 5);
    assert_eq!(h.invocation_count(), 0);
}

#[test]
fn chunked_put_consumes_multiple_trailers_before_reply() {
    let h = Harness::start();
    let path = format!("/jobs/job_multitrail01/t/{HEX_A}/in/{}", input_key());
    let mut stream = h.connect();
    write!(stream, "PUT {path} HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5;ext=1\r\nhello\r\n0\r\nX-First: one\r\n").unwrap();
    // Until the blank line, the trailer section is not complete. A server
    // that treats the first trailer as the end would publish/reply early.
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut byte = [0];
    let err = stream
        .read(&mut byte)
        .expect_err("must wait for the full trailer section");
    assert!(
        matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "{err}"
    );
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(b"X-Second: two\r\nX-Third: three\r\n\r\n")
        .unwrap();
    let (status, reply) = closed_response(&mut stream);
    assert_eq!(status, 201);
    assert_eq!(reply["size"], 5);
    assert_eq!(h.invocation_count(), 0);
}

#[test]
fn chunked_put_rejects_eof_before_trailer_terminator() {
    let h = Harness::start();
    for suffix in [b"".as_slice(), b"X-One: yes\r\n", b"X-One: truncated"] {
        let path = format!("/jobs/job_eoftrail01/t/{HEX_A}/in/{}", input_key());
        let mut stream = h.connect();
        write!(stream, "PUT {path} HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n").unwrap();
        stream.write_all(suffix).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let (status, reply) = closed_response(&mut stream);
        assert_eq!(
            status, 400,
            "EOF must not stage a successful input: {reply}"
        );
        assert!(reply["error"].is_string(), "{reply}");
    }
    assert_eq!(h.invocation_count(), 0);
}

#[test]
fn chunked_put_rejects_oversize_trailer_section() {
    let h = Harness::start();
    let path = format!("/jobs/job_bigtrail01/t/{HEX_A}/in/{}", input_key());
    // Many individually small fields exceed a bounded trailer budget.
    // Stay below the 1 MiB teardown allowance so resets cannot be excused
    // by deliberately exceeding the discard contract.
    let mut body = b"5\r\nhello\r\n0\r\n".to_vec();
    for _ in 0..8192 {
        body.extend_from_slice(b"X-Trailer: aaaaaaaaaaaaaaaaaaaaaaaa\r\n");
    }
    body.extend_from_slice(b"\r\n");
    let mut stream = h.connect();
    write!(
        stream,
        "PUT {path} HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    let (status, reply) = closed_response(&mut stream);
    // The protocol requires rejection, not a particular trailer-budget
    // error spelling; both malformed-framing and payload-size errors fit.
    assert!(
        matches!(status, 400 | 413),
        "oversize trailers accepted: {status} {reply}"
    );
    assert!(reply["error"].is_string(), "{reply}");
    assert_eq!(h.invocation_count(), 0);
}

#[test]
fn early_size_refusals_close_without_waiting_for_advertised_body() {
    let h = Harness::start();
    let put = format!("/jobs/job_unread01/t/{HEX_A}/in/{}", input_key());
    for (method, path, length) in [
        ("POST", "/transcode", 2097152),
        ("PUT", put.as_str(), 68157440),
    ] {
        let mut stream = h.connect();
        let start = Instant::now();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {length}\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n").unwrap();
        // No body and no client half-close: waiting for either would deadlock.
        let (status, reply) = closed_response(&mut stream);
        assert_eq!(status, 413);
        assert_eq!(reply["error"], "payload_too_large");
        assert!(start.elapsed() < Duration::from_secs(2));
        // Keep the refused client's write side open while another mutation
        // completes: teardown must not retain the instance's mutation gate.
        let mut next = h.connect();
        write!(next, "PUT /jobs/job_afterrefusal{length}/t/{HEX_A}/in/{} HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello", input_key()).unwrap();
        let (status, reply) = closed_response(&mut next);
        assert_eq!(status, 201, "unread body held mutation gate: {reply}");
        assert_eq!(reply["size"], 5);
    }
    assert_eq!(h.invocation_count(), 0);
}

#[test]
fn chunked_put_with_extension_stages() {
    let h = Harness::start();
    let key = input_key();
    let path = format!("/jobs/job_chunk01/t/{HEX_A}/in/{key}");
    // Two chunks plus a chunk extension and a trailer.
    let body = b"5;ext=1\r\nhello\r\n7\r\n chunk!\r\n0\r\nX-Trailer: yes\r\n\r\n";
    let head = format!(
        "PUT {path} HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    );
    let (status, bytes) = h.raw(&head, body);
    assert_eq!(
        status,
        201,
        "chunked PUT: {}",
        String::from_utf8_lossy(&bytes)
    );
    let reply: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(reply["key"], key);
    assert_eq!(reply["size"], 12u64);
}

#[test]
fn post_without_framing_is_length_required() {
    let h = Harness::start();
    let (status, bytes) = h.raw(
        "POST /transcode HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        b"",
    );
    assert_eq!(status, 411);
    let reply: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(reply["error"], "length_required");
}

#[test]
fn oversize_json_declaration_rejects_before_reading() {
    let h = Harness::start();
    // Declared 2 MiB over a tiny actual body: the pre-check rejects
    // from the declaration alone.
    let (status, bytes) = h.raw(
        "POST /transcode HTTP/1.1\r\nHost: x\r\nContent-Length: 2097152\r\nConnection: close\r\n\r\n",
        b"{}",
    );
    assert_eq!(status, 413);
    let reply: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(reply["error"], "payload_too_large");
}

#[test]
fn oversize_json_stream_rejects_past_the_cap() {
    let h = Harness::start();
    // 1 MiB + 1 byte streamed in chunks: the running total trips the
    // cap mid-stream, never after buffering the whole body.
    let chunk = vec![b'x'; 64 * 1024];
    let mut body = Vec::new();
    for _ in 0..17 {
        body.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        body.extend_from_slice(&chunk);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"0\r\n\r\n");
    let (status, bytes) = h.raw(
        "POST /transcode HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        &body,
    );
    assert_eq!(status, 413);
    let reply: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(reply["error"], "payload_too_large");
}

#[test]
fn oversize_put_declaration_rejects() {
    let h = Harness::start();
    let key = input_key();
    let path = format!("/jobs/job_bigput01/t/{HEX_A}/in/{key}");
    // 65 MiB declared: past the 64 MiB input ceiling.
    let (status, bytes) = h.raw(
        &format!("PUT {path} HTTP/1.1\r\nHost: x\r\nContent-Length: 68157440\r\nConnection: close\r\n\r\n"),
        b"",
    );
    assert_eq!(status, 413);
    let reply: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(reply["error"], "payload_too_large");
}

#[test]
fn unknown_routes_and_methods_are_not_found() {
    let h = Harness::start();
    let (status, _) = h.get_bytes("/jobs/job_01/t/staging_09/mid/abc");
    assert_eq!(status, 404);
    let (status, _) = h.raw(
        "DELETE /jobs/job_01/t/staging_09/in/abc HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        b"",
    );
    assert_eq!(status, 404);
    // Repair-shaped round paths do not exist here.
    let (status, _) = h.get_bytes("/jobs/job_01/r1/out/abc");
    assert_eq!(status, 404);
}
