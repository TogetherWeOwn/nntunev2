//! Chunked-framing and body-size refusal coverage for the repair Container.
//!
//! Pins existing wire branches in `containers/repair/src/lib.rs` without
//! touching implementation: multi-chunk `Transfer-Encoding: chunked` PUTs
//! (including a short chunk extension and the normal empty trailer), the
//! 411 `length_required` refusal when framing is absent, and the 413
//! `payload_too_large` refusal for an oversized declared JSON body.
//!
//! Socket reads/writes are bounded so a decoding regression fails
//! deterministically instead of hanging the suite. Fake `par2` only,
//! loopback ephemeral port, run-owned scratch; serialised in this binary
//! like `repair_server.rs` to avoid ETXTBSY flakes from forked `prlimit`.
#![cfg(target_os = "linux")]

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use nntune_core::par2::scratch::object_key_for_file_name;
use nntune_repair_server::{serve, ServerConfig};

static NEXT: AtomicUsize = AtomicUsize::new(0);
// Serialize tests in this binary: same ETXTBSY guard as repair_server.rs.
static SERIAL: Mutex<()> = Mutex::new(());

const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// One byte over the server's `MAX_JSON_BYTES` (`1 << 20`).
const OVERSIZED_JSON_LEN: usize = (1 << 20) + 1;

struct Fixture {
    root: PathBuf,
    addr: std::net::SocketAddr,
    count_file: PathBuf,
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new(script_body: &str) -> Self {
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir();
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = base.join(format!(
            "nntune-repair-chunked-test-{}-{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("scratch")).unwrap();
        let count_file = root.join("invocations");
        let binary = root.join("fake-par2.sh");
        let script = format!(
            "#!/bin/sh\nset -eu\necho x >> {}\n{script_body}\n",
            count_file.display()
        );
        fs::write(&binary, script).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let config = Arc::new(ServerConfig {
            scratch_root: root.join("scratch"),
            par2_bin: binary,
        });
        std::thread::spawn(move || {
            let _ = serve(listener, config);
        });
        Self {
            root,
            addr,
            count_file,
            _serial: serial,
        }
    }

    fn invocations(&self) -> usize {
        fs::read_to_string(&self.count_file)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn staged_path(&self, job: &str, round: u32, key: &str) -> PathBuf {
        self.root
            .join("scratch")
            .join(job)
            .join(format!("r{round}"))
            .join("in")
            .join(key)
    }

    fn request(&self, raw: &[u8]) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(self.addr).unwrap();
        stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
        stream.write_all(raw).unwrap();
        let mut response = Vec::new();
        // A refusal (411/413) is sent without consuming already-arrived
        // request body bytes, so the kernel answers the close with RST
        // instead of FIN. Treat bytes that arrived before the reset as
        // the response; a regression that sends nothing still fails via
        // the status/body asserts below (or a read timeout, not a hang).
        match stream.read_to_end(&mut response) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset && !response.is_empty() => {}
            Err(e) => panic!("response read failed: {e:?}"),
        }
        Self::split_response(&response)
    }

    fn split_response(response: &[u8]) -> (u16, Vec<u8>) {
        let text = String::from_utf8_lossy(response);
        let status: u16 = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let body_start = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| p + 4)
            .unwrap_or(response.len());
        (status, response[body_start..].to_vec())
    }

