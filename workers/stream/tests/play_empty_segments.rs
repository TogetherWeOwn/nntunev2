//! Play empty-segments fail-closed: an empty `segments` list is rejected
//! before any pool lease or dial.
//!
//! Pins `play_authorized` (`workers/stream/src/play.rs:516`, `segments.is_empty`
//! → `Err(Upstream("track has no segments"))`): the guard sits after the
//! cache-miss meter event and before segment-id validation and
//! `pool.try_acquire`, so a metadata-less track fails closed with the pool
//! untouched (no lease held, no connection opened, no quarantine). The
//! single-segment control proves the harness can dial: the same wiring with
//! one segment plays, streams, and caches.
//!
//! Failing command:
//! `cargo test --locked -p nntune-stream --test play_empty_segments`
//!
//! Sensitivity: removing the guard must fail
//! `empty_segments_fails_closed_without_touching_pool` (the play dials the
//! closed address and returns a dial error instead of the `no segments`
//! diagnostic).

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::thread;

use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::{MemoryMeter, MeterKind};
use nntune_stream::play::{play, PlayError, PlayInput, StreamPool};
use nntune_stream::pool::Pool;
use nntune_stream::yenc::{self, PartSpec};

const SECRET: &[u8] = b"empty-segments-secret";
const NOW_UNIX: u64 = 1_700_000_000;
// Within the grant TTL cap (`MAX_GRANT_TTL` = 900 s).
const EXP_UNIX: u64 = NOW_UNIX + 900;

/// A port that was bound and released: dialing it fails, so reaching the
/// network proves the guard did not fire.
fn closed_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind temporary port");
    let addr = listener.local_addr().expect("temporary address");
    drop(listener);
    addr
}

/// Empty `segments` is rejected with the `no segments` diagnostic before any
/// pool lease or dial.
#[test]
fn empty_segments_fails_closed_without_touching_pool() {
    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let token = grant::issue("user-1", "track-empty", EXP_UNIX, SECRET);
    let empty: &[String] = &[];
    let input = PlayInput {
        trace_id: None,
        token: &token,
        track_id: "track-empty",
        range: None,
        server_addr: closed_address(),
        segments: empty,
        segment_ranges: None,
        cipher_key: 0,
        now_unix: NOW_UNIX,
        up_next: None,
    };

    let err = match play(&mut pool, &mut cache, &mut meter, SECRET, &input) {
        Ok(_) => panic!("empty segments must fail instead of playing"),
        Err(err) => err,
    };
    match &err {
        PlayError::Upstream(detail) => assert!(
            detail.contains("no segments"),
            "empty segments must name the no-segments diagnostic, got {detail:?}"
        ),
        other => panic!("empty segments must fail with the no-segments Upstream, got {other:?}"),
    }

    assert_eq!(pool.in_use(), 0, "no lease may be held");
    assert_eq!(pool.connects(), 0, "no connection may be opened");
    assert_eq!(pool.reuses(), 0, "no warm reuse may be counted");
    assert!(
        !pool.quarantined(),
        "a refused play is not a provider failure"
    );
    assert!(
        cache.get_track("track-empty").is_none(),
        "a refused play must not cache the track"
    );
    // Auth passed (PlayStart) and the miss path reached the guard (CacheMiss);
    // the failure is the guard itself, not an auth or pool rejection.
    assert!(meter.has_kind(MeterKind::PlayStart));
    assert!(meter.has_kind(MeterKind::CacheMiss));
}

/// Control: the same wiring with one segment plays end to end, proving the
/// empty case fails at the guard rather than in the harness.
#[test]
fn single_segment_control_plays() {
    let plain = b"empty-segments control payload".to_vec();
    let mid = "control-seg0@synthetic.test".to_string();
    let article = yenc::encode_part(
        &plain,
        "control.flac",
        PartSpec {
            file_size: plain.len() as u64,
            part: 1,
            total: 1,
            offset: 0,
            file_crc: Some(yenc::crc32(&plain)),
        },
    );
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub NNTP");
    let addr = listener.local_addr().expect("stub addr");
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("one play connection");
        let mut reader = BufReader::new(stream.try_clone().expect("clone stub"));
        let mut writer = stream;
        writer
            .write_all(b"200 stub NNTP ready\r\n")
            .expect("greeting");
        let mut line = String::new();
        reader.read_line(&mut line).expect("BODY line");
        assert!(line.starts_with("BODY"), "stub only serves BODY");
        writer
            .write_all(format!("222 0 <{mid}> follows\r\n").as_bytes())
            .expect("222");
        for raw_line in article.split(|&b| b == b'\n') {
            let text_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
            if text_line.first() == Some(&b'.') {
                writer.write_all(b".").expect("dot-stuff");
            }
            writer.write_all(text_line).expect("article line");
            writer.write_all(b"\r\n").expect("crlf");
        }
        writer.write_all(b".\r\n").expect("terminator");
    });

    let mut pool: StreamPool = Pool::new(4);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let token = grant::issue("user-1", "track-control", EXP_UNIX, SECRET);
    let segments = ["control-seg0@synthetic.test".to_string()];
    let input = PlayInput {
        trace_id: None,
        token: &token,
        track_id: "track-control",
        range: None,
        server_addr: addr,
        segments: &segments,
        segment_ranges: None,
        cipher_key: 0,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &input).expect("control must play");
    assert!(!resp.cache_hit);
    let mut body = Vec::new();
    let mut stream = resp.stream;
    for chunk in stream.by_ref() {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body, plain);
    stream
        .finalize(&mut pool, &mut cache, &mut meter)
        .expect("finalize");
    server.join().expect("stub served one article");
    assert_eq!(
        cache
            .get_track("track-control")
            .map(|b| b.to_vec())
            .unwrap_or_default(),
        plain,
        "control play must cache the track"
    );
}
