//! Drive COMMIT faults through the production Store acquisition.
use super::commit_ack_fault::{CommitAckFault, CommitOutcome};
use super::disposable_accounts::{self, Accounts};
use nntune_api::playback::LeaseAcquire::Acquired;
use nntune_api::worker_entry::private_aware_lease_for_postgres_test as acquire;
use std::error::Error;
use std::time::Duration;
use tokio::time::timeout;
use tokio_postgres::error::SqlState;

#[tokio::test]
async fn unacknowledged_acquisition_returns_error_but_commits_private_replacement() {
    let (request, fault) = CommitAckFault::start().await.unwrap();
    let accounts = timeout(Duration::from_secs(5), Accounts::new(1))
        .await
        .unwrap();
    let result = timeout(Duration::from_secs(15), async {
        let db = &accounts.observer;
        let user = accounts.id(0);
        let now: i64 = db
            .query_one("SELECT extract(epoch from statement_timestamp())::bigint", &[])
            .await?
            .try_get(0)?;
        db.execute(
            "INSERT INTO public.private_sessions(user_id,session_id,expires_at) VALUES ($1,'private',$2)",
            &[&user, &(now + 300)],
        )
        .await?;
        let expires = now + 45;
        let initial = acquire(
            disposable_accounts::connect_guarded().await?,
            user,
            "incumbent",
            "track",
            "initial",
            now as u64,
            expires as u64,
            false,
        )
        .await
        .map_err(|_| "could not seed incumbent")?;
        if initial != (Acquired { epoch: 1 }) {
            return Err("wrong incumbent".into());
        }
        let result = acquire(
            request,
            user,
            "private",
            "track",
            "replacement",
            now as u64,
            expires as u64,
            true,
        )
        .await;
        let receipt = fault.finish().await?;
        if result.is_ok() || receipt != CommitOutcome::Withheld {
            return Err(format!("expected Store error and withheld ACK, got {result:?}, {receipt:?}").into());
        }
        // One snapshot from a different connection. query_one also rejects
        // extra marks; the exact epoch catches an extra lease replacement.
        let row = db
            .query_one(
                "SELECT session_id,track_id,play_id,epoch,expires_at,play_key,marked_at \
                 FROM public.playback_leases JOIN public.private_plays USING (user_id) \
                 WHERE user_id=$1",
                &[&user],
            )
            .await?;
        let observed = (
            row.try_get::<_, &str>(0)?,
            row.try_get::<_, &str>(1)?,
            row.try_get::<_, &str>(2)?,
            row.try_get::<_, i64>(3)?,
            row.try_get::<_, i64>(4)?,
            row.try_get::<_, &str>(5)?,
            row.try_get::<_, i64>(6)?,
        );
        let key = format!("play:{user}:track:replacement");
        if observed != ("private", "track", "replacement", 2, expires, key.as_str(), now) {
            return Err("observer did not find the exact committed replacement and mark".into());
        }
        Ok::<_, Box<dyn Error>>(())
    })
    .await;
    timeout(Duration::from_secs(5), accounts.cleanup())
        .await
        .unwrap();
    result.unwrap().unwrap();
}

#[tokio::test]
async fn commit_fault_distinguishes_rejection_and_pre_commit_disconnect() {
    timeout(Duration::from_secs(15), async {
        let (mut request, fault) = CommitAckFault::start().await?;
        request.batch_execute(
            "CREATE TEMP TABLE parent (id int PRIMARY KEY); \
             CREATE TEMP TABLE child (parent_id int REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED)",
        ).await?;
        let tx = request.transaction().await?;
        tx.batch_execute("INSERT INTO child VALUES (1)").await?;
        let error = tx.commit().await.expect_err("deferred FK must reject COMMIT");
        assert_eq!(error.code(), Some(&SqlState::FOREIGN_KEY_VIOLATION));
        assert_eq!(fault.finish().await?, CommitOutcome::Rejected);
        drop(request);

        let (mut request, fault) = CommitAckFault::start().await?;
        let tx = request.transaction().await?;
        drop(tx); // Roll back; do not send COMMIT.
        drop(request);
        assert_eq!(fault.finish().await?, CommitOutcome::Unproven);
        Ok::<_, Box<dyn Error>>(())
    }).await.unwrap().unwrap();
}
