//! Disposable-PostgreSQL controls for private acquisition through Store.
use super::playback_dispatch::{playback_store, with_playback_account, TRACK};
use super::{dispatch, disposable_accounts};
use nntune_api::handlers::{Store, StoreError};
use nntune_api::ledger::{derive_play_idempotency_key, period_month_for_timestamp};
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

#[tokio::test]
async fn dispatched_private_delivery_keeps_history_and_billing_distinct() {
    with_playback_account(async |fx| {
        let store = playback_store();
        let call = async |method, path, body| {
            let request = fx.request(method, path, body);
            let response = dispatch(&store, &fx.cfg, fx.now, &request).await;
            assert_eq!(response.status, 200, "{method} {path}");
            serde_json::from_str::<Value>(&response.body).unwrap()
        };
        let play = async |id: &Value| {
            let issued = call("POST", "/v1/plays", json!({"track_id": TRACK, "session_id": "player", "play_id": id})).await;
            assert!(issued["grant"].as_str().is_some_and(|grant| !grant.is_empty()));
            issued
        };
        let toggle = async |enabled| call("PUT", "/v1/me/private-session", json!({"session_id": "player", "enabled": enabled})).await;
        let key = |issued: &Value| derive_play_idempotency_key(fx.user, TRACK, issued["play_id"].as_str().unwrap()).unwrap();
        let marked = async |key: &str| store.private_play_marked(fx.user, TRACK, key).await.unwrap();
        let deliver = async |issued: &Value| {
            let batch = fx.delivered_batch(issued);
            let ack = dispatch(&store, &fx.cfg, fx.now, &batch).await;
            assert_eq!(ack.status, 200);
            assert_eq!(serde_json::from_str::<Value>(&ack.body).unwrap(), json!({"accepted": [issued["play_id"]], "fenced": [], "rejected": []}));
        };
        let same_lease = |start: &Value, resumed: Value| {
            assert_eq!(resumed["play_id"], start["play_id"]);
            assert_eq!(resumed["lease_epoch"], start["lease_epoch"]);
        };

        let public = play(&Value::Null).await;
        toggle(true).await;
        same_lease(&public, play(&public["play_id"]).await);
        assert!(!marked(&key(&public)).await);
        deliver(&public).await;
        let history = async || call("GET", "/v1/me/history", Value::Null).await;
        let public_history = json!({"items": [{
            "play_id": public["play_id"], "track_id": TRACK, "played_at": fx.now, "stream_seconds": 2,
        }]});
        assert_eq!(history().await, public_history, "public play remains visible in private mode");

        let private = play(&Value::Null).await;
        let private_key = key(&private);
        assert!(marked(&private_key).await, "mark must commit before delivery");
        fx.observer.execute("UPDATE public.private_sessions SET expires_at=$2 WHERE user_id=$1 AND session_id='player'", &[&fx.user, &(fx.now as i64 - 1)]).await.unwrap();
        same_lease(&private, play(&private["play_id"]).await);
        toggle(true).await;
        toggle(false).await;
        same_lease(&private, play(&private["play_id"]).await);
        assert!(marked(&private_key).await, "expiry and disable must retain the mark");
        deliver(&private).await;

        let grant_only = play(&Value::Null).await;
        assert_ne!(grant_only["play_id"], public["play_id"]);
        assert!(!marked(&key(&grant_only)).await);
        let rows = fx.observer.query("SELECT idempotency_key, bytes_served, stream_seconds FROM public.stream_play_ledger WHERE user_id=$1", &[&fx.user]).await.unwrap();
        assert_eq!(rows.len(), 2, "grant-only play must not create a ledger row");
        let private_row = rows.iter().find(|row| row.get::<_, &str>(0) == private_key).unwrap();
        assert_eq!((private_row.get::<_, i64>(1), private_row.get::<_, i32>(2)), (80_000, 2));
        assert_eq!(store.monthly_stream_seconds(fx.user, &period_month_for_timestamp(fx.now)).await.unwrap(), 4);
        let usage = call("GET", "/v1/me/usage", Value::Null).await;
        assert_eq!(usage["streamSecondsMonth"], json!(4));
        assert_eq!(history().await, public_history, "private delivery and grant-only plays stay hidden");
    }).await;
}

