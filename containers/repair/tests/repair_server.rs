//! Native HTTP tests for the repair Container server, using fake `par2`
//! executables in owned directories (same pattern as the nntune-core
//! repair fixtures). The fake binary receives the real
//! `prlimit -- par2 repair -B<work> ...` invocation from
//! `nntune_core::par2::repair::repair_with_limits`.
#![cfg(target_os = "linux")]

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use nntune_core::par2::scratch::object_key_for_file_name;
use nntune_repair_server::{serve, ServerConfig};

static NEXT: AtomicUsize = AtomicUsize::new(0);
// Serialize tests in this binary: repair forks via prlimit, and a fork in
// one libtest thread can inherit another thread's still-open write fd to
// its fake-binary script (ETXTBSY flakes) — same guard as nntune-core.
static SERIAL: Mutex<()> = Mutex::new(());

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
            "nntune-repair-server-test-{}-{seq}",
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

    fn request(&self, raw: &[u8]) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(self.addr).unwrap();
        stream.write_all(raw).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let text = String::from_utf8_lossy(&response);
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

    /// Stage one damaged data file + one par2 file and return the request.
    fn staged_request(&self, job: &str, round: u32) -> serde_json::Value {
        assert_eq!(self.put_in(job, round, "track.flac", b"DAMAGED"), 201);
        assert_eq!(self.put_in(job, round, "set.par2", b"PAR2DATA"), 201);
        serde_json::json!({
            "job_id": job,
            "round": round,
            "files": [
                {"file_name": "track.flac", "key": object_key_for_file_name("track.flac"), "size": 7, "role": "data"},
                {"file_name": "set.par2", "key": object_key_for_file_name("set.par2"), "size": 8, "role": "par2"},
            ]
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const JOB: &str = "job_0123456789abcdef";

#[test]
fn health_answers_ok() {
    let f = Fixture::new("exit 0");
    let (status, body) = f.request(b"GET /health HTTP/1.1\r\nHost: c\r\n\r\n");
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["ok"],
        true
    );
}

#[test]
fn repairable_set_is_repaired_and_streamed_back() {
    // The fake binary "repairs" by rewriting the data file in the work dir.
    let f = Fixture::new("printf 'GOODBYTES' > track.flac\nprintf 'Repair complete.\\n'");
    let req = f.staged_request(JOB, 1);
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
fn repeated_repair_is_idempotent_and_runs_par2_once() {
    let f = Fixture::new("printf 'GOODBYTES' > track.flac\nprintf 'Repair complete.\\n'");
    let req = f.staged_request(JOB, 1);
    let (s1, r1) = f.post_repair(&req);
    let (s2, r2) = f.post_repair(&req);
    assert_eq!((s1, s2), (200, 200));
    assert_eq!(r1, r2);
    assert_eq!(f.invocations(), 1);
}

#[test]
fn different_manifest_for_same_round_conflicts() {
    let f = Fixture::new("printf 'GOODBYTES' > track.flac");
    let req = f.staged_request(JOB, 1);
    assert_eq!(f.post_repair(&req).0, 200);
    let mut other = req.clone();
    other["files"][0]["size"] = serde_json::json!(3);
    let (status, body) = f.post_repair(&other);
    assert_eq!(status, 409);
    assert_eq!(body["error"], "repair_manifest_conflict");
    assert_eq!(f.invocations(), 1);
}

#[test]
fn unrepairable_set_reports_unrepairable() {
    let f = Fixture::new("exit 1");
    let req = f.staged_request(JOB, 1);
    let (status, result) = f.post_repair(&req);
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["status"], "unrepairable");
    assert_eq!(result["exit_code"], 1);
}

#[test]
fn exit_6_with_recovery_impossible_line_is_unrepairable() {
    let f = Fixture::new(
        "printf 'Recovery will not be possible because the target file is corrupt.\\n'\nexit 6",
    );
    let req = f.staged_request(JOB, 1);
    let (status, result) = f.post_repair(&req);
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["status"], "unrepairable");
    assert_eq!(result["exit_code"], 6);
}

#[test]
fn exit_6_without_recovery_line_is_retryable() {
    // Exit 6 also covers ENOSPC and other I/O failures while par2
    // writes the repaired file; without the corrupt-set line it must
    // not become a permanent, replayed verdict (core RepairReport
    // contract).
    let f = Fixture::new("exit 6");
    let req = f.staged_request(JOB, 1);
    let (status, body) = f.post_repair(&req);
    assert_eq!(status, 503);
    assert_eq!(body["error"], "repair_failed");
}

#[test]
fn timeout_maps_to_stable_retryable_code() {
    let f = Fixture::new("sleep 10");
    let mut req = f.staged_request(JOB, 1);
    req["timeout_secs"] = serde_json::json!(1);
    let (status, body) = f.post_repair(&req);
    assert_eq!(status, 503);
    assert_eq!(body["error"], "repair_timeout");
}

#[test]
fn invalid_requests_are_rejected_with_stable_code() {
    let f = Fixture::new("exit 0");
    // Traversal in a file name.
    let mut req = f.staged_request(JOB, 1);
    req["files"][0]["file_name"] = serde_json::json!("../track.flac");
    let (status, body) = f.post_repair(&req);
    assert_eq!(status, 400);
    assert_eq!(body["error"], "invalid_request");

    // Key that is not sha256(file_name).
    let mut req = f.staged_request(JOB, 2);
    req["files"][0]["key"] = serde_json::json!("a".repeat(64));
    let (status, body) = f.post_repair(&req);
    assert_eq!(status, 400);
    assert_eq!(body["error"], "invalid_request");

    // Round zero.
    let mut req = f.staged_request(JOB, 3);
    req["round"] = serde_json::json!(0);
    assert_eq!(f.post_repair(&req).0, 400);
    assert_eq!(f.invocations(), 0);
}

#[test]
fn missing_staged_input_is_a_stable_error() {
    let f = Fixture::new("exit 0");
    // Nothing staged for round 1.
    let req = serde_json::json!({
        "job_id": JOB,
        "round": 1,
        "files": [
            {"file_name": "track.flac", "key": object_key_for_file_name("track.flac"), "size": 7, "role": "data"},
            {"file_name": "set.par2", "key": object_key_for_file_name("set.par2"), "size": 8, "role": "par2"},
        ]
    });
    let (status, body) = f.post_repair(&req);
    assert_eq!(status, 400);
    assert_eq!(body["error"], "scratch_input_mismatch");
    assert_eq!(f.invocations(), 0);
}

#[test]
fn get_out_rejects_traversing_job_segments() {
    let f = Fixture::new("exit 0");
    let key = object_key_for_file_name("track.flac");
    // A file outside the scratch root at the traversed location must
    // not be served: GET must reject the job segment, not just 404 on
    // a missing file.
    let outside = f.root.join("r1").join("out");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join(&key), b"OUTSIDE").unwrap();
    let raw = format!("GET /jobs/../r1/out/{key} HTTP/1.1\r\nHost: c\r\n\r\n");
    let (status, body) = f.request(raw.as_bytes());
    assert_eq!(status, 404);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
        "not_found"
    );
    let raw = format!("PUT /jobs/../r1/in/{key} HTTP/1.1\r\nHost: c\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(f.request(raw.as_bytes()).0, 400);
}

#[test]
fn unknown_routes_and_keys_are_404_with_stable_code() {
    let f = Fixture::new("exit 0");
    let (status, body) = f.get_out(JOB, 1, "nope.flac");
    assert_eq!(status, 404);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
        "not_found"
    );
    let (status, _) = f.request(b"GET /nope HTTP/1.1\r\nHost: c\r\n\r\n");
    assert_eq!(status, 404);
}
