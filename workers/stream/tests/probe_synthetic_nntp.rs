//! Pool probe against the synthetic NNTP server (`nntune-test-nntp`): real
//! `STAT` over the shared `nntune-core` client, through the pool's ingest
//! lane, the way the `NntpPool` Durable Object runs it (with the native
//! transport standing in for the Workers socket).

use std::cell::{Cell, RefCell};
use std::net::SocketAddr;
use std::time::Duration;

use nntune_core::nntp::{NntpClient, NntpConfig};
use nntune_core::transports::{connect_native, NativeTlsTransport};
use nntune_stream::pool::{Lane, Pool};
use nntune_stream::probe::{
    run_probe, serve_probe, ProbeError, ProbeOutcome, ProbeRequest, LANE_FULL_RETRY_MS,
};
use nntune_test_nntp::{default_corpus, TestNntpServer};

type Client = NntpClient<NativeTlsTransport>;

const MISSING: &str = "missing-430@nntune.test";
const UNKNOWN: &str = "never-posted@nntune.test";

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

/// Two posted corpus ids (bracketless, as ingest sends them).
fn posted_ids() -> Vec<String> {
    default_corpus()
        .iter()
        .take(2)
        .map(|a| a.message_id.trim_matches(['<', '>']).to_string())
        .collect()
}

fn request(user: &str, batch_index: usize, ids: &[String]) -> ProbeRequest {
    ProbeRequest {
        job_id: "job_01".into(),
        user_id: user.into(),
        idempotency_key: "idem_01".into(),
        batch_index,
        segment_ids: ids.to_vec(),
    }
}

#[tokio::test]
async fn stat_partitions_present_and_missing_and_echoes_the_batch() {
    let server = TestNntpServer::spawn_for_test().await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));

    let posted = posted_ids();
    let mut ids = vec![posted[0].clone(), MISSING.to_string()];
    ids.push(posted[1].clone());
    ids.push(UNKNOWN.to_string());
    let outcome = run_probe(&pool, || dial(cfg.clone(), &dials), &request("u1", 7, &ids))
        .await
        .expect("probe succeeds");

    assert_eq!(
        outcome,
        ProbeOutcome {
            batch_index: 7,
            present: posted.clone(),
            missing: vec![MISSING.to_string(), UNKNOWN.to_string()],
        }
    );
    // A 430 is about one article, not the provider: no quarantine, and the
    // connection goes back warm.
    let pool = pool.borrow();
    assert!(!pool.quarantined());
    assert_eq!((pool.in_use(), pool.idle(), dials.get()), (0, 1, 1));
}

#[tokio::test]
async fn warm_connection_serves_the_next_batch_without_a_dial() {
    let server = TestNntpServer::spawn_for_test().await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));
    let ids = posted_ids();

    for batch in 0..3 {
        run_probe(
            &pool,
            || dial(cfg.clone(), &dials),
            &request("u1", batch, &ids),
        )
        .await
        .expect("probe succeeds");
    }
    assert_eq!(dials.get(), 1);
    let pool = pool.borrow();
    assert_eq!((pool.connects(), pool.reuses()), (1, 2));
}

#[tokio::test]
async fn warm_connection_closed_while_idle_is_redialed_once() {
    let first = TestNntpServer::spawn_for_test().await;
    let target = RefCell::new(config(first.addr(), "testpass"));
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));
    let ids = posted_ids();
    let dial_target = || dial(target.borrow().clone(), &dials);

    run_probe(&pool, dial_target, &request("u1", 0, &ids))
        .await
        .expect("first probe");
    assert_eq!(pool.borrow().idle(), 1);

    // The provider drops the parked connection; the next dial reaches a
    // fresh server.
    drop(first);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = TestNntpServer::spawn_for_test().await;
    *target.borrow_mut() = config(second.addr(), "testpass");

    let outcome = run_probe(&pool, dial_target, &request("u1", 1, &ids))
        .await
        .expect("stale warm connection is replaced, not reported");
    assert_eq!(outcome.present, ids);
    assert_eq!(dials.get(), 2);
    assert!(!pool.borrow().quarantined());
}

