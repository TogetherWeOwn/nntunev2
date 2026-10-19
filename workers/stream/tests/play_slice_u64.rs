//! Acceptance tests: staged-byte play slicing stays in `u64` until the
//! final in-bounds slice index.
//!
//! Do not edit these tests to make them pass: implement the contract in
//! production code. These files are owned by the test author; test
//! corrections stay with them.
//!
//! Contract under test (source `b8e8f744`, `workers/stream/src/play.rs`):
//!
//! - `:1374-1375` (cached serve) computes the chunk window as
//!   `(self.pos as usize + CHUNK).min(self.end as usize)` and slices
//!   `bytes[self.pos as usize..to]`;
//! - `:1383-1386` (live serve) computes `from`/`to` from
//!   `(self.pos - live.buf_start) as usize` and
//!   `(self.end - live.buf_start) as usize`, then slices `live.buf`;
//! - `:983` reports `self.staged_bytes as usize` into
//!   `YencError::SizeMismatch.actual` (today declared `usize` in
//!   `workers/stream/src/yenc.rs:20-23` next to a `u64` `expected`).
//!
//! `pos`, `end`, `buf_start` and `staged_bytes` are `u64` file offsets.
//! Narrowing them with `as usize` *before* the arithmetic/clamp truncates
//! on 32-bit targets (`wasm32`, the production Worker: `usize` is 32-bit):
//! a multi-GiB file offset wraps, so `end - buf_start` can collapse to a
//! small value (wrong/empty slice, or a slice-index panic) and a staged
//! byte total above 4 GiB is misreported. The fix keeps offset arithmetic
//! in `u64`, clamps against the in-memory buffer length in `u64`, and only
//! then converts the already-bounded local to the final `usize` slice
//! index (a clamped local cast or `usize::try_from` both pass); it also
//! widens `SizeMismatch.actual` so it carries the full staged total.
//!
//! Why source gates: CI hosts are 64-bit, where `as usize` is lossless and
//! every behavioural case below already passes, so no native behavioural
//! test can go red on the pre-fix code (a behavioural pin would need an
//! in-memory body over 4 GiB). The three source gates parse `play.rs` /
//! `yenc.rs` with `syn` and inspect token streams, so comments cannot
//! hide a cast and any host fails. They forbid only the narrowing shapes
//! named above — never cast `self.pos` / `self.end` / a `buf_start`
//! difference / `staged_bytes` directly, and never do arithmetic after a
//! narrowing cast (`as usize +`). Deliberately unpinned: the exact
//! widening syntax, local variable names, helper extraction, and the
//! chunk size.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - restoring any named narrowing in `Iterator for PlayStream` fails
//!   `play_slicing_keeps_offsets_in_u64_until_final_index`;
//! - restoring `staged_bytes as usize` anywhere in `play.rs` fails
//!   `staged_byte_count_is_not_narrowed_into_size_mismatch`;
//! - keeping `SizeMismatch.actual: usize` fails
//!   `size_mismatch_actual_field_is_u64`;
//! - breaking exact/off-by-one slicing (chunk, segment or range edges)
//!   fails the cached/live behavioural controls, which are green before
//!   and after the fix.
//!
//! Out of scope: sibling suites (untouched), checksum precedence, the
//! yEnc single-part size compares, and any range-parsing change.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-stream --test play_slice_u64
//! ```

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
use nntune_stream::play::{play, PlayInput, PlayResponse, PlayStream, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::yenc::{self, PartSpec, YencError};
use quote::ToTokens;

const SECRET: &[u8] = b"play-slice-u64-secret";
const NOW_UNIX: u64 = 1_700_000_000;
// Within the grant TTL cap (`MAX_GRANT_TTL` = 900 s).
const EXP_UNIX: u64 = NOW_UNIX + 900;
/// Larger than the 16 KiB serve chunk and a 20_000-byte segment, so both
/// boundaries fall inside the body.
const BODY_LEN: usize = 40_000;
const SEG_LEN: usize = 20_000;
/// Today's serve chunk (`play.rs` `CHUNK`): an adjacent-behaviour control,
/// not part of the u64 contract.
const CHUNK_CEILING: usize = 16 * 1024;

/// Play-path source, read as text and parsed (not imported): the RED
/// gates pin the narrowing textually via token streams so they fail on
/// 64-bit hosts too.
const PLAY_RS: &str = include_str!("../src/play.rs");
const YENC_RS: &str = include_str!("../src/yenc.rs");

/// Deterministic 40_000-byte plaintext.
fn body() -> Vec<u8> {
    (0..BODY_LEN).map(|i| (i % 251) as u8).collect()
}

// ---------------------------------------------------------------------------
// Source-text RED gates: fail on the pre-fix source on any host.
// ---------------------------------------------------------------------------

fn parse(src: &str, name: &str) -> syn::File {
    syn::parse_file(src).unwrap_or_else(|e| panic!("{name} must parse as Rust: {e}"))
}

/// Token text of the `impl Iterator for PlayStream` block (comments are
/// not tokens, so a commented-out cast cannot hide here and a cast named
/// in a comment cannot false-positive).
fn play_stream_iterator_tokens() -> String {
    let file = parse(PLAY_RS, "play.rs");
    for item in &file.items {
        let syn::Item::Impl(item_impl) = item else {
            continue;
        };
        let is_iterator = item_impl.trait_.as_ref().is_some_and(|(path, _)| {
            path.segments
                .last()
                .is_some_and(|seg| seg.ident == "Iterator")
        });
        let self_ty = item_impl.self_ty.to_token_stream().to_string();
        if is_iterator && self_ty.contains("PlayStream") {
            return item_impl.to_token_stream().to_string();
        }
    }
    panic!("play.rs must still contain `impl Iterator for PlayStream`");
}

#[test]
fn play_slicing_keeps_offsets_in_u64_until_final_index() {
    let tokens = play_stream_iterator_tokens();
    // Token-stream text is single-spaced: `self . pos as usize`.
    let forbidden: [(&str, &str); 4] = [
        (
            "self . pos as usize",
            "the cached window/slice narrows the u64 `pos` before clamping",
        ),
        (
            "self . end as usize",
            "the cached window narrows the u64 `end` before clamping",
        ),
        (
            "buf_start) as usize",
            "a `pos - buf_start` / `end - buf_start` difference is narrowed \
             before the buffer-length clamp",
        ),
        (
            "as usize +",
            "arithmetic runs after a narrowing cast instead of in u64",
        ),
    ];
    let violations: Vec<String> = forbidden
        .iter()
        .filter(|(pattern, _)| tokens.contains(pattern))
        .map(|(pattern, why)| format!("`{pattern}` ({why})"))
        .collect();
    assert!(
        violations.is_empty(),
        "`impl Iterator for PlayStream` in workers/stream/src/play.rs still \
         narrows u64 play offsets to usize before slicing: {}. Keep offset \
         arithmetic in u64, clamp against the buffer length in u64, and \
         cast only the already-bounded local to the final slice index.",
        violations.join("; ")
    );
    // The serve paths must still exist (deleting them would otherwise
    // pass the negative pin and fail only behaviourally below).
    assert!(
        tokens.contains("Cached") && tokens.contains("Live"),
        "PlayStream::next must still serve both the cached and live sources"
    );
}

#[test]
fn staged_byte_count_is_not_narrowed_into_size_mismatch() {
    let tokens = parse(PLAY_RS, "play.rs").to_token_stream().to_string();
    assert!(
        !tokens.contains("staged_bytes as usize"),
        "workers/stream/src/play.rs still narrows `staged_bytes` with \
         `as usize`; report the full u64 staged total in \
         `SizeMismatch.actual`"
    );
    assert!(
        tokens.contains("staged_bytes") && tokens.contains("SizeMismatch"),
        "play.rs must still track the staged total and report a size \
         mismatch when it disagrees with the declared file size"
    );
}

#[test]
fn size_mismatch_actual_field_is_u64() {
    let file = parse(YENC_RS, "yenc.rs");
    let mut fields: Vec<(String, String)> = Vec::new();
    for item in &file.items {
        let syn::Item::Enum(item_enum) = item else {
            continue;
        };
        if item_enum.ident != "YencError" {
            continue;
        }
        for variant in &item_enum.variants {
            if variant.ident != "SizeMismatch" {
                continue;
            }
            if let syn::Fields::Named(named) = &variant.fields {
                for field in &named.named {
                    if let Some(ident) = &field.ident {
                        fields.push((ident.to_string(), field.ty.to_token_stream().to_string()));
                    }
                }
            }
        }
    }
    assert!(
        !fields.is_empty(),
        "yenc.rs must still declare `YencError::SizeMismatch` with named \
         `expected`/`actual` fields"
    );
    for (name, ty) in &fields {
        assert_eq!(
            ty, "u64",
            "SizeMismatch.{name} must be u64 so it carries the full value \
             on 32-bit targets (found `{ty}`)"
        );
    }
}

// ---------------------------------------------------------------------------
// Behavioural controls: green before and after the fix. Exact and
// off-by-one slicing at chunk, segment and body edges, on both the
// cached (`Source::Cached`) and live/staged (`Source::Live`) paths.
// ---------------------------------------------------------------------------

struct CachedRig {
    pool: StreamPool,
    cache: ContentCache,
    meter: MemoryMeter,
}

impl CachedRig {
    /// Cache already holds `plain` for `track`: plays are hits and never
    /// dial the dummy server address.
    fn seeded(track: &str, plain: Vec<u8>) -> Self {
        let mut cache = ContentCache::new();
        cache.put_track(track, plain);
        Self {
            pool: Pool::new(1),
            cache,
            meter: MemoryMeter::default(),
        }
    }

    fn play_range(
        &mut self,
        track: &str,
        range: Option<&str>,
    ) -> Result<PlayResponse, nntune_stream::play::PlayError> {
        let token = grant::issue("u1", track, EXP_UNIX, SECRET);
        let addr: SocketAddr = "127.0.0.1:119".parse().expect("dummy addr");
        let input = PlayInput {
            trace_id: None,
            token: &token,
            track_id: track,
            range,
            server_addr: addr,
            segments: &[],
            segment_ranges: None,
            cipher_key: 0,
            now_unix: NOW_UNIX,
            up_next: None,
        };
        play(
            &mut self.pool,
            &mut self.cache,
            &mut self.meter,
            SECRET,
            &input,
        )
    }

    /// Drain while checking the chunking invariants every slice must keep:
    /// no empty chunks (a truncated window computing `to <= from` would
    /// stall or panic) and no chunk past the serve-chunk ceiling.
    fn drain(&mut self, mut stream: PlayStream) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in stream.by_ref() {
            assert!(!chunk.is_empty(), "a served chunk must never be empty");
            assert!(
                chunk.len() <= CHUNK_CEILING,
                "chunk of {} bytes exceeds the serve-chunk ceiling",
                chunk.len()
            );
            out.extend_from_slice(&chunk);
        }
        stream
            .finalize(&mut self.pool, &mut self.cache, &mut self.meter)
            .expect("finalize play");
        out
    }
}

