//! Request-side caps for the pool BODY route ([`parse_body_request`]): the
//! body-request envelope is capped at 64 KiB ([`MAX_BODY_REQUEST_BYTES`])
//! and `user_id` at 128 bytes ([`MAX_USER_ID_LEN`], the ingest rule). Both
//! are refused before any pool lease or dial, the same way the probe envelope
//! is: an oversized envelope answers `413`, an empty or overlong `user_id`
//! answers `400`. The article-side `413` is pinned elsewhere; this suite pins
//! the request-side arms, including the exact boundary values, so the caps
//! are neither over- nor under-broad.

use std::cell::{Cell, RefCell};
use std::net::SocketAddr;
use std::time::Duration;

use nntune_core::nntp::{NntpClient, NntpConfig};
use nntune_core::transports::{connect_native, NativeTlsTransport};
use nntune_stream::body::{parse_body_request, serve_body, BodyRequest, MAX_BODY_REQUEST_BYTES};
use nntune_stream::pool::Pool;
use nntune_stream::probe::{ProbeError, MAX_USER_ID_LEN};
use nntune_test_nntp::TestNntpServer;

type Client = NntpClient<NativeTlsTransport>;

const SEGMENT: &str = "seg-one@synthetic.invalid";

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

async fn dial(config: NntpConfig, dials: &Cell<u32>) -> nntune_core::Result<Client> {
    dials.set(dials.get() + 1);
    let transport = connect_native(&config)
        .await
        .map_err(nntune_core::Error::Io)?;
    NntpClient::connect(transport, &config).await
}

fn no_tick(_: &mut Pool<Client>) {}

/// One valid body-request envelope padded out to exactly `total` bytes.
/// `job_id` carries no validation, and `j` serializes to one byte unescaped,
/// so the filler maps 1:1 onto the wire length.
fn envelope_sized(user_id: &str, total: usize) -> Vec<u8> {
    let base = serde_json::to_vec(&BodyRequest {
        job_id: String::new(),
        user_id: user_id.into(),
        idempotency_key: "idem_01".into(),
        segment_id: SEGMENT.into(),
    })
    .expect("base envelope serializes");
    assert!(
        base.len() <= total,
        "base envelope ({} bytes) already over the {total}-byte target",
        base.len()
    );
    let out = serde_json::to_vec(&BodyRequest {
        job_id: "j".repeat(total - base.len()),
        user_id: user_id.into(),
        idempotency_key: "idem_01".into(),
        segment_id: SEGMENT.into(),
    })
    .expect("padded envelope serializes");
    assert_eq!(out.len(), total, "padded envelope must hit its target");
    out
}

#[test]
fn envelope_one_byte_over_the_cap_is_too_large() {
    let over = envelope_sized("user_01", MAX_BODY_REQUEST_BYTES + 1);
    // Valid JSON: the refusal is the envelope cap, not a parse failure.
    assert!(serde_json::from_slice::<serde_json::Value>(&over).is_ok());
    assert_eq!(
        parse_body_request(&over, "user_01"),
        Err(ProbeError::TooLarge)
    );
}

#[tokio::test]
async fn oversized_envelope_is_refused_before_any_lease_or_dial() {
    let server = TestNntpServer::spawn_for_test().await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));

    let over = envelope_sized("user_01", MAX_BODY_REQUEST_BYTES + 1);
    let reply = serve_body(
        &pool,
        "user_01",
        &over,
        || dial(cfg.clone(), &dials),
        no_tick,
    )
    .await;
    assert_eq!(reply.status, 413);
    assert_eq!(reply.body, br#"{"error":"too_large"}"#.to_vec());
    assert_eq!(dials.get(), 0, "oversized envelope must not dial");
    let pool = pool.borrow();
    assert_eq!(pool.in_use(), 0, "no lease may be held");
    assert_eq!(pool.connects(), 0, "no connection may be opened");
    assert!(
        !pool.quarantined(),
        "a refused envelope is not a provider failure"
    );
}

#[test]
fn empty_and_overlong_user_ids_are_bad_request() {
    let empty = serde_json::to_vec(&BodyRequest {
        job_id: "job_01".into(),
        user_id: String::new(),
        idempotency_key: "idem_01".into(),
        segment_id: SEGMENT.into(),
    })
    .expect("empty user_id envelope serializes");
    assert_eq!(
        parse_body_request(&empty, "user_01"),
        Err(ProbeError::BadRequest("invalid user_id"))
    );

    // The pool name matches, so only the length arm can fire.
    let long = "u".repeat(MAX_USER_ID_LEN + 1);
    assert_eq!(long.len(), 129);
    let overlong = serde_json::to_vec(&BodyRequest {
        job_id: "job_01".into(),
        user_id: long.clone(),
        idempotency_key: "idem_01".into(),
        segment_id: SEGMENT.into(),
    })
    .expect("overlong user_id envelope serializes");
    assert_eq!(
        parse_body_request(&overlong, &long),
        Err(ProbeError::BadRequest("invalid user_id"))
    );
}

#[test]
fn exact_cap_envelope_and_max_user_id_parse_ok() {
    // Exactly 64 KiB is within the cap (`>` rejects, `==` parses).
    let exact = envelope_sized("user_01", MAX_BODY_REQUEST_BYTES);
    let req = parse_body_request(&exact, "user_01").expect("exact-cap envelope parses");
    assert_eq!(req.user_id, "user_01");
    assert_eq!(req.segment_id, SEGMENT);

    // Exactly 128 chars is within the user_id cap.
    let max_user = "u".repeat(MAX_USER_ID_LEN);
    let at_max = serde_json::to_vec(&BodyRequest {
        job_id: "job_01".into(),
        user_id: max_user.clone(),
        idempotency_key: "idem_01".into(),
        segment_id: SEGMENT.into(),
    })
    .expect("max user_id envelope serializes");
    let req = parse_body_request(&at_max, &max_user).expect("128-char user_id parses");
    assert_eq!(req.user_id, max_user);
}