#[tokio::test]
async fn provider_failure_is_generic_and_quarantines_the_pool() {
    let server = TestNntpServer::spawn_for_test().await;
    let cfg = config(server.addr(), "wrong-password");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));
    let req = request("u1", 0, &posted_ids());

    let err = run_probe(&pool, || dial(cfg.clone(), &dials), &req)
        .await
        .expect_err("bad credentials fail the batch");
    assert_eq!(err, ProbeError::Provider);
    assert_eq!(pool.borrow().in_use(), 0, "the lease is returned");
    assert_eq!(pool.borrow().idle(), 0, "the failed connection is dropped");

    // Quarantined: the next batch backs off without touching the provider.
    let err = run_probe(&pool, || dial(cfg.clone(), &dials), &req)
        .await
        .expect_err("quarantined");
    assert_eq!(
        err,
        ProbeError::Busy {
            retry_after_ms: 1_000
        }
    );
    assert_eq!(dials.get(), 1);
}

#[tokio::test]
async fn ingest_lane_never_takes_the_playback_slot() {
    let server = TestNntpServer::spawn_for_test().await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let req = request("u1", 0, &posted_ids());

    // Cap 1: the only connection is playback's.
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(1));
    let err = run_probe(&pool, || dial(cfg.clone(), &dials), &req)
        .await
        .expect_err("lane full");
    assert_eq!(
        err,
        ProbeError::Busy {
            retry_after_ms: LANE_FULL_RETRY_MS
        }
    );

    // Cap 2 with a play in progress: the free slot is reserved for playback.
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(2));
    let play = pool.borrow_mut().try_acquire(Lane::Playback).expect("play");
    assert!(matches!(
        run_probe(&pool, || dial(cfg.clone(), &dials), &req).await,
        Err(ProbeError::Busy { .. })
    ));
    pool.borrow_mut().release(play, None);
    run_probe(&pool, || dial(cfg.clone(), &dials), &req)
        .await
        .expect("lane free once the play ends");
    assert_eq!(dials.get(), 1, "refused probes never dial");
}

#[tokio::test]
async fn concurrent_probes_share_the_pool_without_holding_a_borrow() {
    // The Durable Object interleaves requests at every await; a RefCell
    // borrow held across one would panic here.
    let server = TestNntpServer::spawn_for_test().await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));
    let ids = posted_ids();
    let (a, b) = (request("u1", 0, &ids), request("u1", 1, &ids));

    let (ra, rb) = tokio::join!(
        run_probe(&pool, || dial(cfg.clone(), &dials), &a),
        run_probe(&pool, || dial(cfg.clone(), &dials), &b),
    );
    assert_eq!(ra.expect("a").batch_index, 0);
    assert_eq!(rb.expect("b").batch_index, 1);
    let pool = pool.borrow();
    assert_eq!((pool.in_use(), pool.idle(), dials.get()), (0, 2, 2));
}

#[tokio::test]
async fn serve_probe_answers_the_ingest_envelope_as_json() {
    let server = TestNntpServer::spawn_for_test().await;
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let pool: RefCell<Pool<Client>> = RefCell::new(Pool::new(4));
    let posted = posted_ids();

    // Exactly what ingest serializes, including fields this side ignores.
    let body = serde_json::json!({
        "job_id": "job_01",
        "user_id": "u1",
        "idempotency_key": "idem_01",
        "batch_index": 4,
        "segment_ids": [posted[0], MISSING],
        "segment_bytes": [739, null],
    })
    .to_string();
    let reply = serve_probe(&pool, "u1", body.as_bytes(), || dial(cfg.clone(), &dials)).await;
    assert_eq!(reply.status, 200);
    let outcome: ProbeOutcome = serde_json::from_str(&reply.body).expect("outcome JSON");
    assert_eq!(outcome.batch_index, 4);
    assert_eq!(outcome.present, vec![posted[0].clone()]);
    assert_eq!(outcome.missing, vec![MISSING.to_string()]);

    // Routed to another user's pool: refused before any dial.
    let reply = serve_probe(&pool, "u2", body.as_bytes(), || dial(cfg.clone(), &dials)).await;
    assert_eq!(reply.status, 400);
    assert_eq!(reply.body, r#"{"error":"bad_request"}"#);
    assert_eq!(dials.get(), 1);
}
