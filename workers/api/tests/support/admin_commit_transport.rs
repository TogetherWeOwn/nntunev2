//! Cut the real PostgreSQL transport at COMMIT, not a simulated adapter error.
//! Only the disposable loopback database and copied scratch schema are reachable.
//! No packet contents are logged; this is not live routing/schema/broker proof.
use super::*;
use std::io;
use std::sync::{atomic::AtomicBool, Arc};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_postgres::{Config, NoTls};

#[derive(Clone, Copy, PartialEq, Eq)]
enum CutAt {
    BeforeCommit,
    AfterCommit,
}

struct TransportCut {
    relay: JoinHandle<io::Result<()>>,
    driver: JoinHandle<Result<(), tokio_postgres::Error>>,
    observed: oneshot::Receiver<CutAt>,
}

impl TransportCut {
    async fn finish(&mut self, expected: CutAt) -> bool {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (observed, relay, driver) =
                tokio::join!(&mut self.observed, &mut self.relay, &mut self.driver);
            matches!(observed, Ok(actual) if actual == expected)
                && matches!(relay, Ok(Ok(())))
                && matches!(driver, Ok(Err(_)))
        })
        .await
        .unwrap_or(false)
    }
}

impl Drop for TransportCut {
    fn drop(&mut self) {
        self.relay.abort();
        self.driver.abort();
    }
}

