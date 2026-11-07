//! ADR 0014 account budgets exercised through the real async NNTP client.
//! No credentials or sockets outside the synthetic server are used here.

use std::cell::Cell;
use std::net::SocketAddr;
use std::time::Duration;

use nntune_core::nntp::{NntpClient, NntpConfig};
use nntune_core::transports::{connect_native, NativeTlsTransport};
use nntune_stream::account_pool::{
    ranked_candidates, with_session_deadline, AccountConfig, AccountPool, Candidate, ConfigError,
    WorkClass,
};
use nntune_stream::body::{run_body_on_lane, BodyOutcome, BodyRequest};
use nntune_stream::pool::{Lane, Pool};
use nntune_stream::probe::{run_probe, ProbeError, ProbeRequest};
use nntune_test_nntp::{default_corpus, ServerConfig, TestNntpServer};

const ACCOUNT: &str = "12345678-1234-4234-8234-123456789abc";
const SECRET: &str = "22345678-1234-4234-8234-123456789abc";
type Client = NntpClient<NativeTlsTransport>;

fn account(cap: u32) -> AccountConfig {
    AccountConfig {
        id: ACCOUNT.into(),
        revision: 1,
        secret_ref: SECRET.into(),
        can_read: true,
        max_connections: cap,
        stream_reserve: 1,
        ingest_limit: cap - 1,
        tier: 0,
        enabled: true,
    }
}

fn config(addr: SocketAddr, password: &str) -> NntpConfig {
    NntpConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        tls: false,
        username: Some("testuser".into()),
        password: Some(password.into()),
        timeout: Duration::from_secs(2),
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

fn request(user: &str) -> BodyRequest {
    BodyRequest {
        job_id: "job".into(),
        user_id: user.into(),
        idempotency_key: "idem".into(),
        segment_id: default_corpus()[0]
            .message_id
            .trim_matches(['<', '>'])
            .into(),
    }
}

fn no_tick(_: &mut Pool<Client>) {}

#[tokio::test]
async fn two_users_and_ingest_share_one_hard_account_cap() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut server_cfg = ServerConfig::for_test();
        server_cfg.latency_ms = 50;
        let server = TestNntpServer::spawn(server_cfg).await;
        let pool = AccountPool::<Client>::new(account(2)).unwrap();
        let cfg = config(server.addr(), "testpass");
        let dials = Cell::new(0);
        let ingest = request("admin");
        let user_a = request("user-a");
        let user_b = request("user-b");
        let (a, b, c, d) = tokio::join!(
            run_body_on_lane(
                pool.pool(),
                || dial(cfg.clone(), &dials),
                no_tick,
                &ingest,
                Lane::Ingest
            ),
            run_body_on_lane(
                pool.pool(),
                || dial(cfg.clone(), &dials),
                no_tick,
                &user_a,
                Lane::Playback
            ),
            run_body_on_lane(
                pool.pool(),
                || dial(cfg.clone(), &dials),
                no_tick,
                &user_b,
                Lane::Playback
            ),
            run_body_on_lane(
                pool.pool(),
                || dial(cfg.clone(), &dials),
                no_tick,
                &ingest,
                Lane::Ingest
            ),
        );
        let results = [a, b, c, d];
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Ok(BodyOutcome::Bytes(_))))
                .count(),
            2
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(ProbeError::Busy { .. })))
                .count(),
            2
        );
        assert_eq!(dials.get(), 2, "no rejected request may dial or retry");
        assert_eq!(pool.pool().borrow().in_use(), 0);
        assert_eq!(pool.pool().borrow().idle(), 2);
    })
    .await
    .unwrap();
}

#[test]
fn ingest_limit_and_stream_reserve_hold_even_in_half_open() {
    let pool = AccountPool::<()>::new(account(3)).unwrap();
    let mut inner = pool.pool().borrow_mut();
    let a = inner.try_acquire(Lane::Ingest).unwrap();
    let b = inner.try_acquire(Lane::Ingest).unwrap();
    assert!(inner.try_acquire(Lane::Ingest).is_err());
    let stream = inner.try_acquire(Lane::Playback).unwrap();
    assert!(inner.try_acquire(Lane::Playback).is_err());
    drop((a, b, stream));
    for _ in 0..3 {
        inner.record_provider_error();
    }
    inner.advance(30_001);
    let mut cfg = account(1);
    cfg.revision = 2;
    drop(inner);
    pool.reconcile(cfg).unwrap();
    let mut inner = pool.pool().borrow_mut();
    assert!(
        inner.try_acquire(Lane::Ingest).is_err(),
        "half-open is not permission to borrow reserve"
    );
    assert!(inner.try_acquire(Lane::Playback).is_ok());
}

