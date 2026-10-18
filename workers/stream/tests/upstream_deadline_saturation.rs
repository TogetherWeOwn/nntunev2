//! Upstream shared-deadline `Duration` saturation: `Duration::MAX` budgets must
//! not panic.
//!
//! Pins `connect_with_timeouts` (`workers/stream/src/nntp.rs:125`,
//! `timeouts.connect.saturating_add(timeouts.read)`): the shared
//! connect-plus-read deadline must saturate instead of overflowing, so
//! `Duration::MAX` / `Duration::MAX` dials (and fails on the refused port,
//! never on deadline math) while a normal-timeout control proves the harness
//! can dial the same closed port.
//!
//! Failing command:
//! `cargo test --locked -p nntune-stream --test upstream_deadline_saturation`
//!
//! Sensitivity: replacing `saturating_add` with plain `+` must fail
//! `max_durations_yield_dial_error_without_panic` (debug overflow panic on
//! `Duration::MAX + Duration::MAX`).

use std::net::{SocketAddr, TcpListener};
use std::time::{Duration, Instant};

use nntune_stream::nntp::{NntpClient, NntpError, UpstreamTimeouts};

/// A port that was bound and released: dialing it fails fast with connection
/// refused, so reaching the network proves deadline math did not fail first.
fn closed_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind temporary port");
    let addr = listener.local_addr().expect("temporary address");
    drop(listener);
    addr
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
    match &err {
        NntpError::Io(detail) => assert!(
            !detail.contains("deadline"),
            "saturated deadline must yield the dial refusal, not a deadline failure, got {detail:?}"
        ),
        other => panic!("closed-port dial must be an I/O refusal, got {other:?}"),
    }
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
    match &err {
        NntpError::Io(detail) => assert!(
            !detail.contains("deadline"),
            "control dial must yield the refusal, not a deadline failure, got {detail:?}"
        ),
        other => panic!("control dial must be an I/O refusal, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(5),
        "control dial must fail fast, took {elapsed:?}"
    );
}
