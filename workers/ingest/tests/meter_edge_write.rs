//! S12 acceptance: ingest meter edge-write never silently drops.
//!
//! Slice contract (roadmap S12, build card TOG-25587): stage-transition
//! events are written through the `INGEST_METER` dataset binding at enqueue;
//! a missing or failing binding surfaces a retryable failure (HTTP 503 +
//! `Retry-After`, never 202-and-dropped); the event shape stays owned by
//! `metering.rs`; the stage allowlist (`WORKFLOW_STEPS`) is unchanged.
//!
//! Builder contract (TOG-25587): implement these four items in
//! `src/metering.rs` WITHOUT editing this file:
//!
//! 1. `pub fn meter_write_body(event: &MeterEvent) -> serde_json::Value` —
//!    the exact wire body for one event. It must round-trip back to the same
//!    `MeterEvent` and carry exactly the shape `meter_event` built.
//! 2. `pub enum MeterWriteError { UnknownStage(&'static str), Transport(String),
//!    Rejected { status: u16 } }` with `pub fn is_retryable(&self) -> bool`
//!    (`Transport`/`Rejected` retry, `UnknownStage` does not) and
//!    `pub fn http_status(&self) -> u16` (retryable maps to 503,
//!    `UnknownStage` maps to 400).
//! 3. `pub trait MeterSink { fn send(&mut self, dataset: &str, body:
//!    serde_json::Value) -> Result<u16, String>; }` — deliver one body to the
//!    named dataset; the `u16` is the HTTP-equivalent status of the dataset
//!    write and `Err` means the binding/transport failed before any status
//!    existed. (At the edge this is the `INGEST_METER` binding call; a native
//!    binding success maps to 2xx.)
//! 4. `pub fn emit_meter<S: MeterSink>(sink: &mut S, msg: &QueueMsg, stage:
//!    &str, outcome: MeterOutcome) -> Result<(), MeterWriteError>` — build the
//!    event with `meter_event` (unknown stage returns `UnknownStage` BEFORE
//!    touching the sink), deliver exactly one body shaped like the
//!    `MeterEvent`, and classify the result: 2xx is `Ok`, anything else
//!    (transport `Err` or non-2xx) is a retryable `Err`. Never `Ok`
//!    on failure: a dropped event must never look accepted.
//!
//! These tests speak plain HTTP to a fake dataset-binding endpoint
//! (std-only `TcpListener`, no new dependencies): the fake asserts the exact
//! `MeterEvent` bytes crossed the wire, counts attempts, and scripts success
//! (200), failure (500), or absence (closed port) per case.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

use nntune_ingest::dispatch::build_upload_dispatch;
use nntune_ingest::fixtures::SMALL_NZB;
use nntune_ingest::metering::{
    emit_meter, meter_event, meter_write_body, MeterOutcome, MeterSink, MeterWriteError,
    METER_DATASET,
};
use nntune_ingest::queue::QueueMsg;

struct Received {
    method: String,
    path: String,
    body: String,
}

