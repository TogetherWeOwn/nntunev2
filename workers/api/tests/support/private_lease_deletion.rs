//! Disposable-PostgreSQL account isolation and deletion race control.
use super::{blocker_observer::observe_blocked, disposable_accounts};
use nntune_api::playback::LeaseAcquire::Acquired;
use nntune_api::worker_entry::private_aware_lease_for_postgres_test as acquire;
use std::time::Duration;
use tokio_postgres::Client;

async fn connection() -> Result<Client, String> {
    disposable_accounts::connect_guarded()
        .await
        .map_err(|e| e.to_string())
}

async fn pid(client: &Client) -> Result<i32, String> {
    Ok(client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .map_err(|e| e.to_string())?
        .get(0))
}

#[tokio::test]
async fn acquisition_is_account_scoped_and_loses_to_deletion() {
    let holder = connection().await.unwrap();
    let accounts = disposable_accounts::Accounts::new(2).await;
    let (a, b) = (accounts.id(0).to_owned(), accounts.id(1).to_owned());
    let db = &accounts.observer;
    let result: Result<(), String> = tokio::time::timeout(Duration::from_secs(20), async {
        let now: i64 = db
            .query_one(
                "SELECT extract(epoch from statement_timestamp())::bigint",
                &[],
            )
            .await
            .map_err(|e| e.to_string())?
            .get(0);
        db.execute(
            "INSERT INTO public.private_sessions VALUES ($1,'private',$3),($2,'private',$3)",
            &[&a, &b, &(now + 300)],
        )
        .await
        .map_err(|e| e.to_string())?;
        for user in [&a, &b] {
            if acquire(
                connection().await?,
                user,
                "private",
                "track",
                "old",
                now as u64,
                now as u64 + 45,
                false,
            )
            .await
            .map_err(|_| "seed failed")?
                != (Acquired { epoch: 1 }, true)
            {
                return Err("incorrect seed".into());
            }
        }
        let before_lease = db.query_one("SELECT session_id,track_id,play_id,epoch,expires_at,updated_at FROM public.playback_leases WHERE user_id=$1", &[&a]).await.map_err(|e| e.to_string())?;
        let before_mark = db.query_one("SELECT play_key,marked_at FROM public.private_plays WHERE user_id=$1", &[&a]).await.map_err(|e| e.to_string())?;
        let lease_snapshot: (String, String, String, i64, i64, i64) = (before_lease.get(0), before_lease.get(1), before_lease.get(2), before_lease.get(3), before_lease.get(4), before_lease.get(5));
        let mark_snapshot: (String, i64) = (before_mark.get(0), before_mark.get(1));
        holder
            .batch_execute("BEGIN")
            .await
            .map_err(|e| e.to_string())?;
        holder
            .execute(
                "UPDATE public.users SET deleting_at=now() WHERE id=$1",
                &[&a],
            )
            .await
            .map_err(|e| e.to_string())?;
        let waiter = connection().await?;
        let (holder_pid, waiter_pid) = (pid(&holder).await?, pid(&waiter).await?);
        // Start before the future is polled, so both its overall deadline and
        // the later PostgreSQL statement deadline must still be unexpired.
        let started = std::time::Instant::now();
        let pending = acquire(
            waiter,
            &a,
            "private",
            "track",
            "replacement",
            now as u64 + 1,
            now as u64 + 46,
            true,
        );
        tokio::pin!(pending);
        observe_blocked(db, holder_pid, waiter_pid, pending.as_mut()).await?;
        let unrelated = acquire(
            connection().await?,
            &b,
            "private",
            "track",
            "replacement",
            now as u64 + 1,
            now as u64 + 46,
            true,
        )
        .await
        .map_err(|_| "independent acquisition failed")?;
        if unrelated != (Acquired { epoch: 2 }, true) {
            return Err(format!("incorrect independent acquisition: {unrelated:?}"));
        }
        holder
            .batch_execute("COMMIT")
            .await
            .map_err(|e| e.to_string())?;
        if pending.await.is_ok() {
            return Err("deleting account acquired lease".into());
        }
        if started.elapsed() >= Duration::from_millis(nntune_api::vault::TOTAL_TIMEOUT_MS) {
            return Err("deleting account rejection may have been a timeout".into());
        }
        let after_lease = db.query_one("SELECT session_id,track_id,play_id,epoch,expires_at,updated_at FROM public.playback_leases WHERE user_id=$1", &[&a]).await.map_err(|e| e.to_string())?;
        let after_mark = db.query_one("SELECT play_key,marked_at FROM public.private_plays WHERE user_id=$1", &[&a]).await.map_err(|e| e.to_string())?;
        let current_lease: (String, String, String, i64, i64, i64) = (after_lease.get(0), after_lease.get(1), after_lease.get(2), after_lease.get(3), after_lease.get(4), after_lease.get(5));
        let current_mark: (String, i64) = (after_mark.get(0), after_mark.get(1));
        if current_lease != lease_snapshot || current_mark != mark_snapshot { return Err("deletion changed incumbent or retained mark".into()); }
        let rows = db
            .query(
                "SELECT user_id,play_id,epoch FROM public.playback_leases WHERE user_id=ANY($1)",
                &[&vec![&a, &b]],
            )
            .await
            .map_err(|e| e.to_string())?;
        let leases: Vec<(String, String, i64)> = rows
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2)))
            .collect();
        if leases.len() != 2
            || !leases.contains(&(a.clone(), "old".into(), 1))
            || !leases.contains(&(b.clone(), "replacement".into(), 2))
        {
            return Err("incumbent or independent lease changed".into());
        }
        let rows = db
            .query(
                "SELECT user_id,play_key FROM public.private_plays WHERE user_id=ANY($1)",
                &[&vec![&a, &b]],
            )
            .await
            .map_err(|e| e.to_string())?;
        let marks: Vec<(String, String)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
        let expected = [
            (a.clone(), format!("play:{a}:track:old")),
            (b.clone(), format!("play:{b}:track:old")),
            (b.clone(), format!("play:{b}:track:replacement")),
        ];
        if marks.len() != expected.len() || !expected.iter().all(|m| marks.contains(m)) {
            return Err("incorrect retained marks".into());
        }
        Ok(())
    })
    .await
    .unwrap_or_else(|_| Err("deletion race timed out".into()));
    let _ = holder.batch_execute("ROLLBACK").await;
    drop(holder);
    accounts.cleanup().await;
    result.unwrap();
}