// PostgreSQL v3 frames are length-prefixed. Bound every allocation and never
// format a frame in a diagnostic (startup/auth and query frames may be sensitive).
async fn payload(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
    let length = reader.read_u32().await?;
    if !(4..=1_048_576).contains(&length) {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut bytes = vec![0; length as usize - 4];
    reader.read_exact(&mut bytes).await?;
    Ok(bytes)
}

async fn forward(
    writer: &mut (impl AsyncWrite + Unpin),
    tag: Option<u8>,
    bytes: &[u8],
) -> io::Result<()> {
    if let Some(tag) = tag {
        writer.write_u8(tag).await?;
    }
    writer.write_u32(bytes.len() as u32 + 4).await?;
    writer.write_all(bytes).await?;
    writer.flush().await
}

async fn cut_connection(cut_at: CutAt) -> (Client, TransportCut) {
    let config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("CI disposable database is required; never silently skip")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(config.get_hostaddrs().is_empty(), "no address overrides");
    let host = match config.get_hosts() {
        [tokio_postgres::config::Host::Tcp(host)]
            if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") =>
        {
            host
        }
        _ => panic!("only the disposable loopback database is allowed"),
    };
    assert!(config.get_ports().len() <= 1);
    let port = config.get_ports().first().copied().unwrap_or(5432);
    let server = TcpStream::connect((host.as_str(), port)).await.unwrap();
    assert!(server.peer_addr().unwrap().ip().is_loopback());
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let client_socket = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (peer, address) = listener.accept().await.unwrap();
    assert!(address.ip().is_loopback());
    drop(listener);
    let (observed, hit) = oneshot::channel();
    let relay = tokio::spawn(async move {
        let (mut from_client, mut to_client) = peer.into_split();
        let (mut from_server, mut to_server) = server.into_split();
        let committing = Arc::new(AtomicBool::new(false));
        let frontend_commit = committing.clone();
        let (mut before, mut after) = match cut_at {
            CutAt::BeforeCommit => (Some(observed), None),
            CutAt::AfterCommit => (None, Some(observed)),
        };
        let frontend = async move {
            // connect_raw with NoTls sends the startup packet without SSLRequest.
            let startup = payload(&mut from_client).await?;
            forward(&mut to_server, None, &startup).await?;
            loop {
                let tag = from_client.read_u8().await?;
                let bytes = payload(&mut from_client).await?;
                if tag == b'Q' && bytes == b"COMMIT\0" {
                    if let Some(before) = before.take() {
                        let _ = before.send(CutAt::BeforeCommit);
                        return Ok(());
                    }
                    frontend_commit.store(true, Ordering::Release);
                }
                forward(&mut to_server, Some(tag), &bytes).await?;
            }
        };
        let backend = async move {
            loop {
                let tag = from_server.read_u8().await?;
                let bytes = payload(&mut from_server).await?;
                if tag == b'C' && bytes == b"COMMIT\0" && committing.load(Ordering::Acquire) {
                    if let Some(after) = after.take() {
                        // The server committed, but neither CommandComplete nor
                        // ReadyForQuery reaches the caller. Dropping BOTH relay
                        // directions closes the connection, yielding a real error.
                        let _ = after.send(CutAt::AfterCommit);
                        return Ok(());
                    }
                }
                forward(&mut to_client, Some(tag), &bytes).await?;
            }
        };
        tokio::select! {
            result = frontend => result,
            result = backend => result,
        }
    });
    let (client, connection) = config.connect_raw(client_socket, NoTls).await.unwrap();
    let driver = tokio::spawn(connection);
    client
        .batch_execute("SET statement_timeout = '3s'")
        .await
        .unwrap();
    (
        client,
        TransportCut {
            relay,
            driver,
            observed: hit,
        },
    )
}

#[derive(Default)]
struct MatchService {
    calls: usize,
    transient: bool,
}

impl store::StageServices for MatchService {
    async fn match_release(&mut self, _: &Document) -> Result<Option<String>, store::ServiceError> {
        self.calls += 1;
        if self.transient {
            Err(store::ServiceError::Transient {
                retry_after_secs: Some(300),
            })
        } else {
            Ok(Some(RELEASE.into()))
        }
    }

    async fn probe(&mut self, _: &ProbeRequest) -> Result<ProbeOutcome, store::ServiceError> {
        panic!("one matching stage must not reach probe")
    }
}

async fn continuation_commit_disconnect(transient: bool) {
    for cut_at in [CutAt::BeforeCommit, CutAt::AfterCommit] {
        let (mut app, original, initial) = seeded().await;
        let mut cp = initial.checkpoint().clone();
        let matching = {
            let mut runtime = DatabaseRuntime {
                app: &mut app,
                original: &original,
                snapshot: initial,
                services: Runtime(cp.clone()),
            };
            for _ in 0..2 {
                run_stage(
                    &mut runtime,
                    &mut cp,
                    original.job_id(),
                    original.idempotency_key(),
                    original.parsed(),
                )
                .await
                .unwrap();
            }
            runtime.snapshot
        };
        assert_eq!(matching.checkpoint().state, JobState::Matching);
        let delivery = store::prepare_delivery(&app, &input(&original), matching.generation())
            .await
            .unwrap()
            .unwrap();
        let rows = SharedRows::copy_from(&app).await;
        // An uncut canonical call is the full-successor/outcome control, not a
        // copied checkpoint oracle or a production crash-recovery protocol.
        let mut healthy_services = MatchService {
            transient,
            calls: 0,
        };
        let mut healthy_queue = DeliveryQueue::default();
        let healthy = store::run_delivery_and_enqueue(
            &mut app,
            &input(&original),
            &delivery,
            &mut healthy_services,
            &mut healthy_queue,
        )
        .await
        .unwrap();
        let expected = store::load(&app, &input(&original), delivery.generation())
            .await
            .unwrap();
        let (mut worker, mut transport) = cut_connection(cut_at).await;
        rows.select_schema(&worker).await;
        let mut services = MatchService {
            transient,
            calls: 0,
        };
        let mut queue = DeliveryQueue::default();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            store::run_delivery_and_enqueue(
                &mut worker,
                &input(&original),
                &delivery,
                &mut services,
                &mut queue,
            ),
        )
        .await;
        drop(worker);
        let cut_proved = transport.finish(cut_at).await;
        drop(transport);
        let saved = store::load(&rows.observer, &input(&original), delivery.generation())
            .await
            .unwrap();
        let sends_after_error = queue.calls;
        let mut recovery = rows.client().await;
        if cut_at == CutAt::BeforeCommit {
            // The independent reload proves rollback; redoing this replayable
            // provider read is allowed, and commits exactly one budget change.
            let replay = store::run_delivery_and_enqueue(
                &mut recovery,
                &input(&original),
                &delivery,
                &mut services,
                &mut queue,
            )
            .await
            .unwrap();
            assert_eq!(replay, healthy);
        } else {
            // Known test outcome from the healthy control, not inferred from
            // Err(Database). This proves send-only recovery does not rerun the
            // provider; durable retry-due/outcome recovery is still required.
            assert_eq!(
                store::continue_delivery(
                    &recovery,
                    &input(&original),
                    &delivery,
                    healthy.outcome,
                    &mut queue,
                )
                .await,
                Ok(true)
            );
        }
        let recovered = store::load(&rows.observer, &input(&original), delivery.generation())
            .await
            .unwrap();
        let rejected = store::apply_control(
            &mut recovery,
            actor(),
            &input(&original),
            &recovered,
            Control::Reject,
        )
        .await
        .unwrap();
        let stale = store::continue_delivery(
            &recovery,
            &input(&original),
            &delivery,
            healthy.outcome,
            &mut queue,
        )
        .await;
        let final_authority = store::load(&rows.observer, &input(&original), rejected.generation())
            .await
            .unwrap();
        let album_counts = counts(&rows.observer).await;
        let audits: i64 = rows
            .observer
            .query_one("SELECT count(*) FROM audit_log", &[])
            .await
            .unwrap()
            .get(0);
        rows.cleanup().await;

        assert!(
            cut_proved,
            "expected COMMIT cut and failed driver must be observed"
        );
        assert!(matches!(result, Ok(Err(StoreError::Database))));
        assert_eq!(sends_after_error, 0);
        assert_eq!(healthy_services.calls, 1);
        assert_eq!(healthy.continuation, Ok(true));
        assert_eq!(healthy_queue.calls, 1);
        assert_eq!(healthy_queue.delays, [if transient { 300 } else { 0 }]);
        assert_eq!(expected.revision(), matching.revision() + 1);
        assert_eq!(expected.generation(), matching.generation());
        assert_eq!(expected.checkpoint().attempt, if transient { 2 } else { 1 });
        if cut_at == CutAt::AfterCommit {
            assert!(saved == expected);
            assert_eq!(services.calls, 1);
        } else {
            assert!(saved == matching);
            assert_eq!(services.calls, 2);
        }
        assert!(recovered == expected);
        assert_eq!(queue.calls, 1);
        assert_eq!(queue.delays, healthy_queue.delays);
        assert_eq!(queue.messages, healthy_queue.messages);
        assert_eq!(
            stale,
            Err(store::DeliveryError::Authority(StoreError::Stale))
        );
        assert!(final_authority == rejected);
        assert!(rejected.generation() > delivery.generation());
        assert_eq!(audits, 1);
        assert_eq!(album_counts, (0, 0, 0));
        assert_eq!(counts(&app).await, (0, 0, 0));
        assert!(
            store::load(&app, &input(&original), delivery.generation())
                .await
                .unwrap()
                == expected
        );
    }
}