#[test]
fn cached_full_play_slices_every_chunk_in_order() {
    let plain = body();
    let mut rig = CachedRig::seeded("track-full", plain.clone());
    let resp = rig
        .play_range("track-full", None)
        .expect("full play serves");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body_len, BODY_LEN as u64);
    assert!(resp.cache_hit);
    let served = rig.drain(resp.stream);
    assert_eq!(served, plain, "every byte served exactly once, in order");
}

#[test]
fn cached_range_crossing_chunk_boundary_serves_exact_bytes() {
    let plain = body();
    let mut rig = CachedRig::seeded("track-chunk", plain.clone());
    // 16_380..=16_390 straddles the first 16 KiB chunk boundary (16_384).
    let resp = rig
        .play_range("track-chunk", Some("bytes=16380-16390"))
        .expect("chunk-crossing range serves");
    assert_eq!(resp.status, 206);
    assert_eq!(
        resp.content_range.as_deref(),
        Some(format!("bytes 16380-16390/{BODY_LEN}").as_str())
    );
    assert_eq!(resp.body_len, 11);
    let served = rig.drain(resp.stream);
    assert_eq!(served, &plain[16_380..16_391]);
}

#[test]
fn cached_single_byte_ranges_at_body_edges() {
    let plain = body();
    let mut rig = CachedRig::seeded("track-edges", plain.clone());

    let resp = rig
        .play_range("track-edges", Some("bytes=0-0"))
        .expect("first-byte range serves");
    assert_eq!(resp.body_len, 1);
    assert_eq!(rig.drain(resp.stream), &plain[0..1]);

    let last = BODY_LEN - 1;
    let header = format!("bytes={last}-");
    let resp = rig
        .play_range("track-edges", Some(&header))
        .expect("last-byte open range serves");
    assert_eq!(resp.body_len, 1);
    assert_eq!(rig.drain(resp.stream), &plain[last..]);
}

