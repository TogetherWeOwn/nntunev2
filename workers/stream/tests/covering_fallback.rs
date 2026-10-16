//! Regression tests for the covering-segment *hint* contract: ranges are
//! verified, never trusted blindly. Valid-shape but wrong-valued ranges
//! must fall back to the prefix walk and still serve a playable track, and
//! a covering seek whose range runs to end-of-track must serve every byte
//! (exercising the prefix backfill that bounds out-of-order staging to the
//! covering segment). Companion to `stream_integration.rs`, which pins the
//! happy-path covering fetch and the no-ranges prefix walk.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::MemoryMeter;
use nntune_stream::play::{play, PlayInput, PlayStream, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"covering-fallback-secret";
const NOW_UNIX: u64 = 1_700_000_000;
const CIPHER_KEY: u8 = 0x5A;

fn fixture_track(tag: &str, len: usize, seg_len: usize) -> (Vec<u8>, Vec<String>, Server) {
    let mut plain = Vec::with_capacity(len);
    for i in 0..len {
        plain.push(((i * 7919 + tag.len()) % 256) as u8);
    }
    let ciphered: Vec<u8> = plain.iter().map(|b| b ^ CIPHER_KEY).collect();
    let file_crc = yenc::crc32(&ciphered);
    let total = ciphered.len().div_ceil(seg_len) as u32;
    let mut articles = HashMap::new();
    let mut segments = Vec::new();
    for (i, chunk) in ciphered.chunks(seg_len).enumerate() {
        let part = i as u32 + 1;
        let mid = format!("<{tag}-seg{i}@synthetic.test>");
        articles.insert(
            mid.clone(),
            yenc::encode_part(
                chunk,
                &format!("{tag}.flac"),
                PartSpec {
                    file_size: len as u64,
                    part,
                    total,
                    offset: (i * seg_len) as u64,
                    file_crc: (part == total).then_some(file_crc),
                },
            ),
        );
        segments.push(mid);
    }
    (plain, segments, Server::start(articles))
}

/// Minimal NNTP server counting `BODY` commands (see stream_integration.rs).
struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    bodies: Arc<AtomicUsize>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn start(articles: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
        let addr = listener.local_addr().expect("local addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let bodies = Arc::new(AtomicUsize::new(0));
        let (stop_c, bodies_c) = (Arc::clone(&stop), Arc::clone(&bodies));
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
                Self::serve(stream, &articles, &bodies_c);
            }
        });
        Self {
            addr,
            stop,
            bodies,
            handle: Some(handle),
        }
    }

    fn serve(stream: TcpStream, articles: &HashMap<String, Vec<u8>>, bodies: &AtomicUsize) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(stream.try_clone().expect("clone conn"));
        let mut writer = stream;
        let _ = writer.write_all(b"200 synthetic NNTP ready\r\n");
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            while line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
                line.pop();
            }
            let cmd = String::from_utf8_lossy(&line).into_owned();
            if let Some(mid) = cmd
                .strip_prefix("BODY ")
                .map(|s| s.trim().trim_matches(|c| c == '<' || c == '>'))
            {
                bodies.fetch_add(1, Ordering::SeqCst);
                let full_mid = format!("<{mid}>");
                match articles.get(&full_mid) {
                    Some(article) => {
                        let _ =
                            writer.write_all(format!("222 0 {full_mid} follows\r\n").as_bytes());
                        for raw_line in article.split(|&b| b == b'\n') {
                            let text_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
                            if text_line.first() == Some(&b'.') {
                                let _ = writer.write_all(b".");
                            }
                            let _ = writer.write_all(text_line);
                            let _ = writer.write_all(b"\r\n");
                        }
                        let _ = writer.write_all(b".\r\n");
                    }
                    None => {
                        let _ = writer.write_all(b"430 no such article\r\n");
                    }
                }
            } else if cmd == "QUIT" {
                let _ = writer.write_all(b"205 bye\r\n");
                break;
            } else {
                let _ = writer.write_all(b"500 unknown command\r\n");
            }
        }
    }

    fn bodies(&self) -> usize {
        self.bodies.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run(
    server: &Server,
    segments: &[String],
    range: &str,
    ranges: &[(u64, u64)],
) -> (u16, Vec<u8>, Vec<u8>) {
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let token = grant::issue("user-1", "track-a", 9_999_999_999, SECRET);
    let inp = PlayInput {
        token: &token,
        track_id: "track-a",
        range: Some(range),
        server_addr: server.addr,
        segments,
        segment_ranges: Some(ranges),
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("play must not fail");
    let status = resp.status;
    let mut stream: PlayStream = resp.stream;
    let mut body = Vec::new();
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    let cached = cache
        .get_track("track-a")
        .map(|b| b.to_vec())
        .unwrap_or_default();
    (status, body, cached)
}

#[test]
fn ranges_that_understate_the_file_do_not_416_a_satisfiable_seek() {
    // Real file is 30_000 bytes; ranges claim 20_000. A seek to 25_000 is
    // satisfiable against the real file and must be served, not 416d from
    // the unverified hint.
    let (plain, segments, server) = fixture_track("a", 30_000, 15_000);
    let lying = [(0u64, 10_000u64), (10_000, 20_000)];
    let (status, body, cached) = run(&server, &segments, "bytes=25000-25099", &lying);
    assert_eq!(status, 206);
    assert_eq!(body, &plain[25_000..25_100]);
    assert_eq!(cached, plain);
}

#[test]
fn ranges_disagreeing_with_the_covering_article_fall_back_and_serve() {
    // Total is off by one (60_001 vs the real 60_000): the covering fetch
    // succeeds, verification rejects the hint, and the prefix walk serves
    // the seek instead of failing the play with a decode error.
    let (plain, segments, server) = fixture_track("a", 60_000, 20_000);
    let lying = [(0u64, 20_000u64), (20_000, 40_000), (40_000, 60_001)];
    let (status, body, cached) = run(&server, &segments, "bytes=45000-45099", &lying);
    assert_eq!(status, 206);
    assert_eq!(body, &plain[45_000..45_100]);
    assert_eq!(cached, plain);
}

#[test]
fn wrong_interior_boundary_in_ranges_still_serves() {
    // Total and the covering segment agree with the hint, but the interior
    // boundary lies (hint splits 20k/10k/30k, articles are 20k/20k/20k).
    // Placement is proven by chaining article offsets, not the hint, so
    // the whole remainder serves and caches exactly.
    let (plain, segments, server) = fixture_track("a", 60_000, 20_000);
    let lying = [(0u64, 20_000u64), (20_000, 30_000), (30_000, 60_000)];
    let (status, body, cached) = run(&server, &segments, "bytes=5000-", &lying);
    assert_eq!(status, 206);
    assert_eq!(body, &plain[5_000..]);
    assert_eq!(cached, plain);
}

#[test]
fn covering_seek_running_to_end_of_track_serves_every_byte() {
    // Open-ended seek from segment 1 of 4: serving advances past the
    // covering segment, which first backfills segment 0 so the covering
    // segment flushes into staging instead of accumulating the remainder
    // out of order. First byte still costs one BODY; every byte of the
    // remainder and the cached whole track must be exact.
    let (plain, segments, server) = fixture_track("a", 80_000, 20_000);
    let ranges = [
        (0u64, 20_000u64),
        (20_000, 40_000),
        (40_000, 60_000),
        (60_000, 80_000),
    ];
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let token = grant::issue("user-1", "track-a", 9_999_999_999, SECRET);
    let inp = PlayInput {
        token: &token,
        track_id: "track-a",
        range: Some("bytes=25000-"),
        server_addr: server.addr,
        segments: &segments,
        segment_ranges: Some(&ranges),
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("covering play");
    assert_eq!(resp.status, 206);
    let mut stream = resp.stream;
    let first = stream.next().expect("first chunk");
    assert_eq!(server.bodies(), 1, "covering segment only, to first byte");
    let mut body = first;
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body, &plain[25_000..]);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    assert_eq!(
        cache.get_track("track-a").as_deref(),
        Some(plain.as_slice())
    );
}