#[tokio::test]
async fn dispatched_acquisition_faults_withhold_grant_and_distinguish_commit_outcomes() {
    use super::commit_ack_fault::{CommitAckFault, CommitOutcome};
    use super::playback_dispatch::playback_client;
    use futures_util::FutureExt;
    use nntune_api::worker_entry::playback_store_for_postgres_test;
    use std::panic::{resume_unwind, AssertUnwindSafe};
    use tokio::time::timeout;

    with_playback_account(async |fx| {
        let store = playback_store();
        let incumbent = fx.request("POST", "/v1/plays", json!({"track_id": TRACK, "session_id": "incumbent"}));
        let issued = dispatch(&store, &fx.cfg, fx.now, &incumbent).await;
        assert_eq!(issued.status, 200);
        let issued: Value = serde_json::from_str(&issued.body).unwrap();
        let toggle = fx.request("PUT", "/v1/me/private-session", json!({"session_id": "replacement", "enabled": true}));
        assert_eq!(dispatch(&store, &fx.cfg, fx.now, &toggle).await.status, 200);

        let replacement = |track, takeover| fx.request("POST", "/v1/plays", json!({"track_id": track, "session_id": "replacement", "takeover": takeover}));
        let no_marks = async || assert!(fx.observer.query("SELECT 1 FROM public.private_plays WHERE user_id=$1", &[&fx.user]).await.unwrap().is_empty());
        assert_eq!(dispatch(&store, &fx.cfg, fx.now, &replacement(TRACK, false)).await.status, 429);
        no_marks().await;

        let takeover = async |track, client, now| {
            let client = Rc::new(Cell::new(Some(client)));
            let injected_client = Rc::clone(&client);
            let checkouts = Cell::new(0);
            let faulted = playback_store_for_postgres_test(move || {
                let nth = checkouts.get() + 1;
                checkouts.set(nth);
                // Session, deletion, holder, subscription, caps and usage precede acquisition.
                let injected = (nth == 7).then(|| injected_client.take().unwrap());
                async { match injected { Some(client) => Ok(client), None => playback_client().await } }
            });
            let response = dispatch(&faulted, &fx.cfg, now, &replacement(track, true)).await;
            assert!(client.take().is_none(), "acquisition never checked out the faulted connection");
            assert_eq!(response.status, 503, "{track}");
            assert!(serde_json::from_str::<Value>(&response.body).unwrap().get("grant").is_none());
        };

        // Fault only this fixture's prospective marks; server-minted play IDs need not be known.
        let tag = format!("df_{}", fx.user.replace('-', ""));
        let rejected = timeout(Duration::from_secs(30), AssertUnwindSafe(async {
            fx.observer.batch_execute(&format!("
                CREATE ROLE {tag} NOLOGIN; GRANT USAGE ON SCHEMA public TO {tag}; GRANT SELECT, UPDATE ON public.users TO {tag};
                CREATE FUNCTION public.{tag}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic privacy fault'; END $$;
                CREATE TRIGGER {tag} BEFORE INSERT ON public.private_plays FOR EACH ROW
                WHEN (NEW.user_id = '{}' AND split_part(NEW.play_key, ':', 3) = 'mark-fault') EXECUTE FUNCTION public.{tag}();
                CREATE CONSTRAINT TRIGGER {tag}_commit AFTER INSERT ON public.private_plays DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
                WHEN (NEW.user_id = '{}' AND split_part(NEW.play_key, ':', 3) = 'commit-fault') EXECUTE FUNCTION public.{tag}();",
                fx.user, fx.user)).await.unwrap();
            let beat = fx.request("POST", "/v1/plays/heartbeat", json!({"session_id": "incumbent", "play_id": issued["play_id"], "lease_epoch": issued["lease_epoch"]}));
            for (offset, track, expected) in [(1, "lookup-fault", CommitOutcome::Unproven), (3, "mark-fault", CommitOutcome::Unproven), (5, "commit-fault", CommitOutcome::Rejected)] {
                let now = fx.now + offset;
                let (client, fault) = CommitAckFault::start().await.unwrap();
                if track == "lookup-fault" {
                    client.batch_execute(&format!("SET ROLE {tag}")).await.unwrap();
                    let probe = client.query(nntune_api::preference_rows::private_session_active_statement(), &[&fx.user, &"replacement"]).await.unwrap_err();
                    assert_eq!(probe.code(), Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE));
                }
                takeover(track, client, now).await;
                assert_eq!(fault.finish().await.unwrap(), expected, "{track}");
                no_marks().await;
                let response = dispatch(&store, &fx.cfg, now + 1, &beat).await;
                assert_eq!(response.status, 200);
                assert_eq!(serde_json::from_str::<Value>(&response.body).unwrap(), json!({"lease": "active", "lease_epoch": issued["lease_epoch"], "lease_expires_at": now + 1 + nntune_api::playback::LEASE_TTL_SECS}));
            }
        }).catch_unwind()).await;
        timeout(Duration::from_secs(5), fx.observer.batch_execute(&format!(
            "DROP TRIGGER IF EXISTS {tag} ON public.private_plays; DROP TRIGGER IF EXISTS {tag}_commit ON public.private_plays; DROP FUNCTION IF EXISTS public.{tag}(); \
             DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{tag}') THEN DROP OWNED BY {tag}; DROP ROLE {tag}; END IF; END $$"
        ))).await.unwrap().unwrap();
        if let Err(panic) = rejected.expect("rollback cases timed out") {
            resume_unwind(panic);
        }

        let (client, fault) = CommitAckFault::start().await.unwrap();
        let now = fx.now + 7;
        takeover(TRACK, client, now).await;
        assert_eq!(fault.finish().await.unwrap(), CommitOutcome::Withheld);

        let row = fx.observer.query_one(
            "SELECT session_id, track_id, play_id, epoch, expires_at, play_key, marked_at FROM public.playback_leases JOIN public.private_plays USING (user_id) WHERE user_id=$1",
            &[&fx.user],
        ).await.unwrap();
        let play: &str = row.get(2);
        assert_ne!(Some(play), issued["play_id"].as_str());
        assert_eq!((row.get::<_, &str>(0), row.get::<_, &str>(1)), ("replacement", TRACK));
        assert_eq!(row.get::<_, i64>(3), issued["lease_epoch"].as_i64().unwrap() + 1);
        assert_eq!(row.get::<_, i64>(4), (now + nntune_api::playback::LEASE_TTL_SECS) as i64);
        assert_eq!(row.get::<_, &str>(5), derive_play_idempotency_key(fx.user, TRACK, play).unwrap());
        assert_eq!(row.get::<_, i64>(6), now as i64);
    }).await;
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
