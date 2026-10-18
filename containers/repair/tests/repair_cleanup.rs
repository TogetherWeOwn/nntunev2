//! Cleanup isolation tests for the repair Container server, in a
//! distinct target from the framing tests in `repair_server.rs`.
//!
//! Pins the existing `/cleanup` wire semantics: removing one round
//! removes only that round, repeat cleanup keeps the success envelope,
//! invalid job/round is a stable 400 that preserves valid data, and
//! cleanup never runs `par2` again.
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
            "nntune-repair-cleanup-test-{}-{seq}",
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

    fn post_cleanup(&self, body: &serde_json::Value) -> (u16, serde_json::Value) {
        let body = serde_json::to_vec(body).unwrap();
        let head = format!(
            "POST /cleanup HTTP/1.1\r\nHost: container\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
            "staging_id": "staging-0000000000000001",
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
fn cleanup_removes_only_selected_round_and_is_repeatable() {
    let f = Fixture::new("printf 'GOODBYTES' > track.flac\nprintf 'Repair complete.\\n'");

    // Stage inputs and repaired outputs for two rounds.
    let req1 = f.staged_request(JOB, 1);
    let req2 = f.staged_request(JOB, 2);
    let (s1, r1) = f.post_repair(&req1);
    let (s2, r2) = f.post_repair(&req2);
    assert_eq!((s1, s2), (200, 200), "{r1} / {r2}");
    assert_eq!(r1["status"], "repaired");
    assert_eq!(r2["status"], "repaired");
    assert_eq!(f.invocations(), 2);
    let (s1, out1) = f.get_out(JOB, 1, "track.flac");
    let (s2, out2) = f.get_out(JOB, 2, "track.flac");
    assert_eq!((s1, s2), (200, 200));
    assert_eq!(out1, b"GOODBYTES");
    assert_eq!(out2, b"GOODBYTES");

    // Cleanup round 1 removes only that round.
    let (status, body) = f.post_cleanup(&serde_json::json!({"job_id": JOB, "round": 1}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(f.invocations(), 2, "cleanup must not run par2");
    assert!(!f.root.join("scratch").join(JOB).join("r1").exists());
    let (status, _) = f.get_out(JOB, 1, "track.flac");
    assert_eq!(status, 404);

    // The other round's output bytes are unchanged.
    let (status, kept) = f.get_out(JOB, 2, "track.flac");
    assert_eq!(status, 200);
    assert_eq!(kept, out2);

    // Repeat cleanup keeps the existing success envelope and still
    // runs no par2.
    let (status, body) = f.post_cleanup(&serde_json::json!({"job_id": JOB, "round": 1}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(f.invocations(), 2);
    let (status, kept_again) = f.get_out(JOB, 2, "track.flac");
    assert_eq!(status, 200);
    assert_eq!(kept_again, out2);

    // Cleanup of a never-used but valid round tolerates the absent path.
    let (status, body) = f.post_cleanup(&serde_json::json!({"job_id": JOB, "round": 9}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(f.invocations(), 2);

    // Invalid job or round is a stable 400 and preserves valid data.
    let (status, body) = f.post_cleanup(&serde_json::json!({"job_id": "../evil", "round": 2}));
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "invalid_request");
    let (status, body) = f.post_cleanup(&serde_json::json!({"job_id": JOB, "round": 0}));
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "invalid_request");
    assert_eq!(f.invocations(), 2);
    let (status, kept_final) = f.get_out(JOB, 2, "track.flac");
    assert_eq!(status, 200);
    assert_eq!(kept_final, out2);
}
