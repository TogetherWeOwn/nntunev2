#![allow(dead_code)]
//! Shared live-server harness for the container integration tests.
//!
//! Each integration target compiles this module separately and uses a
//! different subset, so unused helpers are allowed here instead of
//! per-target.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use nntune_core::par2::scratch::sha256_hex;
use nntune_transcode_server::{serve, ServerConfig};

pub const HEX_A: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
pub const HEX_B: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// One real encoder-made Ogg page (Vorbis BOS, serial `0x47b3c05d`,
/// stored CRC `0x271310d6`), copied from the core normaliser tests:
/// the stub's canned encode output. The server must rewrite its
/// serial to 1 and recompute the CRC.
pub const CANNED_PAGE: &[u8; 58] = &[
    0x4f, 0x67, 0x67, 0x53, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5d, 0xc0,
    0xb3, 0x47, 0x00, 0x00, 0x00, 0x00, 0xd6, 0x10, 0x13, 0x27, 0x01, 0x1e, 0x01, 0x76, 0x6f, 0x72,
    0x62, 0x69, 0x73, 0x00, 0x00, 0x00, 0x00, 0x02, 0x44, 0xac, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x03, 0x71, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xb8, 0x01,
];

/// Stub encoder: copies the canned output to its last argv (the
/// output path) unless the mode file says otherwise. Modes: `ok`
/// (copy, exit 0), `fail` (exit 1), `junk` (write non-Ogg, exit 0),
/// `big` (write 33 MiB of zeros, exit 0), `slow` (sleep 30 — the
/// request timeout kills it first). Every invocation appends one
/// line to the count file. Paths are baked into the script text (not
/// environment variables) so parallel tests never share one stub's
/// control files.
pub fn stub_script(output_src: &str, mode_file: &str, count_file: &str) -> String {
    format!(
        r#"#!/bin/sh
for last in "$@"; do :; done
echo x >> "{count_file}"
mode="$(cat "{mode_file}")"
if [ "$mode" = "fail" ]; then exit 1; fi
if [ "$mode" = "slow" ]; then exec sleep 30; fi
if [ "$mode" = "junk" ]; then printf 'not-ogg-bytes' > "$last"; exit 0; fi
if [ "$mode" = "big" ]; then dd if=/dev/zero of="$last" bs=1M count=33 2>/dev/null; exit 0; fi
cp "{output_src}" "$last"
"#
    )
}

pub static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

pub struct Harness {
    addr: String,
    root: PathBuf,
    mode_file: PathBuf,
    count_file: PathBuf,
}

impl Harness {
    pub fn start() -> Self {
        let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "nntune-transcode-test-{}-{}",
            std::process::id(),
            seq
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("scratch")).unwrap();
        let canned = root.join("canned.ogg");
        fs::write(&canned, CANNED_PAGE).unwrap();
        let mode_file = root.join("mode");
        fs::write(&mode_file, "ok").unwrap();
        let count_file = root.join("count");
        fs::write(&count_file, "").unwrap();
        let stub = root.join("stub-ffmpeg.sh");
        fs::write(
            &stub,
            stub_script(
                &canned.to_string_lossy(),
                &mode_file.to_string_lossy(),
                &count_file.to_string_lossy(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&stub).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&stub, perms).unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let config = Arc::new(ServerConfig {
            scratch_root: root.join("scratch"),
            ffmpeg_bin: stub,
        });
        std::thread::spawn(move || {
            let _ = serve(listener, config);
        });
        Self {
            addr,
            root,
            mode_file,
            count_file,
        }
    }

    pub fn set_mode(&self, mode: &str) {
        fs::write(&self.mode_file, mode).unwrap();
    }

    pub fn invocation_count(&self) -> usize {
        fs::read_to_string(&self.count_file)
            .unwrap()
            .lines()
            .count()
    }

    /// Inspect every scratch file, including temporary inputs and outputs.
    pub fn scratch_files(&self) -> Vec<PathBuf> {
        fn visit(dir: &std::path::Path, files: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, files);
                } else {
                    files.push(path);
                }
            }
        }
        let mut files = Vec::new();
        visit(&self.root.join("scratch"), &mut files);
        files.sort();
        files
    }

    /// A bounded connection for tests that deliberately leave the write
    /// side open or half-close it to signal a truncated request.
    pub fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect(&self.addr).unwrap();
        let timeout = Some(std::time::Duration::from_secs(2));
        stream.set_read_timeout(timeout).unwrap();
        stream.set_write_timeout(timeout).unwrap();
        stream
    }

    pub fn raw(&self, head: &str, body: &[u8]) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(&self.addr).unwrap();
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        let mut out = Vec::new();
        stream.read_to_end(&mut out).unwrap();
        let text = String::from_utf8_lossy(&out);
        let status: u16 = text
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let split = out
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| i + 4)
            .unwrap_or(out.len());
        (status, out[split..].to_vec())
    }

    pub fn post_json(&self, path: &str, body: &str) -> (u16, serde_json::Value) {
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let (status, bytes) = self.raw(&head, body.as_bytes());
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    pub fn put(&self, path: &str, body: &[u8]) -> (u16, serde_json::Value) {
        let head = format!(
            "PUT {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let (status, bytes) = self.raw(&head, body);
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    pub fn get_bytes(&self, path: &str) -> (u16, Vec<u8>) {
        self.raw(
            &format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
            b"",
        )
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn input_key() -> String {
    format!("0007-{HEX_A}")
}

pub fn stage_input(h: &Harness, job: &str, staging: &str, bytes: &[u8]) -> String {
    let key = input_key();
    let (status, reply) = h.put(&format!("/jobs/{job}/t/{staging}/in/{key}"), bytes);
    assert_eq!(status, 201, "PUT input: {reply:?}");
    assert_eq!(reply["key"], key);
    sha256_hex(bytes)
}

pub fn transcode_body(
    job: &str,
    staging: &str,
    source_sha256: &str,
    timeout: Option<u64>,
) -> String {
    let mut value = serde_json::json!({
        "job_id": job,
        "staging_id": staging,
        "input_key": input_key(),
        "source_sha256": source_sha256,
    });
    if let Some(secs) = timeout {
        value["timeout_secs"] = secs.into();
    }
    value.to_string()
}
