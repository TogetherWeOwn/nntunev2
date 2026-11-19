//! Disposable-PostgreSQL controls for the unwired private acquisition.
use super::disposable_accounts;
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
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let ids = vec![
        format!("safety_{}_{}_a", std::process::id(), nonce),
        format!("safety_{}_{}_b", std::process::id(), nonce),
    ];
    // No fixture writes occur until this bounded connection setup succeeds.
    let db = tokio::time::timeout(Duration::from_secs(5), connection())
        .await
        .expect("fixture connection timed out")
        .expect("fixture connection failed");
    let result: Result<(), String> = tokio::time::timeout(Duration::from_secs(20), async {
        disposable_accounts::Accounts::insert_named(&db, &ids).await.map_err(|e| e.to_string())?;
        let (a, b) = (ids[0].clone(), ids[1].clone());
        let db = &db;
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
        let key = format!("play:{a}:track:secret");
        let boundary = db.query_one(
            "SELECT EXISTS (SELECT 1 FROM public.private_plays WHERE user_id=$1 AND play_key=$2), EXISTS (SELECT 1 FROM public.stream_play_ledger WHERE idempotency_key=$2), extract(epoch from clock_timestamp())::bigint, (SELECT marked_at FROM public.private_plays WHERE user_id=$1 AND play_key=$2)",
            &[&a, &key],
        ).await.map_err(|e| e.to_string())?;
        if !boundary.get::<_, bool>(0) || boundary.get::<_, bool>(1) {
            return Err("private mark not committed before metering".into());
        }
        // Use a strictly later timestamp even when acquisition and delivery share a second.
        let at = (boundary.get::<_, i64>(2).max(boundary.get::<_, i64>(3)) + 1) as u64;
        let month = nntune_api::ledger::period_month_for_timestamp(at);
        // Only the first, second and foreign grants receive metered bytes.
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
        let billed = db.query_one(
            "SELECT bytes_served, stream_seconds::bigint FROM public.stream_play_ledger WHERE idempotency_key=$1 AND user_id=$2 AND period_month=$3",
            &[&key, &a, &month],
        ).await.map_err(|e| e.to_string())?;
        if billed.get::<_, i64>(0) != 100_000 || billed.get::<_, i64>(1) != 10 {
            return Err("private play ledger values incorrect".into());
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
            .monthly_stream_seconds(&a, &month)
            .await
            .map_err(|_| "usage failed")?;
        let row = connection().await?
            .query_one(
                nntune_api::ledger::ledger_monthly_total_statement(),
                &[&a, &month],
            )
            .await
            .map_err(|e| e.to_string())?;
        if usage != 30 || row.get::<_, i64>(0) != 30 || row.get::<_, i64>(1) != 300_000 {
            return Err("private and public delivery not both metered".into());
        }
        // A legacy row can contain seconds but no bytes; it is not history.
        let zero = format!("play:{a}:track:zero");
        db.execute("INSERT INTO public.stream_play_ledger (idempotency_key,user_id,track_id,period_month,stream_seconds,bytes_served,recorded_at) VALUES ($1,$2,'track',$3,1,0,$4)", &[&zero, &a, &month, &(at as i64)]).await.map_err(|e| e.to_string())?;
        let history = store().await?.history_list(&a).await.map_err(|_| "zero-byte history failed")?;
        if history.len() != 1 || history[0].play_id != "visible" { return Err("zero-byte row appeared in history".into()); }
        Ok(())
    })
    .await
    .unwrap_or_else(|_| Err("history/meter test timed out".into()));
    // The same connection orders this DELETE after a timed-out in-flight INSERT.
    let cleanup = tokio::time::timeout(
        Duration::from_secs(5),
        disposable_accounts::Accounts::cleanup_named(&db, &ids),
    )
    .await;
    assert!(
        matches!(cleanup, Ok(Ok(()))),
        "bounded account cleanup failed: {cleanup:?}"
    );
    result.unwrap();
}
