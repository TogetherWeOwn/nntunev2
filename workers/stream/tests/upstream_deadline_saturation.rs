//! Upstream shared-deadline `Duration` saturation: `Duration::MAX` budgets must
//! not panic.
//!
//! Pins both shared connect-plus-read deadline sites
//! (`workers/stream/src/nntp.rs:125` in `connect_with_timeouts` and
//! `workers/stream/src/play.rs:703` in `open_and_fetch`, both
//! `timeouts.connect.saturating_add(timeouts.read)`): the deadline must
//! saturate instead of overflowing, so `Duration::MAX` / `Duration::MAX`
//! dials fail on the refused port — never on deadline math — while a
//! normal-timeout control proves the harness can dial the same closed port.
//!
//! Failing command:
//! `cargo test --locked -p nntune-stream --test upstream_deadline_saturation`
//!
//! Sensitivity: replacing either `saturating_add` with plain `+` must fail
//! the corresponding max-duration test (debug overflow panic on
//! `Duration::MAX + Duration::MAX`).

use std::net::{SocketAddr, TcpListener};
use std::time::{Duration, Instant};

use nntune_stream::cache::ContentCache;
use nntune_stream::grant;
use nntune_stream::meter::MemoryMeter;
use nntune_stream::nntp::{NntpClient, NntpError, UpstreamTimeouts};
use nntune_stream::play::{play_with_timeouts, PlayError, PlayInput};
use nntune_stream::pool::Pool;

const SECRET: &[u8] = b"deadline-saturation-pin";
const NOW_UNIX: u64 = 1_700_000_000;
// Within the grant TTL cap (`MAX_GRANT_TTL` = 900 s) so the play-path case
// exercises the dial, not grant rejection.
const EXP_UNIX: u64 = NOW_UNIX + 900;

/// A port that was bound and released: dialing it fails fast with connection
/// refused, so reaching the network proves deadline math did not fail first.
fn closed_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind temporary port");
    let addr = listener.local_addr().expect("temporary address");
    drop(listener);
    addr
}

/// The refused dial surfaces as `NntpError::Io`, whose payload is the
/// `io::Error` display text only (the `ErrorKind` is not preserved). Require
/// the connection-refused text so the test proves the dial reached the
/// closed port instead of failing earlier on deadline math.
fn assert_refused_dial(err: &NntpError) {
    match err {
        NntpError::Io(detail) => {
            assert!(
                !detail.contains("deadline"),
                "saturated deadline must yield the dial refusal, not a deadline failure, got {detail:?}"
            );
            assert!(
                detail.to_lowercase().contains("refused"),
                "dial must reach the closed port (connection refused), got {detail:?}"
            );
        }
        other => panic!("closed-port dial must be an I/O refusal, got {other:?}"),
    }
}

/// `Duration::MAX + Duration::MAX` must saturate to `Duration::MAX`, not panic:
/// the dial still reaches the closed port and returns its refusal.
#[test]
fn max_durations_yield_dial_error_without_panic() {
    let addr = closed_address();
    let timeouts = UpstreamTimeouts::new(Duration::MAX, Duration::MAX);
    let started = Instant::now();
    let err = match NntpClient::connect_with_timeouts(&addr, timeouts) {
        Ok(_) => panic!("dialing a closed port must fail, not connect"),
        Err(err) => err,
    };
    let elapsed = started.elapsed();
    assert_refused_dial(&err);
    assert!(
        elapsed < Duration::from_secs(10),
        "a refused dial must fail fast instead of waiting out a saturated deadline, took {elapsed:?}"
    );
}

/// Control: the same closed-port harness with normal timeouts dials and fails
/// on the refusal, proving a failure above is about saturation and not the
/// harness or deadline math.
#[test]
fn normal_timeouts_control_still_dials_without_deadline_math_failure() {
    let addr = closed_address();
    let timeouts = UpstreamTimeouts::new(Duration::from_millis(200), Duration::from_millis(200));
    let started = Instant::now();
    let err = match NntpClient::connect_with_timeouts(&addr, timeouts) {
        Ok(_) => panic!("control dial of a closed port must fail, not connect"),
        Err(err) => err,
    };
    let elapsed = started.elapsed();
    assert_refused_dial(&err);
    assert!(
        elapsed < Duration::from_secs(5),
        "control dial must fail fast, took {elapsed:?}"
    );
}

/// The play path builds its own shared deadline (`play.rs`, `open_and_fetch`)
/// and never reaches `connect_with_timeouts`, so the `nntp.rs` pin alone does
/// not cover it: a saturated budget through `play_with_timeouts` must return
/// an upstream dial failure, never panic on deadline math.
#[test]
fn play_path_max_durations_yield_upstream_error_without_panic() {
    let addr = closed_address();
    let timeouts = UpstreamTimeouts::new(Duration::MAX, Duration::MAX);
    let secret = SECRET;
    let token = grant::issue("user-1", "track-saturated", EXP_UNIX, secret);
    let mut pool = Pool::new(1);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    // A valid segment id so the play reaches the dial (never rejected as an
    // invalid id before anything is sent).
    let segments = ["saturated-seg0@synthetic.test".to_string()];
    let input = PlayInput {
        trace_id: None,
        token: &token,
        track_id: "track-saturated",
        range: None,
        server_addr: addr,
        segments: &segments,
        segment_ranges: None,
        cipher_key: 0,
        now_unix: NOW_UNIX,
        up_next: None,
    };
    let started = Instant::now();
    let err = match play_with_timeouts(&mut pool, &mut cache, &mut meter, secret, &input, timeouts)
    {
        Ok(_) => panic!("play against a closed port must fail, not serve"),
        Err(err) => err,
    };
    let elapsed = started.elapsed();
    match &err {
        PlayError::Upstream(detail) => {
            assert!(
                !detail.contains("deadline"),
                "saturated play-path deadline must yield the dial refusal, not a deadline failure, got {detail:?}"
            );
            assert!(
                detail.to_lowercase().contains("refused"),
                "play-path dial must reach the closed port (connection refused), got {detail:?}"
            );
        }
        other => panic!(
            "saturated play-path budget must surface the dial failure as upstream, got {other:?}"
        ),
    };
    assert!(
        elapsed < Duration::from_secs(10),
        "a refused play-path dial must fail fast instead of waiting out a saturated deadline, took {elapsed:?}"
    );
    assert_eq!(pool.in_use(), 0, "failed play frees its slot");
}
