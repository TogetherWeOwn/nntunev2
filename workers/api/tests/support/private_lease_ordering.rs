//! Lock-observed ordering of the unwired acquisition and session writers.
use super::{blocker_observer::observe_blocked, disposable_accounts};
use nntune_api::handlers::{Store, StoreError};
use nntune_api::playback::LeaseAcquire::Acquired;
use nntune_api::worker_entry::{
    catalogue_store_for_postgres_test, private_aware_lease_for_postgres_test as acquire,
    SubsonicSnapshotCatalogueStore,
};
use std::{cell::Cell, rc::Rc, time::Duration};
use tokio_postgres::Client;

async fn pid(client: &Client) -> Result<i32, String> {
    Ok(client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .map_err(|e| e.to_string())?
        .get(0))
}

async fn ordering_case(enable: bool, toggle_first: bool) -> Result<(), String> {
    let accounts = disposable_accounts::Accounts::new(1).await;
    let user = accounts.id(0).to_owned();
    let db = &accounts.observer;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let now: i64 = db.query_one("SELECT extract(epoch from statement_timestamp())::bigint", &[]).await.map_err(|e| e.to_string())?.get(0);
        if !enable {
            db.execute("INSERT INTO public.private_sessions VALUES ($1,'session',$2)", &[&user, &(now + 300)]).await.map_err(|e| e.to_string())?;
        }
        let seed = acquire(disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?, &user, "session", "track", "seed", now as u64, now as u64 + 45, false).await.map_err(|_| "seed acquisition failed")?;
        if seed != (Acquired { epoch: 1 }, !enable) { return Err(format!("wrong seed: {seed:?}")); }

        let holder = disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?;
        let acq_client = disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?;
        let toggle_client = disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?;
        let (holder_pid, acq_pid, toggle_pid) = (pid(&holder).await?, pid(&acq_client).await?, pid(&toggle_client).await?);
        let store = catalogue_store_for_postgres_test(toggle_client, false, Err(StoreError), Rc::new(Cell::new(0)));
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        if toggle_first {
            let phase = if enable { "private-session-upsert" } else { "private-session-delete" };
            store.set_preflight_hook(phase, Box::new(move || Box::pin(async move {
                let _ = reached_tx.send(());
                release_rx.await.map_err(|_| StoreError)
            })));
        }
        let toggle = async {
            if enable { store.upsert_private_session(&user, "session", (now + 300) as u64).await }
            else { store.delete_private_session(&user, "session").await }
        };
        tokio::pin!(toggle);
        let acquisition = acquire(acq_client, &user, "session", "track", "next", now as u64 + 1, now as u64 + 46, true);
        tokio::pin!(acquisition);

        if toggle_first {
            tokio::select! {
                result = &mut toggle => return Err(format!("toggle finished before barrier: {result:?}")),
                reached = reached_rx => reached.map_err(|_| "toggle did not reach barrier")?,
            }
            observe_blocked(db, toggle_pid, acq_pid, acquisition.as_mut()).await?;
            release_tx.send(()).map_err(|_| "toggle barrier closed")?;
        } else {
            holder.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
            holder.query_one("SELECT user_id FROM public.playback_leases WHERE user_id=$1 FOR UPDATE", &[&user]).await.map_err(|e| e.to_string())?;
            observe_blocked(db, holder_pid, acq_pid, acquisition.as_mut()).await?;
            observe_blocked(db, acq_pid, toggle_pid, toggle.as_mut()).await?;
            holder.batch_execute("ROLLBACK").await.map_err(|e| e.to_string())?;
        }
        let (outcome, mutation) = tokio::join!(acquisition, toggle);
        mutation.map_err(|_| "toggle failed")?;
        let outcome = outcome.map_err(|_| "acquisition failed")?;
        let expected_private = if toggle_first { enable } else { !enable };
        if outcome != (Acquired { epoch: 2 }, expected_private) { return Err(format!("wrong ordering: {outcome:?}")); }
        // Toggle after the grant, then exercise the existing continuation touch.
        if toggle_first {
            let later = catalogue_store_for_postgres_test(disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?, false, Err(StoreError), Rc::new(Cell::new(0)));
            if enable { later.delete_private_session(&user, "session").await.map_err(|_| "later disable failed")?; }
            else { later.upsert_private_session(&user, "session", (now + 300) as u64).await.map_err(|_| "later enable failed")?; }
        }
        let touch = catalogue_store_for_postgres_test(disposable_accounts::connect_guarded().await.map_err(|e| e.to_string())?, false, Err(StoreError), Rc::new(Cell::new(0)));
        if !touch.touch_playback_lease(&user, "session", "next", 2, now as u64 + 2, now as u64 + 60).await.map_err(|_| "continuation touch failed")? { return Err("continuation lost lease".into()); }
        let rows = db.query("SELECT play_key FROM public.private_plays WHERE user_id=$1 ORDER BY play_key", &[&user]).await.map_err(|e| e.to_string())?;
        let marks: Vec<String> = rows.iter().map(|row| row.get(0)).collect();
        let mut expected = Vec::new();
        if !enable { expected.push(format!("play:{user}:track:seed")); }
        if expected_private { expected.push(format!("play:{user}:track:next")); }
        expected.sort();
        if marks != expected { return Err(format!("wrong retained marks: {marks:?}")); }
        let row = db.query_one("SELECT session_id,track_id,play_id,epoch FROM public.playback_leases WHERE user_id=$1", &[&user]).await.map_err(|e| e.to_string())?;
        if (row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, String>(2), row.get::<_, i64>(3)) != ("session".into(), "track".into(), "next".into(), 2) { return Err("lease changed unexpectedly".into()); }
        Ok(())
    }).await.unwrap_or_else(|_| Err("ordering case timed out".into()));
    accounts.cleanup().await;
    result
}

#[tokio::test]
async fn toggle_first_classifies_after_committed_enable_and_disable() {
    for enable in [true, false] {
        ordering_case(enable, true).await.unwrap();
    }
}

#[tokio::test]
async fn acquisition_first_classifies_before_enable_and_disable() {
    for enable in [true, false] {
        ordering_case(enable, false).await.unwrap();
    }
}
