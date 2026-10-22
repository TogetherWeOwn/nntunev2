//! Article-side cap for the pool BODY route
//! ([`MAX_BODY_ARTICLE_BYTES`]): an article whose body is one byte over the
//! 16 MiB reply cap is refused with [`ProbeError::TooLarge`] (`413` at the
//! serve layer), without quarantining the pool and without dropping the
//! healthy connection. The request-side (envelope) caps are pinned in
//! `body_request_caps.rs`; the client-side read-limit arm
//! (`TooLargeUnusable`, desynced connection dropped) is pinned in
//! `body_synthetic_nntp.rs`. This suite pins the reply-cap arm, including
//! the exact boundary values, so the cap is neither over- nor under-broad.
//!
//! Sensitivity: the refusal is one byte wide. Exactly `MAX` body bytes are
//! still served (`>` refuses, `==` passes); `MAX + 1` refuses. The client's
//! own dot-block limit defaults to 64 MiB, so it does not fire first — the
//! refusal below is the route's reply cap, not the transport limit. The
//! oversized bodies below are CRLF-framed in 1 KiB lines: the transport
//! rejects any single line over 16 KiB and counts appended CRLFs, so one
//! giant line would trip a different arm.

use std::cell::{Cell, RefCell};
use std::net::SocketAddr;
use std::time::Duration;

use nntune_core::nntp::{NntpClient, NntpConfig};
use nntune_core::transports::{connect_native, NativeTlsTransport};
use nntune_stream::body::{run_body, serve_body, BodyOutcome, BodyRequest, MAX_BODY_ARTICLE_BYTES};
use nntune_stream::pool::Pool;
use nntune_stream::probe::ProbeError;
use nntune_test_nntp::{Article, ServerConfig, TestNntpServer};

type Client = NntpClient<NativeTlsTransport>;

const BIG_ID: &str = "big-article-16mb-plus-one@nntune.test";
const EXACT_ID: &str = "exact-article-16mb@nntune.test";

fn config(addr: SocketAddr, password: &str) -> NntpConfig {
    NntpConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        tls: false,
        username: Some("testuser".into()),
        password: Some(password.into()),
        timeout: Duration::from_secs(30),
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

fn request(user: &str, segment_id: &str) -> BodyRequest {
    BodyRequest {
        job_id: "job_01".into(),
        user_id: user.into(),
        idempotency_key: "idem_01".into(),
        segment_id: segment_id.to_string(),
    }
}

fn no_tick(_: &mut Pool<Client>) {}

/// One article whose served body is exactly `body_len` bytes: CRLF-framed
/// lines of 1 KiB `A`s (plus a shorter last line), so the wire framing
/// round-trips byte-for-byte through dot-stuffing (each line gains no
/// bytes) and the client's dot-block reader (each line contributes its
/// bytes plus the CRLF it appends).
fn sized_article(number: u32, message_id: &str, body_len: usize) -> Article {
    const LINE: usize = 1024;
    let mut body = Vec::with_capacity(body_len);
    while body.len() + LINE + 2 <= body_len {
        body.extend_from_slice(&vec![b'A'; LINE]);
        body.extend_from_slice(b"\r\n");
    }
    let rest = body_len - body.len();
    assert!(
        rest >= 2,
        "body_len must fit at least one CRLF-terminated line"
    );
    body.extend_from_slice(&vec![b'A'; rest - 2]);
    body.extend_from_slice(b"\r\n");
    assert_eq!(body.len(), body_len);
    let mut raw = format!(
        "From: fixture@nntune.test\r\nNewsgroups: alt.binaries.nntune.test\r\nSubject: NNTune-Fixture big [01/01]\r\nDate: Mon, 13 Oct 2026 00:00:00 +0000\r\nMessage-ID: <{message_id}>\r\nLines: 8\r\n\r\n"
    )
    .into_bytes();
    raw.extend_from_slice(&body);
    Article {
        message_id: format!("<{message_id}>"),
        number,
        group: "alt.binaries.nntune.test".to_string(),
        subject: "NNTune-Fixture big [01/01]".to_string(),
        from: "fixture@nntune.test".to_string(),
        date: "Mon, 13 Oct 2026 00:00:00 +0000".to_string(),
        raw,
        payload: body,
    }
}

async fn server_with(body_len: usize, message_id: &str, number: u32) -> TestNntpServer {
    let mut cfg = ServerConfig::for_test();
    cfg.articles
        .push(sized_article(number, message_id, body_len));
    TestNntpServer::spawn(cfg).await
}

#[test]
fn article_cap_literal_is_16_mib() {
    assert_eq!(
        MAX_BODY_ARTICLE_BYTES, 16_777_216,
        "reply cap must stay exactly 16 MiB (16 << 20)"
    );
    assert_eq!(MAX_BODY_ARTICLE_BYTES, 16 << 20);
}

#[tokio::test]
async fn article_one_byte_over_cap_is_refused_fail_closed() {
    let server = server_with(MAX_BODY_ARTICLE_BYTES + 1, BIG_ID, 900).await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));

    let err = run_body(
        &pool,
        || dial(cfg.clone(), &dials),
        no_tick,
        &request("user_01", BIG_ID),
    )
    .await
    .expect_err("article over the reply cap must refuse");
    assert_eq!(err, ProbeError::TooLarge);
    assert_eq!(err.status(), 413);
    assert_eq!(dials.get(), 1, "one dial, no provider-failure redial");

    // Fail-closed without pool damage: not quarantined, and the healthy
    // in-sync connection is retained for reuse.
    {
        let pool = pool.borrow();
        assert!(
            !pool.quarantined(),
            "an over-cap refusal must not quarantine"
        );
        assert_eq!(pool.in_use(), 0, "no lease may be held");
        assert_eq!(pool.idle(), 1, "the healthy connection must be retained");
    }

    // The retained connection serves the next segment: no fresh dial.
    let small = &nntune_test_nntp::default_corpus()[0];
    let small_id = small.message_id.trim_matches(['<', '>']);
    let outcome = run_body(
        &pool,
        || dial(cfg.clone(), &dials),
        no_tick,
        &request("user_01", small_id),
    )
    .await
    .expect("pool must still serve after an over-cap refusal");
    assert!(matches!(outcome, BodyOutcome::Bytes(_)));
    assert_eq!(dials.get(), 1, "retained connection must be reused");
    assert!(pool.borrow().reuses() >= 1);
}

