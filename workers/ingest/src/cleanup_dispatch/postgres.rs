use std::{future::Future, time::Duration};

use tokio_postgres::{types::ToSql, Client, IsolationLevel, Transaction};
use worker::{Delay, Env};

use super::{
    handoff, send_cleanup_message, CleanupClaim, CleanupKind, CleanupMessage, HandoffError,
};
use crate::library::{with_db_deadline, LIBRARY_DB_DEADLINE_MS};

const CLAIM: &str = include_str!("../../../api/src/ingest_session_claim_cleanup.sql");
const ACK: &str = include_str!("../../../api/src/ingest_session_ack_cleanup_enqueue.sql");

#[cfg(any(target_arch = "wasm32", test))]
mod reconcile;

#[cfg(test)]
#[path = "../../tests/fixtures/multipart_reconcile_postgres.rs"]
mod reconcile_tests;

/// Internal scope from an authorized session; UUIDs use the hyphenated form.
#[derive(Clone, Copy)]
pub struct CleanupScope<'a> {
    pub environment_id: &'a str,
    pub library_id: &'a str,
    pub principal_id: &'a str,
    pub session_id: &'a str,
}

#[derive(Debug, Eq, PartialEq)]
pub enum DispatchError {
    ClaimNotConfirmed,
    MultipartNotConfirmed,
    Handoff(HandoffError),
}

/// Inactive seam. Requires a dedicated connection to the trusted writer DB.
/// Stop on uncertainty: outstanding claims can be recovered after lease expiry.
/// No database transaction stays open across a queue send.
pub async fn dispatch_scope(
    env: &Env,
    mut db: Client,
    scope: CleanupScope<'_>,
) -> Result<(), DispatchError> {
    dispatch_claims(
        &mut db,
        scope,
        async |_, _| Ok(()),
        |message, delay| send_cleanup_message(env, message, delay),
        db_deadline,
    )
    .await
}

/// Dormant staging entry point. Bind the provider and environment from the
/// isolated upload configuration. A failed reconciliation leaves the outbox
/// claim retryable after expiry, with no queue acknowledgement.
#[cfg(target_arch = "wasm32")]
pub async fn dispatch_multipart_scope(
    env: &Env,
    mut db: Client,
    scope: CleanupScope<'_>,
    environment: [u8; 16],
    provider: &crate::multipart::s3_completion::S3CompletionSender,
) -> Result<(), DispatchError> {
    reconcile::dispatch_multipart_claims(
        &mut db,
        scope,
        environment,
        provider.multipart_scope(),
        (
            async |key, until| provider.find_incomplete(key, until).await,
            async |key, id, until| provider.abort_incomplete(key, id, until).await,
        ),
        |message, delay| send_cleanup_message(env, message, delay),
        db_deadline,
    )
    .await
}

async fn dispatch_claims<S, D>(
    db: &mut Client,
    scope: CleanupScope<'_>,
    mut prepare: impl AsyncFnMut(&mut Client, &CleanupClaim) -> Result<(), ()>,
    mut send: impl FnMut(CleanupMessage, u32) -> S,
    deadline: impl Fn() -> D,
) -> Result<(), DispatchError>
where
    S: Future<Output = Result<(), ()>>,
    D: Future<Output = ()>,
{
    let mut claims = with_db_deadline(scope.claim(db), deadline(), || ())
        .await
        .map_err(|()| DispatchError::ClaimNotConfirmed)?;
    claims.sort_by_key(|claim| claim.kind == CleanupKind::TerminalReconcile);
    for claim in claims {
        with_db_deadline(prepare(db, &claim), deadline(), || ())
            .await
            .map_err(|()| DispatchError::MultipartNotConfirmed)?;
        let db = &mut *db;
        let deadline = &deadline;
        handoff(claim, &mut send, |_, kind, id| async move {
            with_db_deadline(scope.ack(db, kind, &id), deadline(), || ()).await
        })
        .await
        .map_err(DispatchError::Handoff)?;
    }
    Ok(())
}

fn db_deadline() -> Delay {
    Delay::from(Duration::from_millis(LIBRARY_DB_DEADLINE_MS))
}

async fn begin(db: &mut Client) -> Result<Transaction<'_>, ()> {
    let tx = db
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .start()
        .await
        .map_err(|_| ())?;
    tx.batch_execute("SET LOCAL statement_timeout = '10s'; SET LOCAL idle_in_transaction_session_timeout = '10s'; SET LOCAL synchronous_commit = on")
        .await
        .map_err(|_| ())?;
    Ok(tx)
}

