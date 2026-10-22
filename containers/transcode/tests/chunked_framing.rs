//! HTTP framing pins: chunked request bodies (with extensions),
//! missing framing (411), and oversize JSON declarations and streams
//! (413). The encoder never runs here: PUT staging and JSON
//! pre-checks reject before any encode could start.

mod common;

use common::*;

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