/// Fake dataset-binding HTTP endpoint: records every POST, answers every
/// request with the scripted status. std-only so the contract is provable in
/// CI with no staging dependency.
struct FakeDatasetEndpoint {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    hits: Arc<AtomicUsize>,
    received: Arc<Mutex<Vec<Received>>>,
    status: u16,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeDatasetEndpoint {
    fn start(status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake dataset endpoint");
        let addr = listener.local_addr().expect("fake endpoint addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let hits = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(Mutex::new(Vec::new()));
        let (stop_c, hits_c, received_c) =
            (Arc::clone(&stop), Arc::clone(&hits), Arc::clone(&received));
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
                Self::serve(stream, status, &hits_c, &received_c);
            }
        });
        Self {
            addr,
            stop,
            hits,
            received,
            status,
            handle: Some(handle),
        }
    }

    fn serve(stream: TcpStream, status: u16, hits: &AtomicUsize, received: &Mutex<Vec<Received>>) {
        let mut stream = stream;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone dataset conn"));
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() {
            return;
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            for prefix in ["content-length:", "Content-Length:"] {
                if let Some(value) = trimmed.strip_prefix(prefix) {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let capped = content_length.min(64 * 1024);
        let mut body = vec![0u8; capped];
        if capped > 0 && reader.read_exact(&mut body).is_err() {
            return;
        }
        received.lock().expect("received lock").push(Received {
            method,
            path,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
        hits.fetch_add(1, Ordering::SeqCst);
        let reason = if (200..300).contains(&status) {
            "OK"
        } else {
            "Error"
        };
        let _ = write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Drop for FakeDatasetEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Minimal HTTP client: the test-side half of the `MeterSink` contract, so the
/// wire shape is asserted over a real socket, not an in-memory stub.
fn post_json(addr: SocketAddr, path: &str, body: &str) -> Result<u16, String> {
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| e.to_string())?;
    String::from_utf8_lossy(&response)
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "dataset endpoint returned no parsable status".to_string())
}

struct HttpDatasetSink {
    addr: SocketAddr,
    last_dataset: Option<String>,
}

impl HttpDatasetSink {
    fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            last_dataset: None,
        }
    }
}

impl MeterSink for HttpDatasetSink {
    fn send(&mut self, dataset: &str, body: serde_json::Value) -> Result<u16, String> {
        self.last_dataset = Some(dataset.to_string());
        post_json(self.addr, &format!("/{dataset}/write"), &body.to_string())
    }
}

fn upload_msg() -> QueueMsg {
    build_upload_dispatch("user_01", SMALL_NZB).expect("fixture dispatch")
}

/// A port nothing listens on: the binding-missing equivalent (connection
/// refused before any byte is written).
fn closed_port() -> SocketAddr {
    let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
    let addr = probe.local_addr().expect("probe addr");
    drop(probe);
    addr
}

#[test]
fn meter_success_delivers_exact_event_shape() {
    let server = FakeDatasetEndpoint::start(200);
    let mut sink = HttpDatasetSink::new(server.addr);
    let msg = upload_msg();

    emit_meter(&mut sink, &msg, "probe", MeterOutcome::Ok).expect("200 must be Ok");

    // The write targeted this dataset, exactly once.
    assert_eq!(sink.last_dataset.as_deref(), Some(METER_DATASET));
    assert_eq!(server.hits(), 1, "one stage transition writes exactly once");
    let received = server.received.lock().expect("received lock");
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].method, "POST");
    assert_eq!(received[0].path, format!("/{METER_DATASET}/write"));

    // The bytes on the wire are exactly the MeterEvent metering.rs owns.
    let expected = serde_json::to_value(meter_event(&msg, "probe", MeterOutcome::Ok).unwrap())
        .expect("event serializes");
    let actual: serde_json::Value =
        serde_json::from_str(&received[0].body).expect("wire body is JSON");
    assert_eq!(
        actual, expected,
        "wire body must be the exact MeterEvent shape"
    );

    // The body builder agrees: it round-trips to the same event.
    let event = meter_event(&msg, "probe", MeterOutcome::Ok).unwrap();
    let rebuilt: nntune_ingest::metering::MeterEvent =
        serde_json::from_value(meter_write_body(&event)).expect("body round-trips");
    assert_eq!(rebuilt, event);
}

#[test]
fn meter_failing_write_surfaces_retryable_failure() {
    let server = FakeDatasetEndpoint::start(500);
    let mut sink = HttpDatasetSink::new(server.addr);
    let msg = upload_msg();

    let err = emit_meter(&mut sink, &msg, "probe", MeterOutcome::Ok)
        .expect_err("a 500 dataset write must never report success");

    // The write WAS attempted with the exact shape — then surfaced, not
    // dropped: 202-and-dropped would have returned Ok here.
    assert_eq!(
        server.hits(),
        1,
        "the failed write was attempted, not skipped"
    );
    let received = server.received.lock().expect("received lock");
    let actual: serde_json::Value =
        serde_json::from_str(&received[0].body).expect("wire body is JSON");
    let expected = serde_json::to_value(meter_event(&msg, "probe", MeterOutcome::Ok).unwrap())
        .expect("event serializes");
    assert_eq!(actual, expected);
    assert!(
        err.is_retryable(),
        "a failing write is retryable, never a silent drop: {err:?}"
    );
    assert_eq!(
        err.http_status(),
        503,
        "a failing write maps to 503 + Retry-After, never 202"
    );
}

#[test]
fn meter_missing_binding_surfaces_retryable_failure() {
    // No server: the INGEST_METER binding is missing, so the write cannot
    // even be attempted — this must still fail loudly and retryably.
    let mut sink = HttpDatasetSink::new(closed_port());
    let msg = upload_msg();

    let err = emit_meter(&mut sink, &msg, "probe", MeterOutcome::Retry)
        .expect_err("a missing binding must never report success");

    assert!(
        err.is_retryable(),
        "a missing binding is retryable, never a silent drop: {err:?}"
    );
    assert_eq!(
        err.http_status(),
        503,
        "a missing binding maps to 503 + Retry-After, never 202"
    );
    assert!(
        !matches!(err, MeterWriteError::UnknownStage(_)),
        "a transport failure is not a stage error: {err:?}"
    );
}

#[test]
fn meter_unknown_stage_rejected_before_any_write() {
    let server = FakeDatasetEndpoint::start(200);
    let mut sink = HttpDatasetSink::new(server.addr);
    let msg = upload_msg();

    let err = emit_meter(&mut sink, &msg, "teleport", MeterOutcome::Ok)
        .expect_err("an unknown stage must be rejected");

    assert!(
        matches!(err, MeterWriteError::UnknownStage(_)),
        "unknown stage keeps its stage error, got: {err:?}"
    );
    assert!(
        !err.is_retryable(),
        "an unknown stage is a caller bug, not a retry: {err:?}"
    );
    assert_eq!(err.http_status(), 400, "an unknown stage maps to 400");
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        server.hits(),
        0,
        "rejection happens before any write attempt"
    );
    assert!(
        sink.last_dataset.is_none(),
        "nothing was delivered anywhere"
    );
}