#[tokio::test]
async fn bad_credentials_quarantine_once_until_secret_identity_rotates() {
    let server = TestNntpServer::spawn_for_test().await;
    let pool = AccountPool::<Client>::new(account(2)).unwrap();
    let bad = config(server.addr(), "wrong");
    let good = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let req = request("a");
    let first = run_body_on_lane(
        pool.pool(),
        || dial(bad.clone(), &dials),
        no_tick,
        &req,
        Lane::Playback,
    )
    .await;
    assert_eq!(first, Err(ProbeError::Provider));
    assert!(pool.pool().borrow().auth_quarantined());
    pool.pool().borrow_mut().advance(86_400_000);
    pool.pool().borrow_mut().record_success(); // a stale success cannot lift auth quarantine
    let mut updated = account(3);
    updated.revision = 2;
    pool.reconcile(updated.clone()).unwrap();
    for _ in 0..3 {
        assert!(matches!(
            run_body_on_lane(
                pool.pool(),
                || dial(good.clone(), &dials),
                no_tick,
                &req,
                Lane::Playback
            )
            .await,
            Err(ProbeError::Busy { .. })
        ));
    }
    assert_eq!(dials.get(), 1);
    updated.revision = 3;
    updated.secret_ref = "32345678-1234-4234-8234-123456789abc".into();
    pool.reconcile(updated).unwrap();
    assert!(matches!(
        run_body_on_lane(
            pool.pool(),
            || dial(good.clone(), &dials),
            no_tick,
            &req,
            Lane::Playback
        )
        .await,
        Ok(BodyOutcome::Bytes(_))
    ));
    assert_eq!(dials.get(), 2);
}

#[tokio::test]
async fn probe_auth_failure_uses_the_same_account_quarantine() {
    let server = TestNntpServer::spawn_for_test().await;
    let pool = AccountPool::<Client>::new(account(2)).unwrap();
    let cfg = config(server.addr(), "wrong");
    let dials = Cell::new(0);
    let req = ProbeRequest {
        job_id: "job".into(),
        user_id: "admin".into(),
        idempotency_key: "idem".into(),
        batch_index: 0,
        segment_ids: vec![request("admin").segment_id],
    };
    assert_eq!(
        run_probe(pool.pool(), || dial(cfg.clone(), &dials), no_tick, &req).await,
        Err(ProbeError::Provider)
    );
    assert!(pool.pool().borrow().auth_quarantined());
    assert!(
        run_probe(pool.pool(), || dial(cfg.clone(), &dials), no_tick, &req)
            .await
            .is_err()
    );
    assert_eq!(dials.get(), 1);
}

#[tokio::test]
async fn kill_switch_during_a_fetch_does_not_repark_or_admit_another_session() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut server_cfg = ServerConfig::for_test();
        server_cfg.latency_ms = 50;
        let server = TestNntpServer::spawn(server_cfg).await;
        let pool = AccountPool::<Client>::new(account(2)).unwrap();
        let cfg = config(server.addr(), "testpass");
        let dials = Cell::new(0);
        let req = request("user-a");
        let (first, ()) = tokio::join!(
            run_body_on_lane(
                pool.pool(),
                || dial(cfg.clone(), &dials),
                no_tick,
                &req,
                Lane::Playback
            ),
            async {
                while dials.get() == 0 {
                    tokio::task::yield_now().await;
                }
                let mut disabled = account(2);
                disabled.revision = 2;
                disabled.enabled = false;
                pool.reconcile(disabled).unwrap();
                assert!(run_body_on_lane(
                    pool.pool(),
                    || dial(cfg.clone(), &dials),
                    no_tick,
                    &req,
                    Lane::Playback
                )
                .await
                .is_err());
            }
        );
        assert!(matches!(first, Ok(BodyOutcome::Bytes(_))));
        assert_eq!(dials.get(), 1);
        assert_eq!(
            pool.pool().borrow().idle(),
            0,
            "old in-flight socket is discarded"
        );
        assert!(!pool.candidate(WorkClass::Stream).eligible);
    })
    .await
    .unwrap();
}

