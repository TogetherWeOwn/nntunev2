//! Disposable-PostgreSQL controls for private acquisition through Store.
use super::disposable_accounts;
use nntune_api::handlers::{self, Request, Store, StoreError};
use nntune_api::playback::LeaseAcquire::Acquired;
use nntune_api::worker_entry::{
    catalogue_store_for_postgres_test, private_aware_lease_for_postgres_test as acquire,
};
use serde_json::{json, Value};
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

async fn dispatch(
    now: u64,
    connections: usize,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Value,
) -> Result<Value, String> {
    use super::{NoExternalIo, SIGNING_KEY};
    use nntune_api::{meter_sink, worker_entry::request_store_for_postgres_test};
    let mut clients = Vec::new();
    for _ in 0..connections {
        clients.push(connection().await?);
    }
    let store = request_store_for_postgres_test(clients);
    let origin = "https://app.example.test";
    let cfg = handlers::Config::new(
        "app.example.test",
        origin,
        SIGNING_KEY.to_vec(),
        [0; 32],
        1,
        3600,
        false,
        false,
        None,
    )
    .map_err(|_| "test configuration")?
    .with_play_grant_secret(SIGNING_KEY.to_vec())
    .with_meter_sink_secret(SIGNING_KEY.to_vec());
    let ctx = handlers::Ctx {
        cfg: &cfg,
        store: &store,
        mailer: &NoExternalIo,
        dialer: &NoExternalIo,
        dns: &NoExternalIo,
        now,
        source_ip: "127.0.0.1".into(),
        stripe: &nntune_api::stripe::NoStripeTransport,
    };
    let body = body.to_string().into_bytes();
    let authorization = (path == meter_sink::SINK_PATH)
        .then(|| meter_sink::authorization_header(Some(SIGNING_KEY), now, &body))
        .flatten();
    let response = handlers::handle(
        &ctx,
        &Request {
            method: method.into(),
            path: path.into(),
            cookie: cookie.map(str::to_owned),
            origin: Some(origin.into()),
            authorization,
            stripe_signature: None,
            body,
        },
    )
    .await;
    if response.status != 200 {
        return Err(format!("{method} {path}: status {}", response.status));
    }
    serde_json::from_str(&response.body).map_err(|_| format!("{path}: invalid response"))
}

#[tokio::test]
async fn dispatched_private_grant_and_meter_keep_history_and_billing_distinct() {
    use nntune_api::{meter_sink::SINK_PATH, session::SessionClaims};
    let accounts = disposable_accounts::Accounts::new(1).await;
    let (db, user) = (&accounts.observer, accounts.id(0));
    let result: Result<(), String> = tokio::time::timeout(Duration::from_secs(30), async {
        let now: i64 = db.query_one("SELECT floor(extract(epoch FROM clock_timestamp()))::bigint", &[]).await.map_err(|e| e.to_string())?.get(0);
        let sid = format!("session-{user}");
        db.execute("INSERT INTO public.sessions(id,user_id,created_at,expires_at) VALUES($1,$2,$3,$4)", &[&sid, &user, &now, &(now + 3_600)]).await.map_err(|e| e.to_string())?;
        db.execute("INSERT INTO public.subscriptions (user_id,stripe_subscription_id,stripe_customer_id,stripe_subscription_item_id,stripe_price_id,plan_id,status,paid_through_at) VALUES($1,$1,$1,$1,'test','plus','active',now()+interval '1 hour')", &[&user]).await.map_err(|e| e.to_string())?;
        let now = now as u64;
        let token = super::token(&SessionClaims { session_id: sid, user_id: user.into(), created_at: now, issued_at: now, expires_at: now + 3_600, rotation_counter: 0 });
        let cookie = format!("{}={token}", super::COOKIE_NAME);
        let play = |id: Value| dispatch(now, if id.is_null() { 7 } else { 4 }, "POST", "/v1/plays", Some(&cookie), json!({"track_id":"track","session_id":"client","play_id":id}));
        let toggle = |enabled| dispatch(now, 3, "PUT", "/v1/me/private-session", Some(&cookie), json!({"session_id":"client","enabled":enabled}));
        let public = play(Value::Null).await?;
        toggle(true).await?;
        let public_resumed = play(public["play_id"].clone()).await?;
        let private = play(Value::Null).await?;
        db.execute("UPDATE public.private_sessions SET expires_at=$2 WHERE user_id=$1 AND session_id='client'", &[&user, &(now as i64 - 1)]).await.map_err(|e| e.to_string())?;
        let expired_resumed = play(private["play_id"].clone()).await?;
        toggle(false).await?;
        let disabled_resumed = play(private["play_id"].clone()).await?;
        for (start, resumed) in [(&public, public_resumed), (&private, expired_resumed), (&private, disabled_resumed)] {
            if resumed["play_id"] != start["play_id"] || resumed["lease_epoch"] != start["lease_epoch"] { return Err("continuation changed the holder".into()); }
        }
        let grant_only = play(Value::Null).await?;
        let mut windows = Vec::new();
        for (play, private, bytes) in [(&public, false, 96_000), (&private, true, 48_000), (&grant_only, false, 0)] {
            let id = play["play_id"].as_str().ok_or("missing play id")?;
            if play["grant"].as_str().is_none() { return Err("missing grant".into()); }
            let key = format!("play:{user}:track:{id}");
            if store().await?.private_play_marked(user, "track", &key).await.map_err(|_| "mark read failed")? != private {
                return Err("wrong committed mark before delivery".into());
            }
            if bytes != 0 {
                windows.push(json!({"user_id": user,"track_id":"track","play_id":id,"union_bytes":bytes,"started_at":now}));
            }
        }
        let ack = dispatch(now, 2, "POST", SINK_PATH, None, json!({"windows":windows})).await?;
        if ack != json!({"accepted":[public["play_id"],private["play_id"]],"fenced":[],"rejected":[]}) { return Err("meter did not acknowledge both deliveries".into()); }
        let month = nntune_api::ledger::period_month_for_timestamp(now);
        let key = format!("play:{user}:track:{}", private["play_id"].as_str().ok_or("missing private id")?);
        let row = db.query_one("SELECT bytes_served, stream_seconds FROM public.stream_play_ledger WHERE idempotency_key=$1", &[&key]).await.map_err(|e| e.to_string())?;
        if row.get::<_, i64>(0) != 48_000 || row.get::<_, i32>(1) != 1 || store().await?.monthly_stream_seconds(user, &month).await.map_err(|_| "usage read failed")? != 3 {
            return Err("private delivery was not billed".into());
        }
        let history = store().await?.history_list(user).await.map_err(|_| "history read failed")?;
        if history.len() != 1 || Some(history[0].play_id.as_str()) != public["play_id"].as_str() { return Err("private or grant-only play leaked into history".into()); }
        Ok(())
    }).await.unwrap_or_else(|_| Err("dispatched privacy/meter test timed out".into()));
    accounts.cleanup().await;
    result.unwrap();
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
            if !matches!(outcome, Acquired { .. }) {
                return Err(format!("incorrect acquisition: {outcome:?}"));
            }
            let key = format!("play:{user}:track:{play}");
            if store().await?.private_play_marked(user, "track", &key).await.map_err(|_| "mark probe failed")? != private {
                return Err(format!("incorrect classification for {play}"));
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