#[tokio::test]
async fn continuation_commit_disconnect_never_sends_uncertain_stage_progress() {
    continuation_commit_disconnect(false).await;
}

#[tokio::test]
async fn continuation_commit_disconnect_never_sends_or_respends_uncertain_service_budget() {
    continuation_commit_disconnect(true).await;
}

#[tokio::test]
async fn publication_commit_disconnect_reloads_atomic_authority_and_never_duplicates_album() {
    for cut_at in [CutAt::BeforeCommit, CutAt::AfterCommit] {
        let (app, original, writing, recordings, geometry) = publication_fixture().await;
        let rows = SharedRows::copy_from(&app).await;
        let (mut publisher, mut transport) = cut_connection(cut_at).await;
        rows.select_schema(&publisher).await;
        let mut sealer = Sealer::default();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            store::publish(
                &mut publisher,
                &input(&original),
                &writing,
                &recordings,
                &geometry,
                &mut sealer,
            ),
        )
        .await;
        drop(publisher);
        let cut_proved = transport.finish(cut_at).await;
        drop(transport);
        let saved = store::load(&rows.observer, &input(&original), writing.generation())
            .await
            .unwrap();
        let album_counts = counts(&rows.observer).await;
        let mut recovery = rows.client().await;
        let mut replay_sealer = Sealer::default();
        let replay = store::publish(
            &mut recovery,
            &input(&original),
            &writing,
            &recordings,
            &geometry,
            &mut replay_sealer,
        )
        .await;
        let final_counts = counts(&rows.observer).await;
        let final_saved = store::load(&rows.observer, &input(&original), writing.generation())
            .await
            .unwrap();
        rows.cleanup().await;

        assert!(
            cut_proved,
            "expected transport cut and failed connection driver must be observed"
        );
        assert!(matches!(
            result,
            Ok(Err(store::PublicationError::Checkpoint(
                StoreError::Database
            )))
        ));
        assert_eq!(sealer.calls, 1);
        assert_eq!(saved.generation(), writing.generation());
        if cut_at == CutAt::AfterCommit {
            assert_eq!(saved.checkpoint().state, JobState::Done);
            assert_eq!(saved.revision(), writing.revision() + 1);
            assert_eq!(album_counts, (1, 1, 1));
            assert!(matches!(
                replay,
                Err(store::PublicationError::Checkpoint(StoreError::Stale))
            ));
            assert_eq!(replay_sealer.calls, 0);
            assert!(final_saved == saved);
        } else {
            assert!(saved == writing);
            assert_eq!(album_counts, (0, 0, 0));
            let done = replay.unwrap();
            assert_eq!(done.checkpoint().state, JobState::Done);
            assert!(final_saved == done);
            assert_eq!(replay_sealer.calls, 1);
        }
        assert_eq!(final_counts, (1, 1, 1));
        assert_eq!(counts(&app).await, (0, 0, 0));
    }
}

