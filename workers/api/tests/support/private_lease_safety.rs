//! Disposable-PostgreSQL controls for the unwired private acquisition.
use super::{blocker_observer::observe_blocked, disposable_accounts};
use nntune_api::handlers::{Store, StoreError};
use nntune_api::playback::LeaseAcquire::Acquired;
use nntune_api::worker_entry::{
    catalogue_store_for_postgres_test, private_aware_lease_for_postgres_test as acquire,
};
use std::{cell::Cell, rc::Rc, time::Duration};
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

async fn store() -> Result<impl Store, String> {
    Ok(catalogue_store_for_postgres_test(
        connection().await?,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    ))
}

#[tokio::test]
async fn private_delivery_is_hidden_from_history_not_metering() {
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
            "INSERT INTO public.private_sessions VALUES ($1,'private',$2)",
            &[&a, &(now + 300)],
        )
        .await
        .map_err(|e| e.to_string())?;
        for (user, session, play, private) in [
            (&a, "private", "secret", true),
            (&a, "public", "visible", false),
            (&a, "public", "grant", false),
            (&b, "public", "other", false),
        ] {
            let outcome = acquire(
                connection().await?,
                user,
                session,
                "track",
                play,
                now as u64,
                now as u64 + 45,
                true,
            )
            .await
            .map_err(|_| "acquisition failed")?;
            if !matches!(outcome.0, Acquired { .. }) || outcome.1 != private {
                return Err(format!("incorrect classification: {outcome:?}"));
            }
        }
        // Only the first, second and foreign grants receive metered bytes.
        let at = 1_793_491_200;
        for (user, play, bytes) in [
            (&a, "secret", 100_000),
            (&a, "visible", 200_000),
            (&b, "other", 50_000),
        ] {
            let window = nntune_api::ledger::prepare_play_window(
                user,
                "track",
                play,
                bytes,
                Some(1_000_000),
                Some(100_000),
                at,
            )
            .map_err(|e| format!("window: {e:?}"))?;
            store()
                .await?
                .record_play_window(&window)
                .await
                .map_err(|_| "meter write failed")?;
        }
        // Zero-byte activity cannot produce an authoritative meter window.
        if nntune_api::ledger::prepare_play_window(&a, "track", "zero", 0, None, None, at).is_ok() {
            return Err("zero-byte window accepted".into());
        }
        let history = store()
            .await?
            .history_list(&a)
            .await
            .map_err(|_| "history failed")?;
        if history.len() != 1 || history[0].play_id != "visible" {
            return Err(format!("wrong history: {history:?}"));
        }
        let other = store()
            .await?
            .history_list(&b)
            .await
            .map_err(|_| "other history failed")?;
        if other.len() != 1 || other[0].play_id != "other" {
            return Err("foreign history changed".into());
        }
        let usage = store()
            .await?
            .monthly_stream_seconds(&a, "2026-11")
            .await
            .map_err(|_| "usage failed")?;
        let row = connection().await?
            .query_one(
                nntune_api::ledger::ledger_monthly_total_statement(),
                &[&a, &"2026-11"],
            )
            .await
            .map_err(|e| e.to_string())?;
        if usage != 30 || row.get::<_, i64>(0) != 30 || row.get::<_, i64>(1) != 300_000 {
            return Err("private and public delivery not both metered".into());
        }
        // A legacy row can contain seconds but no bytes; it is not history.
        let zero = format!("play:{a}:track:zero");
        db.execute("INSERT INTO public.stream_play_ledger (idempotency_key,user_id,track_id,period_month,stream_seconds,bytes_served,recorded_at) VALUES ($1,$2,'track','2026-11',1,0,$3)", &[&zero, &a, &(at as i64)]).await.map_err(|e| e.to_string())?;
        let history = store().await?.history_list(&a).await.map_err(|_| "zero-byte history failed")?;
        if history.len() != 1 || history[0].play_id != "visible" { return Err("zero-byte row appeared in history".into()); }
        Ok(())
    })
    .await
    .unwrap_or_else(|_| Err("history/meter test timed out".into()));
    accounts.cleanup().await;
    result.unwrap();
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