#[test]
fn recapping_and_rotation_keep_outstanding_leases_counted() {
    let pool = AccountPool::<()>::new(account(3)).unwrap();
    let a = pool
        .pool()
        .borrow_mut()
        .try_acquire(Lane::Playback)
        .unwrap();
    let b = pool
        .pool()
        .borrow_mut()
        .try_acquire(Lane::Playback)
        .unwrap();
    let mut cfg = account(1);
    cfg.revision = 2;
    cfg.secret_ref = "32345678-1234-4234-8234-123456789abc".into();
    pool.reconcile(cfg.clone()).unwrap();
    assert_eq!(pool.pool().borrow().in_use(), 2);
    assert!(!pool.candidate(WorkClass::Stream).eligible);
    pool.pool().borrow_mut().release(a, Some(()));
    assert!(!pool.candidate(WorkClass::Stream).eligible);
    pool.pool().borrow_mut().release(b, Some(()));
    assert_eq!(pool.pool().borrow().idle(), 0);
    assert!(pool.candidate(WorkClass::Stream).eligible);
    assert_eq!(pool.reconcile(account(3)), Err(ConfigError::Stale));
    cfg.enabled = false;
    assert_eq!(pool.reconcile(cfg), Err(ConfigError::Conflict));
    assert!(pool.candidate(WorkClass::Stream).eligible);
}

#[test]
fn endpoint_revision_invalidates_idle_sockets_even_with_unchanged_budgets() {
    let pool = AccountPool::<()>::new(account(2)).unwrap();
    let lease = pool
        .pool()
        .borrow_mut()
        .try_acquire(Lane::Playback)
        .unwrap();
    pool.pool().borrow_mut().release(lease, Some(()));
    assert_eq!(pool.pool().borrow().idle(), 1);
    let mut next = account(2);
    next.revision = 2;
    pool.reconcile(next).unwrap();
    assert_eq!(pool.pool().borrow().idle(), 0);
    assert!(pool.candidate(WorkClass::Stream).eligible);
}

#[test]
fn old_credential_failures_cannot_quarantine_a_rotated_account() {
    let pool = AccountPool::<()>::new(account(2)).unwrap();
    let old = pool
        .pool()
        .borrow_mut()
        .try_acquire(Lane::Playback)
        .unwrap();
    let mut cfg = account(2);
    cfg.revision = 2;
    cfg.secret_ref = "32345678-1234-4234-8234-123456789abc".into();
    pool.reconcile(cfg).unwrap();
    pool.pool()
        .borrow_mut()
        .record_dial_failure_for(&old, &nntune_core::Error::AuthFailed("redacted".into()));
    assert!(!pool.pool().borrow().auth_quarantined());
    drop(old);
    assert!(pool.candidate(WorkClass::Stream).eligible);
}

#[tokio::test]
async fn single_account_stream_fetch_and_warm_reuse_work_without_a_fallback() {
    let server = TestNntpServer::spawn_for_test().await;
    let pool = AccountPool::<Client>::new(account(1)).unwrap();
    let cfg = config(server.addr(), "testpass");
    let dials = Cell::new(0);
    let req = request("user-a");
    assert_eq!(
        ranked_candidates(vec![pool.candidate(WorkClass::Stream)]).len(),
        1
    );
    assert!(ranked_candidates(vec![pool.candidate(WorkClass::Ingest)]).is_empty());
    for _ in 0..2 {
        assert!(matches!(
            run_body_on_lane(
                pool.pool(),
                || dial(cfg.clone(), &dials),
                no_tick,
                &req,
                Lane::Playback
            )
            .await,
            Ok(BodyOutcome::Bytes(_))
        ));
    }
    assert_eq!(dials.get(), 1);
}

#[test]
fn selection_uses_tier_then_fewest_in_flight_then_stable_id() {
    let candidate = |id: &str, tier, in_flight, eligible| Candidate {
        id: id.into(),
        tier,
        in_flight,
        eligible,
    };
    let ranked = ranked_candidates(vec![
        candidate("disabled", 0, 0, false),
        candidate("c", 1, 0, true),
        candidate("b", 0, 2, true),
        candidate("a", 0, 1, true),
    ]);
    assert_eq!(
        ranked.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
}

#[tokio::test]
async fn expiry_cancels_the_client_before_a_slot_is_reused() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut server_cfg = ServerConfig::for_test();
        server_cfg.latency_ms = 500;
        let server = TestNntpServer::spawn(server_cfg).await;
        let pool = AccountPool::<Client>::new(account(1)).unwrap();
        let cfg = config(server.addr(), "testpass");
        let dials = Cell::new(0);
        let req = request("user-a");
        let expired = with_session_deadline(
            run_body_on_lane(
                pool.pool(),
                || dial(cfg.clone(), &dials),
                no_tick,
                &req,
                Lane::Playback,
            ),
            tokio::time::sleep(Duration::from_millis(50)),
        )
        .await;
        assert!(expired.is_err());
        assert_eq!(dials.get(), 1);
        assert_eq!(pool.pool().borrow().in_use(), 0);
        assert_eq!(pool.pool().borrow().idle(), 0);
        assert!(pool.candidate(WorkClass::Stream).eligible);
    })
    .await
    .unwrap();
}
