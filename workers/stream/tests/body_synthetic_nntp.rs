//! Pool BODY fetch against the synthetic NNTP server (`nntune-test-nntp`):
//! real `BODY` over the shared `nntune-core` client, through the pool's
//! ingest lane, the way the `NntpPool` Durable Object runs it (with the
//! native transport standing in for the Workers socket).

use std::cell::{Cell, RefCell};
use std::net::SocketAddr;
use std::time::Duration;

use nntune_core::nntp::{NntpClient, NntpConfig};
use nntune_core::transports::{connect_native, NativeTlsTransport};
use nntune_stream::body::{parse_body_request, run_body, BodyOutcome, BodyRequest};
use nntune_stream::pool::Pool;
use nntune_stream::probe::ProbeError;
use nntune_test_nntp::{default_corpus, TestNntpServer};

type Client = NntpClient<NativeTlsTransport>;

const MISSING: &str = "missing-430@nntune.test";

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

fn request(user: &str, segment_id: &str) -> BodyRequest {
    BodyRequest {
        job_id: "job_01".into(),
        user_id: user.into(),
        idempotency_key: "idem_01".into(),
        segment_id: segment_id.to_string(),
    }
}

fn no_tick(_: &mut Pool<Client>) {}

/// Header/body split offset (mirrors the server's BODY behaviour).
fn body_of(raw: &[u8]) -> &[u8] {
    raw.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| &raw[i + 4..])
        .unwrap_or(raw)
}

#[tokio::test]
async fn body_returns_the_article_body_bytes() {
    let server = TestNntpServer::spawn_for_test().await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));

    let article = &default_corpus()[0];
    let id = article.message_id.trim_matches(['<', '>']);
    let outcome = run_body(
        &pool,
        || dial(cfg.clone(), &dials),
        no_tick,
        &request("user_01", id),
    )
    .await
    .expect("posted article must fetch");
    match outcome {
        BodyOutcome::Bytes(bytes) => assert_eq!(bytes, body_of(&article.raw)),
        BodyOutcome::Missing => panic!("posted article reported missing"),
    }
    assert_eq!(dials.get(), 1, "one dial for the cold pool");

    // The warm connection is reused for the next segment.
    let article2 = &default_corpus()[1];
    let id2 = article2.message_id.trim_matches(['<', '>']);
    let outcome = run_body(
        &pool,
        || dial(cfg.clone(), &dials),
        no_tick,
        &request("user_01", id2),
    )
    .await
    .expect("posted article must fetch");
    assert!(matches!(outcome, BodyOutcome::Bytes(_)));
    assert_eq!(dials.get(), 1, "warm connection must be reused");
}

#[tokio::test]
async fn body_reports_a_missing_article_without_failing_the_pool() {
    let server = TestNntpServer::spawn_for_test().await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));

    let outcome = run_body(
        &pool,
        || dial(cfg.clone(), &dials),
        no_tick,
        &request("user_01", MISSING),
    )
    .await
    .expect("430 is an outcome, not an error");
    assert_eq!(outcome, BodyOutcome::Missing);

    // The pool is not quarantined by a 430: a posted id still fetches.
    let article = &default_corpus()[0];
    let id = article.message_id.trim_matches(['<', '>']);
    let outcome = run_body(
        &pool,
        || dial(cfg.clone(), &dials),
        no_tick,
        &request("user_01", id),
    )
    .await
    .expect("pool must still serve after a 430");
    assert!(matches!(outcome, BodyOutcome::Bytes(_)));
    assert_eq!(dials.get(), 1, "430 must not drop the connection either");
}

#[tokio::test]
async fn body_over_the_client_read_limit_is_too_large_not_a_provider_failure() {
    // The NNTP client enforces its own article limit while reading the
    // dot-block; when it binds below the route's reply cap, the refusal
    // must surface as a request-specific TooLarge (`413`), never as a
    // provider failure that quarantines the user's pool and makes
    // ingest retry the same article on every delivery.
    let server = TestNntpServer::spawn_for_test().await;
    let mut tight = config(server.addr(), "testpass");
    tight.max_article_bytes = 16;
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));

    let article = &default_corpus()[0];
    let id = article.message_id.trim_matches(['<', '>']);
    let err = run_body(
        &pool,
        || dial(tight.clone(), &dials),
        no_tick,
        &request("user_01", id),
    )
    .await
    .expect_err("article over the client limit must refuse");
    assert_eq!(err, ProbeError::TooLarge);
    assert_eq!(dials.get(), 1, "one dial, no provider-failure redial");

    // The pool is not quarantined: a properly configured client still
    // fetches. The desynced connection was dropped (not pooled), so
    // this takes a fresh dial.
    let cfg = config(server.addr(), "testpass");
    let outcome = run_body(
        &pool,
        || dial(cfg.clone(), &dials),
        no_tick,
        &request("user_01", id),
    )
    .await
    .expect("pool must still serve after an over-limit refusal");
    assert!(matches!(outcome, BodyOutcome::Bytes(_)));
    assert_eq!(dials.get(), 2, "dropped connection is redialed once");
}

#[test]
fn body_request_validation_rejects_bad_envelopes() {
    let ok = serde_json::to_vec(&request("user_01", "seg-one@synthetic.invalid")).unwrap();
    assert!(parse_body_request(&ok, "user_01").is_ok());

    // Another user's pool: refused before anything is dialed.
    assert_eq!(
        parse_body_request(&ok, "user_02"),
        Err(ProbeError::BadRequest("request is for another user's pool"))
    );

    // CR/LF smuggled inside the id would interpolate into `BODY <id>` on the
    // user's authenticated connection: refused by the strict msg-id rule.
    let injecting = serde_json::to_vec(&request("user_01", "x@y>\r\nQUIT\r\n<z")).unwrap();
    assert_eq!(
        parse_body_request(&injecting, "user_01"),
        Err(ProbeError::BadRequest(
            "segment id must be a valid message-id"
        ))
    );

    assert!(parse_body_request(b"not json", "user_01").is_err());
}