impl CleanupScope<'_> {
    fn params(&self) -> [&(dyn ToSql + Sync); 4] {
        [
            &self.environment_id,
            &self.library_id,
            &self.principal_id,
            &self.session_id,
        ]
    }

    async fn claim(&self, db: &mut Client) -> Result<Vec<CleanupClaim>, ()> {
        let tx = begin(db).await?;
        let rows = tx.query(CLAIM, &self.params()).await.map_err(|_| ())?;
        // No caller receives a sendable claim unless COMMIT returned success.
        tx.commit().await.map_err(|_| ())?;
        rows.iter()
            .map(|row| {
                let kind = match row.try_get::<_, &str>("kind").map_err(|_| ())? {
                    "absolute_expiry" => CleanupKind::AbsoluteExpiry,
                    "terminal_reconcile" => CleanupKind::TerminalReconcile,
                    _ => return Err(()),
                };
                CleanupClaim::from_committed_row(
                    row.try_get("session_id").map_err(|_| ())?,
                    kind,
                    row.try_get("delivery_claim_id").map_err(|_| ())?,
                    row.try_get("delay_seconds").map_err(|_| ())?,
                )
                .filter(|claim| claim.session_id.eq_ignore_ascii_case(self.session_id))
                .ok_or(())
            })
            .collect()
    }

    async fn ack(&self, db: &mut Client, kind: CleanupKind, claim_id: &str) -> Result<bool, ()> {
        let kind = match kind {
            CleanupKind::AbsoluteExpiry => "absolute_expiry",
            CleanupKind::TerminalReconcile => "terminal_reconcile",
        };
        let tx = begin(db).await?;
        let [environment, library, principal, session] = self.params();
        let matched = tx
            .query_opt(
                ACK,
                &[environment, library, principal, session, &kind, &claim_id],
            )
            .await
            .map_err(|_| ())?
            .is_some();
        tx.commit().await.map_err(|_| ())?;
        Ok(matched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{sync::oneshot, time};

    #[tokio::test]
    #[ignore = "requires the disposable hosted migration database"]
    async fn postgres_handoff_commits_claim_and_scoped_ack() {
        let url = std::env::var("NNTUNE_TEST_DATABASE_URL").expect("scratch database URL");
        assert!(
            std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
                && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
                && url == "postgresql://postgres:ci-throwaway@127.0.0.1:5432/nntune_ci"
        );
        let (mut db, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(async move { connection.await.unwrap() });
        let (mut observer, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(async move { connection.await.unwrap() });
        let scope = CleanupScope {
            environment_id: "01000000-0000-4000-8000-000000000001",
            library_id: "11000000-0000-4000-8000-000000000001",
            principal_id: "digest-test-owner",
            session_id: "51000000-0000-4000-8000-000000000038",
        };
        db.execute("INSERT INTO public.ingest_session_reservations
            (id, library_id, principal_id, idempotency_key, fingerprint, absolute_deadline, idle_deadline)
            VALUES ($1::text::uuid, $2::text::uuid, $3, 'rust-handoff', decode(repeat('ca', 32), 'hex'),
                    clock_timestamp() + interval '24 hours', clock_timestamp() + interval '1 hour')",
            &[&scope.session_id, &scope.library_id, &scope.principal_id]).await.unwrap();
        db.execute("INSERT INTO public.ingest_session_cleanup_intents (session_id, kind, due_at)
            SELECT id, 'absolute_expiry', absolute_deadline FROM public.ingest_session_reservations WHERE id=$1::text::uuid",
            &[&scope.session_id]).await.unwrap();
        let outside = CleanupScope {
            principal_id: "not-the-owner",
            ..scope
        };
        let deadline = || time::sleep(Duration::from_millis(LIBRARY_DB_DEADLINE_MS));
        dispatch_claims(
            &mut db,
            outside,
            async |_, _| Ok(()),
            |_, _| async { panic!("out-of-scope dispatch attempted a queue send") },
            deadline,
        )
        .await
        .unwrap();

        let (sent, observed_send) = oneshot::channel();
        let (accept, accepted) = oneshot::channel();
        let mut queue = Some((sent, accepted));
        let dispatch = dispatch_claims(
            &mut db,
            scope,
            async |_, _| Ok(()),
            move |message, delay| {
                let (sent, accepted) = queue.take().expect("unexpected second queue send");
                async move {
                    assert!(sent.send((message, delay)).is_ok());
                    accepted.await.map_err(|_| ())
                }
            },
            deadline,
        );
        let observe = async {
            let (message, delay) = observed_send
                .await
                .expect("dispatch skipped the queue send");
            let CleanupMessage::UploadSessionCleanup { session_id, kind } = message;
            assert_eq!(session_id, scope.session_id);
            assert_eq!(kind, CleanupKind::AbsoluteExpiry);
            assert!((1..=86400).contains(&delay));
            let intent = observer.query_one(
                "SELECT delivery_claim_id::text, enqueued_at IS NULL, claimed_until > clock_timestamp()
                 FROM public.ingest_session_cleanup_intents WHERE session_id=$1::text::uuid AND kind='absolute_expiry'",
                &[&scope.session_id],
            ).await.unwrap();
            let claim_id: String = intent
                .get::<_, Option<String>>(0)
                .expect("claim must be committed and visible before send");
            assert!(
                intent.get::<_, bool>(1),
                "acknowledged before queue acceptance"
            );
            assert!(intent.get::<_, bool>(2), "claim must be live at send time");
            assert!(!outside.ack(&mut observer, kind, &claim_id).await.unwrap());
            assert!(accept.send(()).is_ok());
            claim_id
        };
        let (outcome, claim_id) = time::timeout(Duration::from_secs(30), async {
            tokio::join!(dispatch, observe)
        })
        .await
        .expect("dispatch or queue observation timed out");
        outcome.unwrap();
        assert!(!scope
            .ack(&mut observer, CleanupKind::AbsoluteExpiry, &claim_id)
            .await
            .unwrap());
        let acknowledged: bool = observer.query_one("SELECT enqueued_at IS NOT NULL AND delivery_claim_id IS NULL AND claimed_until IS NULL
            FROM public.ingest_session_cleanup_intents WHERE session_id=$1::text::uuid", &[&scope.session_id]).await.unwrap().get(0);
        assert!(acknowledged);
        let tx = db.transaction().await.unwrap();
        tx.execute(
            "DELETE FROM public.ingest_session_cleanup_intents WHERE session_id = $1::text::uuid",
            &[&scope.session_id],
        )
        .await
        .unwrap();
        tx.execute(
            "DELETE FROM public.ingest_session_reservations WHERE id = $1::text::uuid",
            &[&scope.session_id],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
}
