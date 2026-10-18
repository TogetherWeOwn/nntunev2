//! Probe bound literal pins (tests-only).
//!
//! Contract under test (`workers/stream/src/probe.rs:46,50,57`):
//!
//! - `MAX_PROBE_SEGMENTS == 500` (the ingest batch size);
//! - `MAX_PROBE_BODY_BYTES == 1_048_576` (1 MiB probe body envelope cap);
//! - `LANE_FULL_RETRY_MS == 5_000` (ingest-lane-full `Retry-After` hint).
//!
//! Boundaries pinned here (synthetic NNTP helpers follow
//! `tests/probe_synthetic_nntp.rs`):
//!
//! - a body of `MAX_PROBE_BODY_BYTES + 1` bytes is refused with
//!   [`ProbeError::TooLarge`], while a body of exactly
//!   `MAX_PROBE_BODY_BYTES` bytes passes the size gate (fails later as
//!   `BadRequest`, proving the gate is `>` not `>=`);
//! - a request of `MAX_PROBE_SEGMENTS + 1` ids is refused with
//!   [`ProbeError::BadRequest`], while exactly `MAX_PROBE_SEGMENTS` ids parse;
//! - an ingest probe on an exhausted lane (cap 1, the only connection is
//!   playback's) is refused with `Busy { retry_after_ms: 5_000 }` without
//!   dialing the provider.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - lowering/raising `MAX_PROBE_SEGMENTS` breaks the exact boundary pair
//!   (`500` ok / `501` refused) or the literal pin;
//! - lowering `MAX_PROBE_BODY_BYTES` turns the exactly-`MAX` body from
//!   `BadRequest` into `TooLarge`; raising it turns the `MAX + 1` body from
//!   `TooLarge` into `BadRequest` (or Ok-parse); either breaks the pin;
//! - changing `LANE_FULL_RETRY_MS` breaks the literal pin and the exhausted
//!   lane's exact `Busy { retry_after_ms: 5_000 }`.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-stream --test probe_bound_literals_acceptance
//! ```

use std::cell::{Cell, RefCell};
use std::net::SocketAddr;
use std::time::Duration;

use nntune_core::nntp::{NntpClient, NntpConfig};
use nntune_core::transports::{connect_native, NativeTlsTransport};
use nntune_stream::pool::Pool;
use nntune_stream::probe::{
    parse_request, run_probe, ProbeError, ProbeRequest, LANE_FULL_RETRY_MS, MAX_PROBE_BODY_BYTES,
    MAX_PROBE_SEGMENTS,
};
use nntune_test_nntp::TestNntpServer;

type Client = NntpClient<NativeTlsTransport>;

/// Plain-TCP config for the synthetic server at `addr` with the given password.
fn config(addr: SocketAddr, password: &str) -> NntpConfig {
    NntpConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        tls: false,
        username: Some("testuser".into()),
        password: Some(password.into()),
        timeout: Duration::from_secs(5),
        ..NntpConfig::default()
    }
}

/// Dials and authenticates a client, counting the attempt in `dials`.
async fn dial(config: NntpConfig, dials: &Cell<u32>) -> nntune_core::Result<Client> {
    dials.set(dials.get() + 1);
    let transport = connect_native(&config)
        .await
        .map_err(nntune_core::Error::Io)?;
    NntpClient::connect(transport, &config).await
}

/// A probe envelope for `user`'s pool carrying `ids`.
fn request(user: &str, ids: &[String]) -> ProbeRequest {
    ProbeRequest {
        job_id: "job_01".into(),
        user_id: user.into(),
        idempotency_key: "idem_01".into(),
        batch_index: 0,
        segment_ids: ids.to_vec(),
    }
}

/// The pool's virtual clock stays where it is; these tests do not move time.
fn no_tick(_: &mut Pool<Client>) {}

/// An ingest-shaped envelope body for `user` carrying `ids`.
fn body(user: &str, ids: &[String]) -> Vec<u8> {
    serde_json::json!({
        "job_id": "job_01",
        "user_id": user,
        "idempotency_key": "idem_01",
        "batch_index": 0,
        "segment_ids": ids,
    })
    .to_string()
    .into_bytes()
}

#[test]
fn probe_bound_literals_are_pinned() {
    assert_eq!(MAX_PROBE_SEGMENTS, 500, "probe batch cap must stay 500 ids");
    assert_eq!(
        MAX_PROBE_BODY_BYTES, 1_048_576,
        "probe body cap must stay exactly 1 MiB"
    );
    assert_eq!(
        LANE_FULL_RETRY_MS, 5_000,
        "ingest lane-full retry hint must stay exactly 5s"
    );
}

#[test]
fn probe_body_one_byte_over_cap_is_refused() {
    // Exactly at the cap passes the size gate: an all-spaces body is not
    // valid JSON, so it must fail as BadRequest, not TooLarge. This proves
    // the gate is `len > MAX`, not `len >= MAX`.
    let at_cap = vec![b' '; MAX_PROBE_BODY_BYTES];
    assert_eq!(at_cap.len(), 1_048_576);
    assert!(matches!(
        parse_request(&at_cap, "user_01"),
        Err(ProbeError::BadRequest(_))
    ));

    // One byte over is refused before any JSON parsing.
    let over_cap = vec![b' '; MAX_PROBE_BODY_BYTES + 1];
    assert_eq!(
        parse_request(&over_cap, "user_01"),
        Err(ProbeError::TooLarge)
    );
}

#[test]
fn probe_segment_count_one_over_cap_is_refused() {
    let id = "a@b.invalid".to_string();
    let at_cap = vec![id.clone(); MAX_PROBE_SEGMENTS];
    assert!(parse_request(&body("user_01", &at_cap), "user_01").is_ok());

    let over_cap = vec![id; MAX_PROBE_SEGMENTS + 1];
    assert!(matches!(
        parse_request(&body("user_01", &over_cap), "user_01"),
        Err(ProbeError::BadRequest(_))
    ));
}

#[tokio::test]
async fn lane_full_retry_is_five_seconds_without_dialing() {
    assert_eq!(LANE_FULL_RETRY_MS, 5_000);
    let server = TestNntpServer::spawn_for_test().await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);

    // Cap 1: the only connection is playback's, so the ingest probe is
    // refused with the 5-second hint before any dial.
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(1));
    let req = request("u1", &["a@b.invalid".to_string()]);
    let err = run_probe(&pool, || dial(cfg.clone(), &dials), no_tick, &req)
        .await
        .expect_err("lane full");
    assert_eq!(
        err,
        ProbeError::Busy {
            retry_after_ms: 5_000
        }
    );
    assert_eq!(dials.get(), 0, "refused probes never dial");
}