#[tokio::test]
async fn article_over_cap_on_warm_connection_refused_without_quarantine() {
    // Pins the warm-lease `Err(FetchFail::TooLarge)` arm (body.rs warm
    // path): an over-cap article fetched on an already-pooled connection
    // must refuse with `TooLarge` while keeping the pool healthy and the
    // in-sync connection retained. A regression that quarantines the pool
    // or drops the connection here would 503 every later segment.
    // (`server_with` seeds the default corpus plus the big article, so the
    // small fetch below warms the pool and the big fetch hits the warm arm.)
    let server = server_with(MAX_BODY_ARTICLE_BYTES + 1, BIG_ID, 900).await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));

    // Warm the pool with a small corpus article: one dial, one idle conn.
    let small = &nntune_test_nntp::default_corpus()[0];
    let small_id = small.message_id.trim_matches(['<', '>']);
    run_body(
        &pool,
        || dial(cfg.clone(), &dials),
        no_tick,
        &request("user_01", small_id),
    )
    .await
    .expect("warm-up fetch must serve");
    assert_eq!(dials.get(), 1, "warm-up must dial once");
    assert_eq!(pool.borrow().idle(), 1, "warm-up must leave one idle conn");

    // Over-cap fetch on the warm pooled connection: no fresh dial.
    let err = run_body(
        &pool,
        || dial(cfg.clone(), &dials),
        no_tick,
        &request("user_01", BIG_ID),
    )
    .await
    .expect_err("warm over-cap article must refuse");
    assert_eq!(err, ProbeError::TooLarge);
    assert_eq!(dials.get(), 1, "warm refusal must not redial");
    {
        let pool = pool.borrow();
        assert!(
            !pool.quarantined(),
            "a warm over-cap refusal must not quarantine"
        );
        assert_eq!(pool.in_use(), 0, "no lease may be held");
        assert_eq!(
            pool.idle(),
            1,
            "the warm in-sync connection must be retained"
        );
    }

    // The retained connection still serves: no fresh dial.
    let outcome = run_body(
        &pool,
        || dial(cfg.clone(), &dials),
        no_tick,
        &request("user_01", small_id),
    )
    .await
    .expect("pool must still serve after a warm over-cap refusal");
    assert!(matches!(outcome, BodyOutcome::Bytes(_)));
    assert_eq!(dials.get(), 1, "retained connection must be reused");
    assert!(pool.borrow().reuses() >= 2);
}

#[tokio::test]
async fn serve_layer_maps_over_cap_article_to_413() {
    let server = server_with(MAX_BODY_ARTICLE_BYTES + 1, BIG_ID, 900).await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));

    let envelope = serde_json::to_vec(&request("user_01", BIG_ID)).expect("envelope serializes");
    let reply = serve_body(
        &pool,
        "user_01",
        &envelope,
        || dial(cfg.clone(), &dials),
        no_tick,
    )
    .await;
    assert_eq!(reply.status, 413);
    assert_eq!(reply.body, br#"{"error":"too_large"}"#.to_vec());
    {
        let pool = pool.borrow();
        assert!(!pool.quarantined(), "a 413 refusal must not quarantine");
        assert_eq!(pool.idle(), 1, "the healthy connection must be retained");
    }
}

#[tokio::test]
async fn article_at_exactly_cap_still_serves() {
    // Sensitivity boundary: `>` refuses, `==` passes.
    let server = server_with(MAX_BODY_ARTICLE_BYTES, EXACT_ID, 901).await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));

    let outcome = run_body(
        &pool,
        || dial(cfg.clone(), &dials),
        no_tick,
        &request("user_01", EXACT_ID),
    )
    .await
    .expect("article at exactly the cap must serve");
    match outcome {
        BodyOutcome::Bytes(bytes) => assert_eq!(bytes.len(), MAX_BODY_ARTICLE_BYTES),
        BodyOutcome::Missing => panic!("exact-cap article reported missing"),
    }
}