#[tokio::test]
async fn control_commit_disconnect_reloads_rotated_generation_and_exactly_one_audit() {
    for cut_at in [CutAt::BeforeCommit, CutAt::AfterCommit] {
        let (app, original, writing, _, _) = publication_fixture().await;
        let rows = SharedRows::copy_from(&app).await;
        let (mut controller, mut transport) = cut_connection(cut_at).await;
        rows.select_schema(&controller).await;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            store::apply_control(
                &mut controller,
                actor(),
                &input(&original),
                &writing,
                Control::Reject,
            ),
        )
        .await;
        drop(controller);
        let cut_proved = transport.finish(cut_at).await;
        drop(transport);
        // Resolve the specific attempted successor, not an arbitrary latest
        // generation. A newer independent control cannot be adopted by old work.
        let successor_generation = writing.revision() + 1;
        let resolved = store::load(&rows.observer, &input(&original), successor_generation).await;
        let old = store::load(&rows.observer, &input(&original), writing.generation()).await;
        let audit_count: i64 = rows
            .observer
            .query_one("SELECT count(*) FROM audit_log", &[])
            .await
            .unwrap()
            .get(0);
        let mut recovery = rows.client().await;
        let replay = store::apply_control(
            &mut recovery,
            actor(),
            &input(&original),
            &writing,
            Control::Reject,
        )
        .await;
        let final_saved = store::load(&rows.observer, &input(&original), successor_generation)
            .await
            .unwrap();
        let audits = rows
            .observer
            .query("SELECT action, ok, details::text FROM audit_log", &[])
            .await
            .unwrap();
        let deliveries =
            store::prepare_delivery(&rows.observer, &input(&original), successor_generation)
                .await
                .unwrap();
        let album_counts = counts(&rows.observer).await;
        rows.cleanup().await;

        assert!(
            cut_proved,
            "expected transport cut and failed connection driver must be observed"
        );
        assert!(matches!(result, Ok(Err(StoreError::Database))));
        if cut_at == CutAt::AfterCommit {
            let rejected = resolved.unwrap();
            assert_eq!(rejected.checkpoint().state, JobState::Rejected);
            assert!(matches!(old, Err(StoreError::Stale)));
            assert_eq!(audit_count, 1);
            assert!(matches!(replay, Err(StoreError::Stale)));
            assert!(final_saved == rejected);
        } else {
            assert!(matches!(resolved, Err(StoreError::Stale)));
            assert!(old.unwrap() == writing);
            assert_eq!(audit_count, 0);
            assert!(replay.unwrap() == final_saved);
        }
        assert_eq!(final_saved.checkpoint().state, JobState::Rejected);
        assert_eq!(final_saved.revision(), successor_generation);
        assert_eq!(final_saved.generation(), successor_generation);
        assert!(deliveries.is_none());
        assert_eq!(album_counts, (0, 0, 0));
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].get::<_, String>(0), "ingest.controlled");
        assert!(audits[0].get::<_, bool>(1));
        let details: serde_json::Value =
            serde_json::from_str(&audits[0].get::<_, String>(2)).unwrap();
        assert_eq!(
            details,
            serde_json::json!({
                "job_id": original.job_id(), "operation": "reject",
                "previous_status": writing.status(), "status": "rejected",
                "previous_revision": writing.revision(), "revision": successor_generation,
                "generation": successor_generation,
            })
        );
    }
}