/// Minimal NNTP stub serving articles by message-id (mirrors
/// `covering_fallback_pins.rs`), so covering/backfill fetch order on the
/// live path does not matter.
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

/// `(plaintext, segment message-ids, segment ranges, stub server)`.
type LiveFixture = (Vec<u8>, Vec<String>, [(u64, u64); 2], Server);

/// Two-segment live fixture over `body()`; identity cipher (key 0).
fn live_fixture() -> LiveFixture {
    let plain = body();
    let crc = yenc::crc32(&plain);
    let total = plain.len().div_ceil(SEG_LEN) as u32;
    let mut articles = HashMap::new();
    let mut segments = Vec::new();
    for (i, chunk) in plain.chunks(SEG_LEN).enumerate() {
        let part = i as u32 + 1;
        let mid = format!("<slice-u64-seg{i}@synthetic.test>");
        articles.insert(
            mid.clone(),
            yenc::encode_part(
                chunk,
                "slice-u64.flac",
                PartSpec {
                    file_size: plain.len() as u64,
                    part,
                    total,
                    offset: (i * SEG_LEN) as u64,
                    file_crc: (part == total).then_some(crc),
                },
            ),
        );
        segments.push(mid);
    }
    let ranges = [(0u64, SEG_LEN as u64), (SEG_LEN as u64, BODY_LEN as u64)];
    (plain, segments, ranges, Server::start(articles))
}

