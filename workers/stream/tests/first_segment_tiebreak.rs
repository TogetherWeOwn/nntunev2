//! Tie-break pins for the segment-0 agreement arm: segment 0 alone cannot
//! tell a lying provider `size=` (ranges win) from lying ranges (provider
//! wins), so a disagreeing segment 0 refetches and corroborates with
//! segment 1. When segment 1 agrees with the provider against lying ranges,
//! the provider total stands and the full track still serves — wrong ranges
//! must never make a playable track unplayable. Companion to
//! `first_segment_refetch.rs`, which pins the ranges-win direction.
//! Synthetic segments only; no provider network.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::MemoryMeter;
use nntune_stream::play::{play, PlayInput, PlayStream, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"first-segment-tiebreak-secret";
const NOW_UNIX: u64 = 1_700_000_000;
// Play-path grants must sit within the TTL cap (`MAX_GRANT_TTL` = 900 s).
const EXP_UNIX: u64 = NOW_UNIX + 900;
const CIPHER_KEY: u8 = 0x5A;

const SEG_LEN: usize = 20_000;

/// Build a fully truthful track: every article declares `len` as the whole
/// file size, with correct offsets. The caller supplies the ranges hint,
/// which may lie.
fn fixture_track(tag: &str, len: usize) -> (Vec<u8>, Vec<String>, Server) {
    let mut plain = Vec::with_capacity(len);
    for i in 0..len {
        plain.push(((i * 7919 + tag.len()) % 256) as u8);
    }
    let ciphered: Vec<u8> = plain.iter().map(|b| b ^ CIPHER_KEY).collect();
    let file_crc = yenc::crc32(&ciphered);
    let total = ciphered.len().div_ceil(SEG_LEN) as u32;
    let mut articles = HashMap::new();
    let mut segments = Vec::new();
    for (i, chunk) in ciphered.chunks(SEG_LEN).enumerate() {
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
                    offset: (i * SEG_LEN) as u64,
                    file_crc: (part == total).then_some(file_crc),
                },
            ),
        );
        segments.push(mid);
    }
    (plain, segments, Server::start(articles))
}

/// Minimal NNTP server recording the message-ids of `BODY` commands in
/// fetch order, so tests can observe the refetch and the corroborating read.
struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    fetched: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn start(articles: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake NNTP");
        let addr = listener.local_addr().expect("local addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let (stop_c, fetched_c) = (Arc::clone(&stop), Arc::clone(&fetched));
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
                Self::serve(stream, &articles, &fetched_c);
            }
        });
        Self {
            addr,
            stop,
            fetched,
            handle: Some(handle),
        }
    }

    fn serve(stream: TcpStream, articles: &HashMap<String, Vec<u8>>, fetched: &Mutex<Vec<String>>) {
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
                let full_mid = format!("<{mid}>");
                fetched.lock().expect("fetched log").push(full_mid.clone());
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

    fn fetched(&self) -> Vec<String> {
        self.fetched.lock().expect("fetched log").clone()
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
    ranges: Option<&[(u64, u64)]>,
) -> Outcome {
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let token = grant::issue("user-1", "track-a", EXP_UNIX, SECRET);
    let inp = PlayInput {
        trace_id: None,
        token: &token,
        track_id: "track-a",
        range: Some(range),
        server_addr: server.addr,
        segments,
        segment_ranges: ranges,
        cipher_key: CIPHER_KEY,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &inp).expect("play must not fail");
    let (status, content_range, body_len) =
        (resp.status, resp.content_range.clone(), resp.body_len);
    let mut stream: PlayStream = resp.stream;
    let mut body = Vec::new();
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    let finalize = stream.finalize(&mut pool, &mut cache, &mut meter);
    Outcome {
        status,
        content_range,
        body_len,
        body,
        cached: cache.get_track("track-a").map(|b| b.to_vec()),
        finalize_ok: finalize.is_ok(),
    }
}

struct Outcome {
    status: u16,
    content_range: Option<String>,
    body_len: u64,
    body: Vec<u8>,
    cached: Option<Vec<u8>>,
    finalize_ok: bool,
}

#[test]
fn lying_ranges_total_with_seek_at_zero_keeps_provider_total_and_serves() {
    // All three articles truthfully declare 60_000 bytes, but the
    // valid-shape ranges claim 60_001. Seeking at byte 0 disagrees on the
    // total; segment 1 corroborates the provider, so the provider total
    // stands and the whole real track serves and caches — the mirror of the
    // ranges-win direction. Fetch order shows the discard-and-refetch plus
    // the single corroborating read, with segment 1 served from that read.
    let (plain, segments, server) = fixture_track("ranges-lie", 60_000);
    let lying = [(0u64, 20_000u64), (20_000, 40_000), (40_000, 60_001)];
    let out = run(&server, &segments, "bytes=0-", Some(&lying));
    assert_eq!(out.status, 206);
    assert_eq!(out.content_range.as_deref(), Some("bytes 0-59999/60000"));
    assert_eq!(out.body_len, 60_000);
    assert_eq!(out.body, plain);
    assert!(out.finalize_ok, "provider-win play must finalize cleanly");
    assert_eq!(out.cached.as_deref(), Some(plain.as_slice()));
    assert_eq!(
        server.fetched(),
        vec![
            segments[0].clone(),
            segments[0].clone(),
            segments[1].clone(),
            segments[2].clone(),
        ],
        "disagreeing segment 0 must be discarded, refetched, then corroborated"
    );
}

#[test]
fn single_segment_with_lying_ranges_total_keeps_legacy_provider_total() {
    // One truthful 20_000-byte article against ranges claiming 20_001: no
    // second article exists to corroborate, so the legacy adopt stands and
    // the track serves on the provider total with a single fetch.
    let (plain, segments, server) = fixture_track("single-lie", SEG_LEN);
    assert_eq!(segments.len(), 1);
    let lying = [(0u64, 20_001u64)];
    let out = run(&server, &segments, "bytes=0-", Some(&lying));
    assert_eq!(out.status, 206);
    assert_eq!(out.content_range.as_deref(), Some("bytes 0-19999/20000"));
    assert_eq!(out.body_len, SEG_LEN as u64);
    assert_eq!(out.body, plain);
    assert!(out.finalize_ok);
    assert_eq!(out.cached.as_deref(), Some(plain.as_slice()));
    assert_eq!(
        server.fetched(),
        vec![segments[0].clone()],
        "single-segment disagreement keeps the legacy single fetch"
    );
}
