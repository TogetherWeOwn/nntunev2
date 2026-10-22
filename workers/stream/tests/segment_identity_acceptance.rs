//! Acceptance: probe/body, native BODY and play agree with ingest on bare
//! printable message-ids with exactly one non-edge '@' and a 250-byte cap.
//! Synthetic loopback only. Each vector is a separate test, including the
//! 250/251/512/513 boundaries, so one RED cannot hide another vector.
//!
//! Sensitivity: a raised cap, trim/unbracket bypass, missing pre-lease list
//! validation, or mapping bad metadata to retryable upstream errors fails.
//! Successful yEnc play and a genuine provider failure are independent controls.
//! Old tests that intentionally pin 512 B / normalization / empty->Upstream
//! need reconciliation by the paired Builder, not edits to this acceptance.

#[macro_use]
#[path = "support/segment_identity_vectors.rs"]
mod identity_vectors;

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use nntune_stream::body::parse_body_request;
use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::{MemoryMeter, MeterKind};
use nntune_stream::nntp::{NntpClient, NntpError, UpstreamTimeouts};
use nntune_stream::play::{play_with_timeouts, PlayError, PlayInput, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::probe::{parse_request, valid_probe_id};
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"synthetic-identity-acceptance-secret";
const NOW: u64 = 1_700_000_000;
const TRACK: &str = "identity-track";
const PLAIN: &[u8] = b"synthetic segment identity payload";

fn timeouts() -> UpstreamTimeouts {
    UpstreamTimeouts::new(Duration::from_secs(2), Duration::from_secs(2))
}

#[derive(Clone, Debug, Default)]
struct Observed {
    connections: usize,
    command: Vec<u8>,
}

/// One real NNTP connection, one BODY response. The stop connection wakes a
/// blocking accept on local refusals; it is never counted as a provider dial.
/// Reads/writes are bounded, and Drop joins even when an assertion unwinds.
struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    observed: Arc<Mutex<Observed>>,
    handle: Option<JoinHandle<()>>,
}