fn live_play(
    server: &Server,
    segments: &[String],
    ranges: &[(u64, u64)],
    range: Option<&str>,
) -> (PlayResponse, CachedRig) {
    let mut rig = CachedRig {
        pool: Pool::new(4),
        cache: ContentCache::new(),
        meter: MemoryMeter::default(),
    };
    let token = grant::issue("user-1", "track-live", EXP_UNIX, SECRET);
    let input = PlayInput {
        trace_id: None,
        token: &token,
        track_id: "track-live",
        range,
        server_addr: server.addr,
        segments,
        segment_ranges: Some(ranges),
        cipher_key: 0,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let resp = play(
        &mut rig.pool,
        &mut rig.cache,
        &mut rig.meter,
        SECRET,
        &input,
    )
    .expect("live play must serve");
    (resp, rig)
}

#[test]
fn live_full_play_slices_across_segment_boundary() {
    let (plain, segments, ranges, server) = live_fixture();
    let (resp, mut rig) = live_play(&server, &segments, &ranges, None);
    assert_eq!(resp.status, 200);
    assert!(!resp.cache_hit);
    assert_eq!(resp.body_len, BODY_LEN as u64);
    // Serving crosses into segment 1, whose staged buffer starts at
    // `buf_start = SEG_LEN`: `pos - buf_start` must stay exact there.
    let served = rig.drain(resp.stream);
    assert_eq!(served, plain);
    assert_eq!(
        rig.cache
            .get_track("track-live")
            .map(|b| b.to_vec())
            .unwrap_or_default(),
        plain,
        "finalize stages the whole track into the cache"
    );
    assert!(server.bodies() >= 2, "both segments fetched");
}

#[test]
fn live_range_crossing_segment_boundary_serves_exact_bytes() {
    let (plain, segments, ranges, server) = live_fixture();
    // 19_995..=20_004 straddles the segment boundary at 20_000.
    let (resp, mut rig) = live_play(&server, &segments, &ranges, Some("bytes=19995-20004"));
    assert_eq!(resp.status, 206);
    assert_eq!(
        resp.content_range.as_deref(),
        Some(format!("bytes 19995-20004/{BODY_LEN}").as_str())
    );
    assert_eq!(resp.body_len, 10);
    let served = rig.drain(resp.stream);
    assert_eq!(served, &plain[19_995..20_005]);
    assert_eq!(
        rig.cache
            .get_track("track-live")
            .map(|b| b.to_vec())
            .unwrap_or_default(),
        plain,
        "finalize backfills and stages the whole track"
    );
}

#[test]
fn live_range_inside_second_segment_serves_exact_bytes() {
    let (plain, segments, ranges, server) = live_fixture();
    // Entirely inside segment 1 (`buf_start = 20_000`): the slice offset
    // is the in-buffer difference, not the absolute file offset.
    let (resp, mut rig) = live_play(&server, &segments, &ranges, Some("bytes=35000-35099"));
    assert_eq!(resp.status, 206);
    assert_eq!(resp.body_len, 100);
    let served = rig.drain(resp.stream);
    assert_eq!(served, &plain[35_000..35_100]);
}

// ---------------------------------------------------------------------------
// SizeMismatch reporting control: exact small values before and after the
// field widening (integer literals infer to either field type).
// ---------------------------------------------------------------------------

#[test]
fn size_mismatch_reports_exact_small_actual() {
    // `=ybegin` declares 4 bytes, the body decodes to exactly 3.
    let text = b"=ybegin line=128 size=4 name=a.bin\r\nXYZ\r\n=yend size=3\r\n";
    assert!(
        matches!(
            yenc::decode_part(text),
            Err(YencError::SizeMismatch {
                expected: 4,
                actual: 3
            })
        ),
        "a one-byte size lie must still report the exact actual byte count"
    );
}
