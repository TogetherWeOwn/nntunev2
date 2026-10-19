//! Acceptance tests for the first-segment agreement arm of the covering
//! hint contract: when valid segment ranges (ground truth) disagree with
//! the first article (`first_seg == 0`), the article must be discarded and
//! refetched as a prefix walk — exactly like the covering (`first_seg !=
//! 0`) arm — and the provider-declared `=ybegin size=` must never define
//! the 206 window. Today the lying first article is adopted as-is, so the
//! window follows the declared size and the stream later truncates when
//! the truthful later articles fail chain validation against that total.
//!
//! Companion pins: an agreeing first segment is adopted with no refetch,
//! and with `segment_ranges: None` there is no ground truth, so the
//! declared size remains the only total and no refetch is attempted.
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
use nntune_stream::play::{play, PlayError, PlayInput, PlayStream, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"first-segment-refetch-secret";
const NOW_UNIX: u64 = 1_700_000_000;
// Play-path grants must sit within the TTL cap (`MAX_GRANT_TTL` = 900 s).
const EXP_UNIX: u64 = NOW_UNIX + 900;
const CIPHER_KEY: u8 = 0x5A;

const REAL_LEN: usize = 60_000;
const SEG_LEN: usize = 20_000;
/// Ranges are the ground truth in every test below: three 20_000-byte
/// segments covering the real 60_000-byte track.
const GROUND_TRUTH: [(u64, u64); 3] = [(0, 20_000), (20_000, 40_000), (40_000, 60_000)];
/// What a lying article 0 declares as the whole-file size (5 GiB).
const LYING_BIG_SIZE: u64 = 5_368_709_120;

/// Build a track whose article bytes, offsets and lengths all agree with
/// `GROUND_TRUTH`, but where article 0 declares `declared_first_size` as
/// the whole-file `=ybegin size=`. Later articles declare the real total.
fn fixture_track(tag: &str, declared_first_size: u64) -> (Vec<u8>, Vec<String>, Server) {
    let mut plain = Vec::with_capacity(REAL_LEN);
    for i in 0..REAL_LEN {
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
                    file_size: if i == 0 {
                        declared_first_size
                    } else {
                        REAL_LEN as u64
                    },
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
/// fetch order (see stream_integration.rs), so tests can observe a
/// discard-and-refetch as a duplicated first fetch.
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

struct Outcome {
    status: u16,
    content_range: Option<String>,
    body_len: u64,
    body: Vec<u8>,
    cached: Option<Vec<u8>>,
    stream_error_is_bad_part: bool,
    finalize_ok: bool,
    finalize_error_is_bad_part: bool,
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
    let stream_error_is_bad_part = matches!(
        stream.error(),
        Some(PlayError::Decode(yenc::YencError::BadPart))
    );
    let finalize = stream.finalize(&mut pool, &mut cache, &mut meter);
    let finalize_error_is_bad_part =
        matches!(finalize, Err(PlayError::Decode(yenc::YencError::BadPart)));
    Outcome {
        status,
        content_range,
        body_len,
        body,
        cached: cache.get_track("track-a").map(|b| b.to_vec()),
        stream_error_is_bad_part,
        finalize_ok: finalize.is_ok(),
        finalize_error_is_bad_part,
    }
}

fn refetch_order(segments: &[String]) -> Vec<String> {
    // Discard-and-prefix-walk: the first article is fetched, discarded,
    // then fetched again before the walk proceeds in file order.
    vec![
        segments[0].clone(),
        segments[0].clone(),
        segments[1].clone(),
        segments[2].clone(),
    ]
}

#[test]
fn disagreeing_first_segment_is_refetched_and_206_window_is_bounded_by_ranges() {
    // Article 0 declares a 5 GiB file; its bytes agree with the ground
    // truth ranges (offset 0, 20_000 bytes). The disagreeing first fetch
    // must be discarded and refetched as a prefix walk, and the 206
    // window must be bounded by the ranges total — never the declared
    // size — so the whole real track serves and caches.
    let (plain, segments, server) = fixture_track("big-lie", LYING_BIG_SIZE);
    let out = run(&server, &segments, "bytes=0-", Some(&GROUND_TRUTH));
    assert_eq!(out.status, 206);
    assert_eq!(out.content_range.as_deref(), Some("bytes 0-59999/60000"));
    assert_eq!(out.body_len, REAL_LEN as u64);
    assert_eq!(out.body, plain);
    assert!(out.finalize_ok, "refetched play must finalize cleanly");
    assert_eq!(out.cached.as_deref(), Some(plain.as_slice()));
    assert_eq!(
        server.fetched(),
        refetch_order(&segments),
        "disagreeing first segment must be discarded and refetched"
    );
}

#[test]
fn first_segment_declaring_a_smaller_file_is_refetched_and_full_track_serves() {
    // Article 0 declares only its own segment length as the file size.
    // Adopting it would cap the window at 20_000 bytes of a 60_000-byte
    // track; the refetch must serve the ranges-bounded full track.
    let (plain, segments, server) = fixture_track("small-lie", SEG_LEN as u64);
    let out = run(&server, &segments, "bytes=0-", Some(&GROUND_TRUTH));
    assert_eq!(out.status, 206);
    assert_eq!(out.content_range.as_deref(), Some("bytes 0-59999/60000"));
    assert_eq!(out.body_len, REAL_LEN as u64);
    assert_eq!(out.body, plain);
    assert!(out.finalize_ok, "refetched play must finalize cleanly");
    assert_eq!(out.cached.as_deref(), Some(plain.as_slice()));
    assert_eq!(
        server.fetched(),
        refetch_order(&segments),
        "disagreeing first segment must be discarded and refetched"
    );
}

#[test]
fn subrange_in_first_segment_reports_ranges_total_not_declared_size() {
    // Even when the requested slice sits entirely inside segment 0 (so
    // its bytes could be served from the lying article alone), the
    // Content-Range total must come from the ground-truth ranges and
    // the background backfill must cache the whole real track.
    let (plain, segments, server) = fixture_track("slice-lie", LYING_BIG_SIZE);
    let out = run(&server, &segments, "bytes=5000-5999", Some(&GROUND_TRUTH));
    assert_eq!(out.status, 206);
    assert_eq!(out.content_range.as_deref(), Some("bytes 5000-5999/60000"));
    assert_eq!(out.body_len, 1_000);
    assert_eq!(out.body, &plain[5_000..6_000]);
    assert!(out.finalize_ok, "backfill after the slice must succeed");
    assert_eq!(out.cached.as_deref(), Some(plain.as_slice()));
    assert_eq!(
        server.fetched(),
        refetch_order(&segments),
        "disagreeing first segment must be discarded and refetched"
    );
}

#[test]
fn agreeing_first_segment_is_adopted_without_refetch() {
    // Pin: when article 0 agrees with the ranges on size, offset and
    // length, it is adopted directly — one fetch per segment, in order.
    let (plain, segments, server) = fixture_track("agreeing", REAL_LEN as u64);
    let out = run(&server, &segments, "bytes=0-", Some(&GROUND_TRUTH));
    assert_eq!(out.status, 206);
    assert_eq!(out.content_range.as_deref(), Some("bytes 0-59999/60000"));
    assert_eq!(out.body, plain);
    assert!(out.finalize_ok);
    assert_eq!(out.cached.as_deref(), Some(plain.as_slice()));
    assert_eq!(
        server.fetched(),
        segments,
        "agreeing first segment must not be refetched"
    );
}

#[test]
fn no_ranges_prefix_walk_is_unchanged_for_truthful_articles() {
    // Pin: without ranges there is no hint to agree or disagree with;
    // the plain prefix walk serves and caches the truthful track.
    let (plain, segments, server) = fixture_track("no-ranges", REAL_LEN as u64);
    let out = run(&server, &segments, "bytes=5000-5099", None);
    assert_eq!(out.status, 206);
    assert_eq!(out.content_range.as_deref(), Some("bytes 5000-5099/60000"));
    assert_eq!(out.body, &plain[5_000..5_100]);
    assert!(out.finalize_ok);
    assert_eq!(out.cached.as_deref(), Some(plain.as_slice()));
    assert_eq!(server.fetched(), segments);
}

#[test]
fn no_ranges_declared_size_remains_the_only_total_and_is_not_refetched() {
    // Pin: without ground-truth ranges a lying declared size cannot be
    // detected at the first segment, so behavior is unchanged — the
    // window follows the declared size, the first segment is fetched
    // exactly once, and the play fails when the truthful second article
    // cannot chain onto the declared total. Only the ranges-present
    // disagreement above is the defect.
    let (plain, segments, server) = fixture_track("no-ranges-lie", LYING_BIG_SIZE);
    let out = run(&server, &segments, "bytes=0-", None);
    assert_eq!(out.status, 206);
    assert_eq!(
        out.content_range.as_deref(),
        Some("bytes 0-5368709119/5368709120")
    );
    assert_eq!(out.body, &plain[..SEG_LEN]);
    assert!(
        out.stream_error_is_bad_part,
        "second article must fail chain validation against the declared total"
    );
    assert!(!out.finalize_ok);
    assert!(out.finalize_error_is_bad_part);
    assert_eq!(out.cached, None, "failed play must not cache the track");
    assert_eq!(
        server.fetched(),
        vec![segments[0].clone(), segments[1].clone()],
        "first segment is fetched once: no refetch without ground truth"
    );
}