impl Server {
    fn start(provider_failure: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("synthetic NNTP listener");
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(Mutex::new(Observed::default()));
        let (stopping, record) = (stop.clone(), observed.clone());
        let handle = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("synthetic accept");
            if stopping.load(Ordering::SeqCst) {
                return;
            }
            record.lock().unwrap().connections += 1;
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket.write_all(b"200 synthetic NNTP ready\r\n").unwrap();
            let mut reader = BufReader::new(socket);
            let mut command = Vec::new();
            reader.read_until(b'\n', &mut command).expect("BODY or EOF");
            record.lock().unwrap().command = command.clone();
            if command.is_empty() {
                return;
            }
            let mut socket = reader.into_inner();
            if provider_failure {
                socket
                    .write_all(b"400 synthetic upstream failure\r\n")
                    .unwrap();
                return;
            }
            let article = yenc::encode_part(
                PLAIN,
                "identity.flac",
                PartSpec {
                    file_size: PLAIN.len() as u64,
                    part: 1,
                    total: 1,
                    offset: 0,
                    file_crc: Some(yenc::crc32(PLAIN)),
                },
            );
            let mut reply = b"222 article follows\r\n".to_vec();
            for line in article.split(|&b| b == b'\n') {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line.first() == Some(&b'.') {
                    reply.push(b'.');
                }
                reply.extend_from_slice(line);
                reply.extend_from_slice(b"\r\n");
            }
            reply.extend_from_slice(b".\r\n");
            // A regressed rejection may close while the reply is sent. The
            // observable command is still recorded; do not mask it with EPIPE.
            let _ = socket.write_all(&reply);
        });
        Self {
            addr,
            stop,
            observed,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> Observed {
        self.shutdown();
        let observed = self.observed.lock().unwrap().clone();
        observed
    }

    fn shutdown(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(2));
            handle.join().expect("synthetic NNTP thread");
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn input<'a>(token: &'a str, segments: &'a [String], addr: SocketAddr) -> PlayInput<'a> {
    PlayInput {
        trace_id: None,
        token,
        track_id: TRACK,
        range: None,
        server_addr: addr,
        segments,
        segment_ranges: None,
        cipher_key: 0,
        now_unix: NOW,
        up_next: None,
    }
}

fn assert_final_bad_request(error: &PlayError) {
    assert_eq!(
        error.http_status(),
        400,
        "local metadata is final: {error:?}"
    );
    let envelope = error.api_error();
    assert_eq!(envelope.status, 400);
    assert!(
        !envelope.retryable,
        "bad metadata must not retry: {error:?}"
    );
    assert_eq!(envelope.code, "invalid_request");
    assert!(error.retry_after_header().is_none());
}

fn assert_no_provider_effects(pool: &StreamPool, meter: &MemoryMeter, cache: &ContentCache) {
    assert_eq!(pool.in_use(), 0, "no lease held");
    assert_eq!(pool.connects(), 0, "no dial");
    assert_eq!(pool.reuses(), 0, "no warm lease reuse");
    assert!(!pool.quarantined());
    assert!(!meter.has_kind(MeterKind::ProviderError));
    assert!(!meter.has_kind(MeterKind::QuarantineEntered));
    assert!(cache.get_track(TRACK).is_none());
}

mod probe_route {
    use super::*;
    macro_rules! case {
        ($name:ident, $id:expr, $want:expr) => {
            #[test]
            fn $name() {
                let id = $id;
                assert_eq!(valid_probe_id(&id), $want, "probe validator: {id:?}");
                let body = serde_json::to_vec(&serde_json::json!({
                    "job_id": "job_01", "user_id": "user_01",
                    "idempotency_key": "key_01", "batch_index": 0, "segment_ids": [&id]
                })).unwrap();
                let result = parse_request(&body, "user_01");
                assert_eq!(result.is_ok(), $want, "probe request: {id:?}: {result:?}");
                match result {
                    Ok(request) => assert_eq!(request.segment_ids, [id]),
                    Err(error) => assert_eq!(error.status(), 400),
                }
            }
        };
    }
    identity_cases!(case);
}

mod body_route {
    use super::*;
    macro_rules! case {
        ($name:ident, $id:expr, $want:expr) => {
            #[test]
            fn $name() {
                let id = $id;
                let body = serde_json::to_vec(&serde_json::json!({
                    "job_id": "job_01", "user_id": "user_01",
                    "idempotency_key": "key_01", "segment_id": &id
                })).unwrap();
                let result = parse_body_request(&body, "user_01");
                assert_eq!(result.is_ok(), $want, "body request: {id:?}: {result:?}");
                match result {
                    Ok(request) => assert_eq!(request.segment_id, id),
                    Err(error) => assert_eq!(error.status(), 400),
                }
            }
        };
    }
    identity_cases!(case);
}

mod native_wire {
    use super::*;
    macro_rules! case {
        ($name:ident, $id:expr, $want:expr) => {
            #[test]
            fn $name() {
                let id = $id;
                let server = Server::start(false);
                let mut client = NntpClient::connect_with_timeouts(&server.addr, timeouts())
                    .expect("synthetic greeting");
                let result = client.article(&id);
                drop(client);
                let observed = server.finish();
                assert_eq!(result.is_ok(), $want, "native BODY: {id:?}: {result:?}");
                if $want {
                    assert_eq!(observed.command, format!("BODY <{id}>\r\n").as_bytes());
                    assert!(id.len() <= 250, "wire argument ceiling");
                } else {
                    let error = result.unwrap_err();
                    assert!(matches!(error, NntpError::InvalidMessageId(_)), "{error:?}");
                    assert!(!error.is_provider_failure());
                    assert!(
                        observed.command.is_empty(),
                        "rejected ID reached wire: {observed:?}"
                    );
                }
            }
        };
    }
    identity_cases!(case);
}

mod play_path {
    use super::*;
    macro_rules! case {
        ($name:ident, $id:expr, $want:expr) => {
            #[test]
            fn $name() {
                let id = $id;
                let server = Server::start(false);
                let mut pool: StreamPool = Pool::new(4);
                let mut cache = ContentCache::new();
                let mut meter = MemoryMeter::default();
                let token = grant::issue("user_01", TRACK, NOW + 900, SECRET);
                let segments = [id.clone()];
                let result = play_with_timeouts(
                    &mut pool,
                    &mut cache,
                    &mut meter,
                    SECRET,
                    &input(&token, &segments, server.addr),
                    timeouts(),
                );
                if $want {
                    let response = result.unwrap_or_else(|e| panic!("valid play {id:?}: {e:?}"));
                    assert_eq!(response.status, 200);
                    let mut stream = response.stream;
                    let body: Vec<u8> = stream.by_ref().flatten().collect();
                    assert_eq!(body, PLAIN);
                    assert!(stream.error().is_none());
                    stream.finalize(&mut pool, &mut cache, &mut meter).unwrap();
                    assert_eq!(cache.get_track(TRACK).unwrap().as_ref(), PLAIN);
                    let observed = server.finish();
                    assert_eq!(observed.connections, 1);
                    assert_eq!(observed.command, format!("BODY <{id}>\r\n").as_bytes());
                    assert!(id.len() <= 250, "wire argument ceiling");
                } else {
                    let error = match result {
                        Err(error) => error,
                        Ok(response) => {
                            response.stream.abort(&mut pool);
                            panic!("play accepted refused spelling {id:?}");
                        }
                    };
                    let observed = server.finish();
                    assert_final_bad_request(&error);
                    assert_no_provider_effects(&pool, &meter, &cache);
                    assert_eq!(observed.connections, 0, "pre-dial refusal: {id:?}");
                    assert!(observed.command.is_empty());
                }
            }
        };
    }
    identity_cases!(case);
}

#[test]
fn invalid_later_segment_is_checked_before_any_lease_or_dial() {
    let server = Server::start(false);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let token = grant::issue("user_01", TRACK, NOW + 900, SECRET);
    let segments = ["valid@synthetic.invalid".into(), identity_vectors::id(251)];
    let result = play_with_timeouts(
        &mut pool,
        &mut cache,
        &mut meter,
        SECRET,
        &input(&token, &segments, server.addr),
        timeouts(),
    );
    let error = match result {
        Err(error) => error,
        Ok(response) => {
            response.stream.abort(&mut pool);
            panic!("invalid later segment must fail before fetching the first");
        }
    };
    let observed = server.finish();
    assert_final_bad_request(&error);
    assert_no_provider_effects(&pool, &meter, &cache);
    assert_eq!(observed.connections, 0);
}

#[test]
fn empty_segments_are_final_400_without_provider_effects() {
    let server = Server::start(false);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let token = grant::issue("user_01", TRACK, NOW + 900, SECRET);
    for _ in 0..4 {
        let result = play_with_timeouts(
            &mut pool,
            &mut cache,
            &mut meter,
            SECRET,
            &input(&token, &[], server.addr),
            timeouts(),
        );
        let error = match result {
            Err(error) => error,
            Ok(response) => {
                response.stream.abort(&mut pool);
                panic!("empty metadata cannot play");
            }
        };
        assert_final_bad_request(&error);
        assert_no_provider_effects(&pool, &meter, &cache);
    }
    assert_eq!(server.finish().connections, 0);
}

#[test]
fn empty_segments_are_refused_before_pool_admission() {
    let server = Server::start(false);
    let mut pool: StreamPool = Pool::new(0);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let token = grant::issue("user_01", TRACK, NOW + 900, SECRET);
    let result = play_with_timeouts(
        &mut pool,
        &mut cache,
        &mut meter,
        SECRET,
        &input(&token, &[], server.addr),
        timeouts(),
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("empty metadata cannot play with no pool capacity"),
    };
    assert_final_bad_request(&error);
    assert_no_provider_effects(&pool, &meter, &cache);
    assert_eq!(server.finish().connections, 0);
}

#[test]
fn genuine_provider_failure_remains_retryable_502() {
    let server = Server::start(true);
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let token = grant::issue("user_01", TRACK, NOW + 900, SECRET);
    let segments = ["valid@synthetic.invalid".to_owned()];
    let error = match play_with_timeouts(
        &mut pool,
        &mut cache,
        &mut meter,
        SECRET,
        &input(&token, &segments, server.addr),
        timeouts(),
    ) {
        Err(error) => error,
        Ok(_) => panic!("scripted provider failure cannot play"),
    };
    let observed = server.finish();
    assert_eq!(observed.connections, 1, "control reaches provider");
    assert_eq!(observed.command, b"BODY <valid@synthetic.invalid>\r\n");
    assert_eq!(error.http_status(), 502, "{error:?}");
    assert_eq!(error.api_error().status, 502);
    assert!(error.api_error().retryable, "provider failure is transient");
    assert_eq!(pool.in_use(), 0);
    assert!(cache.get_track(TRACK).is_none());
}