    fn put_in(&self, job: &str, round: u32, file_name: &str, bytes: &[u8]) -> u16 {
        let key = object_key_for_file_name(file_name);
        let head = format!(
            "PUT /jobs/{job}/r{round}/in/{key} HTTP/1.1\r\nHost: container\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        );
        let mut raw = head.into_bytes();
        raw.extend_from_slice(bytes);
        self.request(&raw).0
    }

    fn get_out(&self, job: &str, round: u32, file_name: &str) -> (u16, Vec<u8>) {
        let key = object_key_for_file_name(file_name);
        let raw = format!(
            "GET /jobs/{job}/r{round}/out/{key} HTTP/1.1\r\nHost: container\r\nConnection: close\r\n\r\n"
        );
        self.request(raw.as_bytes())
    }

    fn post_repair(&self, body: &serde_json::Value) -> (u16, serde_json::Value) {
        let body = serde_json::to_vec(body).unwrap();
        let head = format!(
            "POST /repair HTTP/1.1\r\nHost: container\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let mut raw = head.into_bytes();
        raw.extend_from_slice(&body);
        let (status, bytes) = self.request(&raw);
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const JOB: &str = "job_0123456789abcdef";

#[test]
fn chunked_put_with_extension_stages_exact_bytes_and_repairs() {
    // The fake binary "repairs" by rewriting the data file in the work dir.
    let f = Fixture::new("printf 'GOODBYTES' > track.flac\nprintf 'Repair complete.\\n'");

    // Multi-chunk PUT: "DAMAGED" split 3 + 4, short extension on the first
    // chunk, normal empty trailer.
    let key = object_key_for_file_name("track.flac");
    let head = format!(
        "PUT /jobs/{JOB}/r1/in/{key} HTTP/1.1\r\nHost: container\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    );
    let mut raw = head.into_bytes();
    raw.extend_from_slice(b"3;ext=x\r\nDAM\r\n4\r\nAGED\r\n0\r\n\r\n");
    let (status, body) = f.request(&raw);
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["key"], key);
    assert_eq!(value["size"], 7);

    // Exactly the decoded staged bytes land under `in/`.
    let staged = fs::read(f.staged_path(JOB, 1, &key)).unwrap();
    assert_eq!(staged, b"DAMAGED");

    // The chunked-staged bytes flow through an ordinary successful repair.
    assert_eq!(f.put_in(JOB, 1, "set.par2", b"PAR2DATA"), 201);
    let req = serde_json::json!({
        "job_id": JOB,
        "round": 1,
        "staging_id": "staging-0000000000000001",
        "files": [
            {"file_name": "track.flac", "key": object_key_for_file_name("track.flac"), "size": 7, "role": "data"},
            {"file_name": "set.par2", "key": object_key_for_file_name("set.par2"), "size": 8, "role": "par2"},
        ]
    });
    let (status, result) = f.post_repair(&req);
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["status"], "repaired");
    assert_eq!(result["files"][0]["file_name"], "track.flac");
    assert_eq!(result["files"][0]["size"], 9);
    let (status, bytes) = f.get_out(JOB, 1, "track.flac");
    assert_eq!(status, 200);
    assert_eq!(bytes, b"GOODBYTES");
}

#[test]
fn missing_framing_is_411_without_par2() {
    let f = Fixture::new("exit 0");

    // POST /repair with neither Content-Length nor Transfer-Encoding.
    // Head only: any body bytes sent here would stay unread when the
    // server refuses, turning the close into an RST that can race the
    // error body. The 411 branch fires on the headers alone.
    let raw = b"POST /repair HTTP/1.1\r\nHost: container\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n";
    let (status, body) = f.request(raw);
    assert_eq!(status, 411, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
        "length_required"
    );

    // PUT without framing hits the same existing envelope.
    let key = object_key_for_file_name("track.flac");
    let raw = format!(
        "PUT /jobs/{JOB}/r1/in/{key} HTTP/1.1\r\nHost: container\r\nConnection: close\r\n\r\n"
    );
    let (status, body) = f.request(raw.as_bytes());
    assert_eq!(status, 411, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
        "length_required"
    );

    assert_eq!(f.invocations(), 0);
}

#[test]
fn oversized_declared_json_is_413_without_par2() {
    let f = Fixture::new("exit 0");

    // Declared length one byte over the server's JSON cap. The server
    // refuses on the declaration alone without waiting for the body, so
    // no body bytes are sent: they would stay unread and turn the close
    // into an RST that can race the error body. A hang here would hit
    // the read timeout instead of passing.
    let raw = format!(
        "POST /repair HTTP/1.1\r\nHost: container\r\nContent-Type: application/json\r\nContent-Length: {OVERSIZED_JSON_LEN}\r\nConnection: close\r\n\r\n"
    );
    let (status, body) = f.request(raw.as_bytes());
    assert_eq!(status, 413, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
        "payload_too_large"
    );
    assert_eq!(f.invocations(), 0);
}
