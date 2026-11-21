//! Production catalogue reads with only connection acquisition injected.
//! CI's disposable PostgreSQL owns the app rows; no mirror service is required.
use nntune_api::catalogue::{assemble_library_rows, LibraryPageRow};
use nntune_api::handlers::{Store, StoreError};
use nntune_api::worker_entry::{catalogue_store_for_postgres_test, SubsonicSnapshotCatalogueStore};
use std::cell::Cell;
use std::rc::Rc;
use tokio_postgres::{Client, Config, NoTls};

#[path = "support/blocker_observer.rs"]
mod blocker_observer;
#[path = "support/disposable_accounts.rs"]
mod disposable_accounts;

const RELEASE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const RECORDING: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const OTHER_RELEASE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

async fn client(populated: bool) -> Client {
    let client = disposable_accounts::connect().await;
    client.batch_execute(
        "CREATE TEMP TABLE library (user_id TEXT, job_id TEXT, release_mbid TEXT, recording_mbids TEXT[], idempotency_key TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text)",
    ).await.unwrap();
    if populated {
        // Duplicate recordings and malformed stored MBIDs must survive library
        // degradation. The other user's row must never become membership.
        client
            .execute(
                "INSERT INTO library VALUES ('u1','job-1',$1,ARRAY[$2,$2,'stored-invalid']),
             ('u1','job-0',NULL,ARRAY[]::text[]), ('other','private',$3,ARRAY[$2])",
                &[&RELEASE, &RECORDING, &OTHER_RELEASE],
            )
            .await
            .unwrap();
    }
    client
}

async fn private_client() -> Result<Client, String> {
    let client = disposable_accounts::connect_guarded()
        .await
        .map_err(|e| e.to_string())?;
    client
        .batch_execute("SET search_path = pg_temp, public")
        .await
        .map_err(|e| e.to_string())?;
    Ok(client)
}

async fn store(populated: bool, configured: bool) -> (impl Store, Rc<Cell<usize>>) {
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(
        client(populated).await,
        configured,
        Err(StoreError),
        attempts.clone(),
    );
    (store, attempts)
}

#[tokio::test]
async fn metered_play_adapter_writes_updates_and_respects_fence() {
    use nntune_api::ledger::prepare_play_window;
    use nntune_api::meter_sink::PlayWindowOutcome;

    let accounts = disposable_accounts::Accounts::new(1).await;
    let owner = accounts.id(0).to_owned();
    let db = &accounts.observer;
    let result: Result<(), String> = async {
        let fresh = || async {
            Ok::<_, String>(catalogue_store_for_postgres_test(
                private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)),
            ))
        };
        let window = prepare_play_window(&owner, "track", "play", 48_000, None, None, 1)
            .map_err(|_| "prepare")?;
        if fresh().await?.record_play_window(&window).await.map_err(|_| "insert")? != PlayWindowOutcome::Written {
            return Err("insert not written".into());
        }
        if fresh().await?.record_play_window(&window).await.map_err(|_| "replay")? != PlayWindowOutcome::Covered {
            return Err("replay not covered".into());
        }
        let larger = prepare_play_window(&owner, "track", "play", 96_000, None, None, 1)
            .map_err(|_| "prepare larger")?;
        if fresh().await?.record_play_window(&larger).await.map_err(|_| "update")? != PlayWindowOutcome::Written {
            return Err("larger union not written".into());
        }
        let row = db.query_one("SELECT stream_seconds, bytes_served FROM public.stream_play_ledger WHERE idempotency_key=$1", &[&window.idempotency_key]).await.map_err(|e| e.to_string())?;
        if (row.get::<_, i32>(0), row.get::<_, i64>(1)) != (2, 96_000) {
            return Err("wrong persisted metering".into());
        }
        db.execute("UPDATE public.users SET deleting_at=now() WHERE id=$1", &[&owner]).await.map_err(|e| e.to_string())?;
        let fenced = prepare_play_window(&owner, "track", "newplay", 48_000, None, None, 1)
            .map_err(|_| "prepare fenced")?;
        if fresh().await?.record_play_window(&fenced).await.map_err(|_| "fence")? != PlayWindowOutcome::Fenced {
            return Err("deleting account was not fenced".into());
        }
        let count: i64 = db.query_one("SELECT count(*) FROM public.stream_play_ledger WHERE idempotency_key=$1", &[&fenced.idempotency_key]).await.map_err(|e| e.to_string())?.get(0);
        if count != 0 { return Err("fenced write persisted".into()); }
        Ok(())
    }.await;
    accounts.cleanup().await;
    result.unwrap();
}

#[tokio::test]
async fn history_reader_decodes_metered_public_owner_rows() {
    let accounts = disposable_accounts::Accounts::new(2).await;
    let owner = accounts.id(0).to_owned();
    let other = accounts.id(1).to_owned();
    let db = &accounts.observer;
    let result: Result<(), String> = async {
        let insert = "INSERT INTO public.stream_play_ledger (idempotency_key,user_id,track_id,period_month,stream_seconds,bytes_served,recorded_at) VALUES ($1,$2,$3,'2026-11',$4,$5,$6)";
        for (key, user, track, seconds, bytes, time) in [
            (format!("play:{owner}:track:public"), &owner, "track", 12_i32, 100_i64, 10_i64),
            (format!("play:{owner}:track:private"), &owner, "track", 2, 100, 20),
            (format!("play:{owner}:track:zero"), &owner, "track", 1, 0, 30),
            (format!("play:{other}:track:foreign"), &other, "track", 3, 100, 40),
            (format!("play:{other}:track:forged"), &owner, "track", 4, 100, 50),
            (format!("play:{owner}:wrong:mismatch"), &owner, "track", 4, 100, 60),
            (format!("play:{owner}:track:bad:extra"), &owner, "track", 4, 100, 70),
            (format!("request:{owner}:track:search"), &owner, "track", 4, 100, 80),
        ] {
            db.execute(insert, &[&key, user, &track, &seconds, &bytes, &time]).await.map_err(|e| e.to_string())?;
        }
        let public = format!("play:{owner}:track:public");
        db.execute(&format!("{insert} ON CONFLICT (idempotency_key) DO NOTHING"), &[&public, &owner, &"track", &12_i32, &100_i64, &10_i64]).await.map_err(|e| e.to_string())?;
        let private = format!("play:{owner}:track:private");
        db.execute("INSERT INTO public.private_plays (user_id,play_key,marked_at) VALUES ($1,$2,1)", &[&owner, &private]).await.map_err(|e| e.to_string())?;
        let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
        let rows = store.history_list(&owner).await.map_err(|_| "read failed")?;
        if rows.len() != 1 || rows[0].play_id != "public" || rows[0].track_id != "track" || rows[0].played_at != 10 || rows[0].stream_seconds != 12 {
            return Err(format!("incorrect owner history: {rows:?}"));
        }
        for i in 0..55_i64 {
            let key = format!("play:{owner}:track:p{i:02}");
            db.execute(insert, &[&key, &owner, &"track", &1_i32, &1_i64, &(100 + i)]).await.map_err(|e| e.to_string())?;
        }
        let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
        let rows = store.history_list(&owner).await.map_err(|_| "bounded read failed")?;
        if rows.len() != 50 || rows[0].play_id != "p54" || rows[49].play_id != "p05" {
            return Err("history order or cap incorrect".into());
        }
        db.execute("DELETE FROM public.users WHERE id = $1", &[&owner]).await.map_err(|e| e.to_string())?;
        let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
        if !store.history_list(&owner).await.map_err(|_| "deleted read failed")?.is_empty() {
            return Err("account cascade failed".into());
        }
        let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
        if store.history_list(&other).await.map_err(|_| "other read failed")?.len() != 1 {
            return Err("surviving owner history incorrect".into());
        }
        if store.history_list(&other).await.is_ok() {
            return Err("connection failure must be opaque".into());
        }
        Ok(())
    }.await;
    accounts.cleanup().await;
    result.unwrap();
}

#[tokio::test]
async fn private_play_writer_is_owner_bound_idempotent_and_atomic() {
    let accounts = disposable_accounts::Accounts::new(2).await;
    let owner = accounts.id(0).to_owned();
    let other = accounts.id(1).to_owned();
    let db = &accounts.observer;
    let result: Result<(), String> = async {
        let fresh = || async { Ok::<_, String>(catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)))) };
        let key = format!("play:{owner}:Track:Play");
        let other_key = format!("play:{other}:Track:Play");
        for at in [0, 22] {
            fresh().await?.mark_private_play(&owner, "Track", &key, at).await.map_err(|_| "mark failed")?;
        }
        fresh().await?.mark_private_play(&other, "Track", &other_key, 253_402_300_799).await.map_err(|_| "upper boundary failed")?;
        let timestamp: i64 = db.query_one("SELECT marked_at FROM public.private_plays WHERE user_id=$1 AND play_key=$2", &[&owner, &key]).await.map_err(|e| e.to_string())?.get(0);
        if timestamp != 0 { return Err("replay changed timestamp".into()); }
        let lower = format!("play:{owner}:Track:play");
        fresh().await?.mark_private_play(&owner, "Track", &lower, 1).await.map_err(|_| "case-sensitive mark failed")?;
        let count: i64 = db.query_one("SELECT count(*) FROM public.private_plays WHERE user_id=$1 AND play_key IN ($2,$3)", &[&owner, &key, &lower]).await.map_err(|e| e.to_string())?.get(0);
        if count != 2 { return Err("distinct case keys collapsed".into()); }
        let missing = format!("{owner}-missing");
        let missing_key = format!("play:{missing}:Track:Play");
        for (id, track, candidate, at) in [(&other, "Track", key.as_str(), 1), (&owner, "track", key.as_str(), 1), (&owner, "Track", "play:bad", 1), (&missing, "Track", missing_key.as_str(), 1), (&owner, "Track", key.as_str(), 253_402_300_800)] {
            if fresh().await?.mark_private_play(id, track, candidate, at).await.is_ok() { return Err("invalid write accepted".into()); }
        }
        let failed_key = format!("play:{owner}:Track:rollback");
        let failing = fresh().await?;
        let reached = Rc::new(Cell::new(false));
        let marker = reached.clone();
        failing.set_preflight_hook("private-play-mark", Box::new(move || Box::pin(async move { marker.set(true); Err(StoreError) })));
        if failing.mark_private_play(&owner, "Track", &failed_key, 1).await.is_ok() || !reached.get() { return Err("rollback hook not reached".into()); }
        let count: i64 = db.query_one("SELECT count(*) FROM public.private_plays WHERE user_id=$1 AND play_key=$2", &[&owner, &failed_key]).await.map_err(|e| e.to_string())?.get(0);
        if count != 0 { return Err("failed write persisted".into()); }
        db.execute("UPDATE public.users SET deleting_at=now() WHERE id=$1", &[&owner]).await.map_err(|e| e.to_string())?;
        let unreachable = fresh().await?;
        let reached = Rc::new(Cell::new(false));
        let marker = reached.clone();
        unreachable.set_preflight_hook("private-play-mark", Box::new(move || Box::pin(async move { marker.set(true); Ok(()) })));
        if unreachable.mark_private_play(&owner, "Track", &key, 1).await.is_ok() || reached.get() { return Err("deleting account reached mutation hook".into()); }
        db.execute("DELETE FROM public.users WHERE id=$1", &[&owner]).await.map_err(|e| e.to_string())?;
        let counts = db.query_one("SELECT count(*) FILTER (WHERE user_id=$1), count(*) FILTER (WHERE user_id=$2) FROM public.private_plays", &[&owner, &other]).await.map_err(|e| e.to_string())?;
        if counts.get::<_, i64>(0) != 0 || counts.get::<_, i64>(1) != 1 { return Err("cascade or owner isolation failed".into()); }
        let failed = private_client().await?;
        failed.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
        let _ = failed.batch_execute("SELECT 1/0").await;
        let broken = catalogue_store_for_postgres_test(failed, false, Err(StoreError), Rc::new(Cell::new(0)));
        if !matches!(broken.mark_private_play(&other, "Track", &other_key, 1).await, Err(StoreError)) { return Err("store failure was not opaque".into()); }
        Ok(())
    }.await;
    accounts.cleanup().await;
    result.unwrap();
}

#[tokio::test]
async fn private_play_writer_serializes_with_account_deletion() {
    let accounts = disposable_accounts::Accounts::new(2).await;
    let owner = accounts.id(0).to_owned();
    let other = accounts.id(1).to_owned();
    let observer = &accounts.observer;
    let locker = client(false).await;
    let result: Result<(), String> = async {
        for deleting in [false, true] {
            locker
                .batch_execute("BEGIN")
                .await
                .map_err(|e| e.to_string())?;
            if deleting {
                locker
                    .execute(
                        "UPDATE public.users SET deleting_at=now() WHERE id=$1",
                        &[&owner],
                    )
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                locker
                    .query_one(
                        "SELECT id FROM public.users WHERE id=$1 FOR UPDATE",
                        &[&owner],
                    )
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let locker_pid: i32 = locker
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .map_err(|e| e.to_string())?
                .get(0);
            let writer = private_client().await?;
            let writer_pid: i32 = writer
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .map_err(|e| e.to_string())?
                .get(0);
            let store = catalogue_store_for_postgres_test(
                writer,
                false,
                Err(StoreError),
                Rc::new(Cell::new(0)),
            );
            let key = format!("play:{owner}:Track:race{deleting}");
            let mark = store.mark_private_play(&owner, "Track", &key, 1);
            tokio::pin!(mark);
            blocker_observer::observe_blocked(observer, locker_pid, writer_pid, mark.as_mut())
                .await?;
            let unrelated = catalogue_store_for_postgres_test(
                private_client().await?,
                false,
                Err(StoreError),
                Rc::new(Cell::new(0)),
            );
            unrelated
                .mark_private_play(&other, "Track", &format!("play:{other}:Track:race"), 1)
                .await
                .map_err(|_| "unrelated writer blocked")?;
            locker
                .batch_execute("COMMIT")
                .await
                .map_err(|e| e.to_string())?;
            if mark.await.is_ok() == deleting {
                return Err("wrong outcome after lock release".into());
            }
            let count: i64 = observer
                .query_one(
                    "SELECT count(*) FROM public.private_plays WHERE user_id=$1 AND play_key=$2",
                    &[&owner, &key],
                )
                .await
                .map_err(|e| e.to_string())?
                .get(0);
            if count != i64::from(!deleting) {
                return Err("wrong persisted race outcome".into());
            }
        }
        Ok(())
    }
    .await;
    let _ = locker.batch_execute("ROLLBACK").await;
    accounts.cleanup().await;
    result.unwrap();
}

#[tokio::test]
async fn private_play_probe_is_owner_bound_and_fail_closed() {
    let accounts = disposable_accounts::Accounts::new(2).await;
    let owner = accounts.id(0).to_owned();
    let other = accounts.id(1).to_owned();
    let db = &accounts.observer;
    let result: Result<(), String> = async {
        let key = format!("play:{owner}:Track:Play");
        let other_key = format!("play:{other}:Track:Play");
        let fresh = || async {
            Ok::<_, String>(catalogue_store_for_postgres_test(
                private_client().await?,
                false,
                Err(StoreError),
                Rc::new(Cell::new(0)),
            ))
        };
        if fresh()
            .await?
            .private_play_marked(&owner, "Track", &key)
            .await
            .map_err(|_| "probe")?
        {
            return Err("missing mark present".into());
        }
        db.execute(
            "INSERT INTO public.private_plays (user_id, play_key, marked_at) VALUES ($1, $2, 11), ($3, $4, 33)",
            &[&owner, &key, &other, &other_key],
        ).await.map_err(|e| e.to_string())?;
        if !fresh().await?.private_play_marked(&owner, "Track", &key).await.map_err(|_| "present probe")? {
            return Err("seeded mark missing".into());
        }
        let missing = format!("{owner}-missing");
        let missing_key = format!("play:{missing}:Track:Play");
        for (id, track, candidate) in [
            (&other, "Track", key.as_str()),
            (&owner, "track", key.as_str()),
            (&owner, "Track", "play:bad"),
            (&missing, "Track", missing_key.as_str()),
        ] {
            if fresh()
                .await?
                .private_play_marked(id, track, candidate)
                .await
                .is_ok()
            {
                return Err("invalid binding accepted".into());
            }
        }
        let lower = format!("play:{owner}:Track:play");
        if fresh()
            .await?
            .private_play_marked(&owner, "Track", &lower)
            .await
            .map_err(|_| "case probe")?
        {
            return Err("case mismatch".into());
        }
        db.execute(
            "UPDATE public.users SET deleting_at = now() WHERE id = $1",
            &[&owner],
        )
        .await
        .map_err(|e| e.to_string())?;
        if fresh()
            .await?
            .private_play_marked(&owner, "Track", &key)
            .await
            .is_ok()
        {
            return Err("deleting account accepted".into());
        }
        db.execute("DELETE FROM public.users WHERE id = $1", &[&owner])
            .await
            .map_err(|e| e.to_string())?;
        if fresh()
            .await?
            .private_play_marked(&owner, "Track", &key)
            .await
            .is_ok()
            || !fresh()
                .await?
                .private_play_marked(&other, "Track", &other_key)
                .await
                .map_err(|_| "survivor probe")?
        {
            return Err("cascade or survivor failed".into());
        }
        let count: i64 = db
            .query_one(
                "SELECT count(*) FROM public.private_plays WHERE user_id=$1",
                &[&owner],
            )
            .await
            .map_err(|e| e.to_string())?
            .get(0);
        if count != 0 {
            return Err("mark did not cascade".into());
        }
        let failed = private_client().await?;
        failed
            .batch_execute("BEGIN")
            .await
            .map_err(|e| e.to_string())?;
        let _ = failed.batch_execute("SELECT 1/0").await;
        let broken = catalogue_store_for_postgres_test(
            failed,
            false,
            Err(StoreError),
            Rc::new(Cell::new(0)),
        );
        if !matches!(
            broken
                .private_play_marked(&other, "Track", &other_key)
                .await,
            Err(StoreError)
        ) {
            return Err("failure not opaque".into());
        }
        Ok(())
    }
    .await;
    accounts.cleanup().await;
    result.unwrap();
}

#[tokio::test]
async fn private_session_active_observes_owner_and_server_time() {
    let accounts = disposable_accounts::Accounts::new(2).await;
    let owner = accounts.id(0).to_owned();
    let other = accounts.id(1).to_owned();
    let observer = &accounts.observer;
    let result: Result<(), String> = async {
        observer.execute("INSERT INTO public.private_sessions VALUES ($1,'Session',253402300799), ($1,'expired',0), ($2,'foreign',253402300799)", &[&owner, &other]).await.map_err(|e| e.to_string())?;
        for (id, session, expected) in [(&owner, "Session", true), (&owner, "session", false), (&owner, "expired", false), (&owner, "absent", false), (&owner, "foreign", false), (&other, "Session", false)] {
            let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
            if store.private_session_active(id, session).await.map_err(|_| "lookup failed")? != expected { return Err("incorrect active-session result".into()); }
        }
        for (id, session) in [(format!("{owner}-missing"), "Session"), (owner.clone(), ""), (owner.clone(), "bad\0id"), (owner.clone(), &"x".repeat(129))] {
            let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
            if store.private_session_active(&id, session).await.is_ok() { return Err("invalid lookup succeeded".into()); }
        }
        observer.execute("UPDATE public.users SET deleting_at = now() WHERE id = $1", &[&owner]).await.map_err(|e| e.to_string())?;
        let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
        if store.private_session_active(&owner, "Session").await.is_ok() { return Err("deleting account accepted".into()); }
        let failed = private_client().await?;
        failed.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
        let _ = failed.batch_execute("SELECT 1/0").await;
        let broken = catalogue_store_for_postgres_test(failed, false, Err(StoreError), Rc::new(Cell::new(0)));
        if !matches!(broken.private_session_active(&other, "foreign").await, Err(StoreError)) { return Err("database failure was not opaque".into()); }
        Ok(())
    }.await;
    accounts.cleanup().await;
    result.unwrap();
}

#[tokio::test]
async fn private_session_upsert_rolls_back_after_mutation() {
    let accounts = disposable_accounts::Accounts::new(2).await;
    let owner = accounts.id(0).to_owned();
    let other = accounts.id(1).to_owned();
    let observer = &accounts.observer;
    let result: Result<(), String> = async {
        observer.execute("INSERT INTO public.private_sessions VALUES ($1,'keep',11), ($2,'target',12)",
            &[&owner, &other]).await.map_err(|e| e.to_string())?;
        for (session, expiry, inject) in [("target", 30_u64, true), ("target", 40, false),
            ("target", 20, false), ("target", 60, false), ("target", 10, true)] {
            let reached = Rc::new(Cell::new(false));
            let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
            if inject {
                let marker = reached.clone();
                store.set_preflight_hook("private-session-upsert", Box::new(move || Box::pin(async move {
                    marker.set(true);
                    Err(StoreError)
                })));
            }
            let outcome = store.upsert_private_session(&owner, session, expiry).await;
            if outcome.is_err() != inject || reached.get() != inject {
                return Err(format!("unexpected outcome or hook reachability for expiry {expiry}"));
            }
            // The observer fence waits for rollback/commit before reading committed rows.
            observer.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
            observer.query_one("SELECT id FROM public.users WHERE id = $1 FOR UPDATE", &[&owner]).await.map_err(|e| e.to_string())?;
            let rows = observer.query("SELECT user_id,session_id,expires_at FROM public.private_sessions WHERE user_id IN ($1,$2) ORDER BY user_id,session_id", &[&owner, &other]).await.map_err(|e| e.to_string())?;
            let actual: Vec<(String, String, i64)> = rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect();
            observer.batch_execute("COMMIT").await.map_err(|e| e.to_string())?;
            let mut expected = vec![(owner.clone(), "keep".into(), 11), (other.clone(), "target".into(), 12)];
            if expiry != 30 { expected.push((owner.clone(), "target".into(), match expiry { 40 => 40, 20 => 20, _ => 60 })); }
            expected.sort();
            if actual != expected { return Err(format!("unexpected committed rows after expiry {expiry}")); }
        }
        let missing = format!("{owner}-missing");
        for (id, session, expiry) in [
            (owner.as_str(), "", 1_u64),
            (owner.as_str(), "\0", 1),
            (owner.as_str(), "target", u64::MAX),
            (owner.as_str(), "target", 253_402_300_800),
            (owner.as_str(), &"x".repeat(129), 1),
            (missing.as_str(), "target", 1),
        ] {
            let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
            if store.upsert_private_session(id, session, expiry).await.is_ok() {
                return Err("invalid or missing-account upsert succeeded".into());
            }
        }
        // Lock only the session row, so the writer's account fence succeeds but its mutation times out.
        observer.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
        observer.query_one("SELECT session_id FROM public.private_sessions WHERE user_id = $1 AND session_id = 'target' FOR UPDATE", &[&owner]).await.map_err(|e| e.to_string())?;
        let failed_mutation = private_client().await?;
        failed_mutation.batch_execute("SET lock_timeout = '200ms'").await.map_err(|e| e.to_string())?;
        let store = catalogue_store_for_postgres_test(failed_mutation, false, Err(StoreError), Rc::new(Cell::new(0)));
        if !matches!(store.upsert_private_session(&owner, "target", 99).await, Err(StoreError)) {
            return Err("mutation failure did not return opaque store error".into());
        }
        drop(store);
        observer.batch_execute("ROLLBACK").await.map_err(|e| e.to_string())?;
        let failed = private_client().await?;
        failed.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
        if failed.batch_execute("SELECT 1/0").await.is_ok() {
            return Err("database failure injection did not abort the transaction".into());
        }
        let store = catalogue_store_for_postgres_test(failed, false, Err(StoreError), Rc::new(Cell::new(0)));
        if !matches!(store.upsert_private_session(&owner, "target", 99).await, Err(StoreError)) {
            return Err("database failure did not return opaque store error".into());
        }
        drop(store);
        observer.execute("UPDATE public.users SET deleting_at = now() WHERE id = $1", &[&owner]).await.map_err(|e| e.to_string())?;
        let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
        if store.upsert_private_session(&owner, "target", 99).await.is_ok() {
            return Err("deleting-account upsert succeeded".into());
        }
        let rows = observer.query("SELECT user_id,session_id,expires_at FROM public.private_sessions WHERE user_id IN ($1,$2) ORDER BY user_id,session_id", &[&owner, &other]).await.map_err(|e| e.to_string())?;
        let actual: Vec<(String, String, i64)> = rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect();
        let mut expected = vec![(owner.clone(), "keep".into(), 11), (owner.clone(), "target".into(), 60), (other.clone(), "target".into(), 12)];
        expected.sort();
        if actual != expected { return Err("failed upsert changed scoped rows".into()); }
        observer.execute("UPDATE public.users SET deleting_at = NULL WHERE id = $1", &[&owner]).await.map_err(|e| e.to_string())?;
        let wide = "é".repeat(128);
        for (session, expiry) in [("A", 0_u64), ("a", 1), (wide.as_str(), 2)] {
            let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
            store.upsert_private_session(&owner, session, expiry).await.map_err(|_| "valid session rejected".to_string())?;
            let row = observer.query_one("SELECT expires_at FROM public.private_sessions WHERE user_id = $1 AND session_id = $2", &[&owner, &session]).await.map_err(|e| e.to_string())?;
            if row.get::<_, i64>(0) != expiry as i64 { return Err("unexpected boundary expiration".into()); }
        }
        let cases = observer.query("SELECT session_id FROM public.private_sessions WHERE user_id = $1 AND session_id IN ('A', 'a')", &[&owner]).await.map_err(|e| e.to_string())?;
        if cases.len() != 2 { return Err("case-distinct sessions did not coexist".into()); }
        Ok(())
    }.await;
    // Also release a failed observer transaction before fixture cleanup.
    let _ = observer.batch_execute("ROLLBACK").await;
    accounts.cleanup().await;
    result.unwrap();
}

#[tokio::test]
async fn private_session_blocked_mutation_releases_account_fence() {
    let accounts = disposable_accounts::Accounts::new(1).await;
    let owner = accounts.id(0).to_owned();
    let observer = &accounts.observer;
    let locker = client(false).await;
    let result: Result<(), String> = async {
        observer.execute("INSERT INTO public.private_sessions VALUES ($1, 'target', 10)", &[&owner]).await.map_err(|e| e.to_string())?;
        locker.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
        locker.query_one("SELECT session_id FROM public.private_sessions WHERE user_id = $1 AND session_id = 'target' FOR UPDATE", &[&owner]).await.map_err(|e| e.to_string())?;
        let locker_pid: i32 = locker.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
        let writer = private_client().await?;
        writer.batch_execute("SET statement_timeout = 0; SET lock_timeout = 0").await.map_err(|e| e.to_string())?;
        let writer_pid: i32 = writer.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
        let store = catalogue_store_for_postgres_test(writer, false, Err(StoreError), Rc::new(Cell::new(0)));
        let upsert = store.upsert_private_session(&owner, "target", 99);
        tokio::pin!(upsert);
        blocker_observer::observe_blocked(observer, locker_pid, writer_pid, upsert.as_mut()).await?;
        if !matches!(tokio::time::timeout(std::time::Duration::from_secs(20), upsert).await, Ok(Err(StoreError))) {
            return Err("blocked mutation did not return a bounded opaque error".into());
        }
        // The session lock is still held; acquiring the account fence proves the
        // writer has released its transaction independently of that lock.
        observer.batch_execute("BEGIN; SET LOCAL lock_timeout = '2s'").await.map_err(|e| e.to_string())?;
        observer.query_one("SELECT id FROM public.users WHERE id = $1 FOR UPDATE", &[&owner]).await.map_err(|e| e.to_string())?;
        let expiry: i64 = observer.query_one("SELECT expires_at FROM public.private_sessions WHERE user_id = $1 AND session_id = 'target'", &[&owner]).await.map_err(|e| e.to_string())?.get(0);
        observer.batch_execute("COMMIT").await.map_err(|e| e.to_string())?;
        if expiry != 10 { return Err("blocked mutation changed the stored expiration".into()); }
        Ok(())
    }.await;
    let _ = observer.batch_execute("ROLLBACK").await;
    let _ = locker.batch_execute("ROLLBACK").await;
    accounts.cleanup().await;
    result.unwrap();
}

#[tokio::test]
async fn private_session_upsert_respects_account_fence() {
    let accounts = disposable_accounts::Accounts::new(2).await;
    let owner = accounts.id(0).to_owned();
    let other = accounts.id(1).to_owned();
    let locker = client(false).await;
    let observer = &accounts.observer;
    let result: Result<(), String> = async {
        locker.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
        locker.execute("UPDATE public.users SET deleting_at = now() WHERE id = $1", &[&owner]).await.map_err(|e| e.to_string())?;
        let blocked = private_client().await?;
        let blocked_pid: i32 = blocked.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
        let locker_pid: i32 = locker.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
        let store = catalogue_store_for_postgres_test(blocked, false, Err(StoreError), Rc::new(Cell::new(0)));
        let upsert = store.upsert_private_session(&owner, "absent", 10);
        tokio::pin!(upsert);
        blocker_observer::observe_blocked(&observer, locker_pid, blocked_pid, upsert.as_mut()).await?;
        let unrelated = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
        unrelated.upsert_private_session(&other, "absent", 20).await.map_err(|_| "unrelated account blocked".to_string())?;
        locker.batch_execute("COMMIT").await.map_err(|e| e.to_string())?;
        if upsert.await.is_ok() { return Err("upsert accepted committed deletion".into()); }
        let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
        if store.upsert_private_session(&owner, "absent", 10).await.is_ok() {
            return Err("upsert accepted account after deletion fence committed".into());
        }
        observer.execute("UPDATE public.users SET deleting_at = NULL WHERE id = $1", &[&owner]).await.map_err(|e| e.to_string())?;
        locker.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
        locker.query_one("SELECT id FROM public.users WHERE id = $1 FOR UPDATE", &[&owner]).await.map_err(|e| e.to_string())?;
        let blocked = private_client().await?;
        let blocked_pid: i32 = blocked.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
        let store = catalogue_store_for_postgres_test(blocked, false, Err(StoreError), Rc::new(Cell::new(0)));
        let upsert = store.upsert_private_session(&owner, "absent", 10);
        tokio::pin!(upsert);
        blocker_observer::observe_blocked(&observer, locker_pid, blocked_pid, upsert.as_mut()).await?;
        locker.batch_execute("COMMIT").await.map_err(|e| e.to_string())?;
        upsert.await.map_err(|_| "upsert failed after exclusive lock released".to_string())?;
        let rows = observer.query("SELECT user_id,session_id,expires_at FROM public.private_sessions WHERE user_id IN ($1,$2) ORDER BY user_id", &[&owner, &other]).await.map_err(|e| e.to_string())?;
        let actual: Vec<(String, String, i64)> = rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect();
        let mut expected = vec![(owner.clone(), "absent".into(), 10), (other.clone(), "absent".into(), 20)];
        expected.sort();
        if actual != expected { return Err("unexpected rows after fence controls".into()); }

        // Hold an actual writer after its mutation; a distinct absent session must
        // wait on that writer rather than merely on an externally held lock.
        let first_client = private_client().await?;
        let first_pid: i32 = first_client.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
        let first = catalogue_store_for_postgres_test(first_client, false, Err(StoreError), Rc::new(Cell::new(0)));
        let reached = Rc::new(Cell::new(false));
        let release = Rc::new(Cell::new(false));
        let (hook_reached, hook_release) = (reached.clone(), release.clone());
        first.set_preflight_hook("private-session-upsert", Box::new(move || Box::pin(async move {
            hook_reached.set(true);
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while !hook_release.get() { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
            }).await.map_err(|_| StoreError)?;
            Ok(())
        })));
        let writer = first.upsert_private_session(&owner, "writer-one", 30);
        tokio::pin!(writer);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !reached.get() {
                tokio::select! {
                    _ = writer.as_mut() => return Err("first writer finished before its hook".to_string()),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
                }
            }
            Ok(())
        }).await.map_err(|_| "first writer did not reach its hook".to_string())??;
        let second_client = private_client().await?;
        let second_pid: i32 = second_client.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
        let second = catalogue_store_for_postgres_test(second_client, false, Err(StoreError), Rc::new(Cell::new(0)));
        let waiter = second.upsert_private_session(&owner, "writer-two", 40);
        tokio::pin!(waiter);
        blocker_observer::observe_blocked(observer, first_pid, second_pid, waiter.as_mut()).await?;
        let unrelated = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
        unrelated.upsert_private_session(&other, "writer-three", 50).await.map_err(|_| "unrelated writer blocked".to_string())?;
        release.set(true);
        writer.await.map_err(|_| "first writer failed".to_string())?;
        waiter.await.map_err(|_| "second writer failed after release".to_string())?;
        let count: i64 = observer.query_one("SELECT count(*) FROM public.private_sessions WHERE (user_id = $1 AND session_id IN ('writer-one','writer-two')) OR (user_id = $2 AND session_id = 'writer-three')", &[&owner, &other]).await.map_err(|e| e.to_string())?.get(0);
        if count != 3 { return Err("writer controls did not commit all sessions".into()); }
        Ok(())
    }.await;
    let _ = locker.batch_execute("ROLLBACK").await;
    accounts.cleanup().await;
    result.unwrap();
}

#[tokio::test]
async fn shared_memberships_use_production_queries_without_private_leaks() {
    use nntune_api::catalogue::*;
    let app = client(true).await;
    app.execute(
        "INSERT INTO library VALUES (NULL, 'shared', $1, ARRAY[upper($2)]),
         ('u1', upper($2), NULL, ARRAY[]::text[])",
        &[&OTHER_RELEASE, &RECORDING],
    )
    .await
    .unwrap();
    for caller in ["u1", "fresh-a", "fresh-b"] {
        let releases = app
            .query(catalogue_member_releases_statement(), &[&caller])
            .await
            .unwrap();
        let ids: Vec<String> = releases.iter().map(|r| r.get(0)).collect();
        assert!(ids.iter().any(|id| id == OTHER_RELEASE));
        assert_eq!(ids.iter().any(|id| id == RELEASE), caller == "u1");
        let pairs = app
            .query(catalogue_member_track_pairs_statement(), &[&caller])
            .await
            .unwrap();
        assert_eq!(pairs.len(), if caller == "u1" { 3 } else { 1 });
        for release in [RELEASE, OTHER_RELEASE] {
            let is_member = caller == "u1" || release == OTHER_RELEASE;
            assert_eq!(
                app.query(
                    catalogue_release_membership_statement(),
                    &[&caller, &release]
                )
                .await
                .unwrap()
                .len(),
                usize::from(is_member)
            );
            let recordings = app
                .query(
                    catalogue_member_release_recordings_statement(),
                    &[&caller, &release],
                )
                .await
                .unwrap();
            assert_eq!(!recordings.is_empty(), is_member);
        }
        let page = app
            .query(library_page_statement(), &[&caller, &50_i64])
            .await
            .unwrap();
        assert_eq!(page.len(), if caller == "u1" { 6 } else { 1 });
        assert!(page.iter().all(|r| r.get::<_, String>(0) != "private"));
        let shared = page
            .iter()
            .find(|row| row.get::<_, &str>("job_id") == "shared")
            .unwrap();
        assert_eq!(shared.get::<_, bool>("recording_job_match"), caller == "u1");
    }
}

#[tokio::test]
async fn member_target_queries_isolate_caller_and_kind() {
    use nntune_api::target_identity::TargetKind;
    async fn seeded() -> Client {
        let app = client(false).await;
        app.batch_execute(
            "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES
             ('u1', 'Job_Own', 'AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA', ARRAY['BBBBBBBB-BBBB-4BBB-8BBB-BBBBBBBBBBBB', 'opaque-invalid']),
             (NULL, 'Job_Shared', 'cccccccc-cccc-4ccc-8ccc-cccccccccccc', ARRAY['dddddddd-dddd-4ddd-8ddd-dddddddddddd']),
             ('other', 'Job_Private', 'eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee', ARRAY['ffffffff-ffff-4fff-8fff-ffffffffffff']),
             ('u1', '11111111-1111-1111-1111-AAAAAAAAAAAA', NULL, ARRAY[]::text[])"
        ).await.unwrap();
        app
    }
    for (kind, target, expected) in [
        (TargetKind::Track, "Job_Own", true),
        (TargetKind::Track, "job_own", false),
        (
            TargetKind::Track,
            "11111111-1111-1111-1111-AAAAAAAAAAAA",
            true,
        ),
        (
            TargetKind::Track,
            "11111111-1111-1111-1111-aaaaaaaaaaaa",
            false,
        ),
        (TargetKind::Track, "OPAQUE-INVALID", false),
        (TargetKind::Track, "Job_Shared", true),
        (TargetKind::Track, "Job_Private", false),
        (
            TargetKind::Track,
            "BBBBBBBB-BBBB-4BBB-8BBB-BBBBBBBBBBBB",
            true,
        ),
        (
            TargetKind::Track,
            "DDDDDDDD-DDDD-4DDD-8DDD-DDDDDDDDDDDD",
            true,
        ),
        (
            TargetKind::Track,
            "ffffffff-ffff-4fff-8fff-ffffffffffff",
            false,
        ),
        (TargetKind::Track, RELEASE, false),
        (
            TargetKind::Release,
            "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
            true,
        ),
        (TargetKind::Release, OTHER_RELEASE, true),
        (
            TargetKind::Release,
            "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
            false,
        ),
        (TargetKind::Release, RECORDING, false),
        (TargetKind::Release, "Job_Own", false),
        (TargetKind::Track, "missing", false),
        (TargetKind::Release, "missing", false),
        (TargetKind::Track, "x' OR true --", false),
        (TargetKind::Release, "x' OR true --", false),
    ] {
        let attempts = Rc::new(Cell::new(0));
        let store = catalogue_store_for_postgres_test(
            seeded().await,
            false,
            Err(StoreError),
            attempts.clone(),
        );
        assert_eq!(
            store
                .catalogue_member_target("u1", kind, target)
                .await
                .unwrap(),
            expected,
            "{kind:?} {target}"
        );
        assert_eq!(attempts.get(), 0);
    }
    let store = catalogue_store_for_postgres_test(
        seeded().await,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    );
    assert!(
        !store
            .catalogue_member_target("u1' OR true --", TargetKind::Track, "Job_Own")
            .await
            .unwrap(),
        "owner must also be bound"
    );
    let app = seeded().await;
    app.batch_execute("DROP TABLE library").await.unwrap();
    let store =
        catalogue_store_for_postgres_test(app, false, Err(StoreError), Rc::new(Cell::new(0)));
    assert!(store
        .catalogue_member_target("u1", TargetKind::Track, "Job_Own")
        .await
        .is_err());
}

#[tokio::test]
async fn taste_collection_is_bounded_scoped_and_fails_closed() {
    async fn seeded() -> Client {
        let app = client(false).await;
        app.batch_execute("BEGIN;
            INSERT INTO public.users (id,email,created_at) VALUES ('collection-owner','collection-owner@example.invalid',1), ('collection-other','collection-other@example.invalid',1);
            INSERT INTO public.account_taste SELECT 'collection-owner', 'track', 'Job-' || i, 'love', NULL, i, i FROM generate_series(1, 501) i;
            INSERT INTO public.account_taste VALUES
                ('collection-owner','release','Job-501','love',NULL,502,502),
                ('collection-owner','track','job-501','dislike',NULL,503,503),
                ('collection-owner','track','hide','hide',NULL,504,504),
                ('collection-owner','track','ban','ban',NULL,505,505),
                ('collection-owner','track','past','snooze',99,506,506),
                ('collection-owner','track','equal','snooze',100,507,507),
                ('collection-owner','track','future','snooze',101,508,508),
                ('collection-other','track','private','love',NULL,999,999)").await.unwrap();
        app
    }
    let store = catalogue_store_for_postgres_test(
        seeded().await,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    );
    let rows = store.taste_list("collection-owner", 100).await.unwrap();
    assert_eq!(rows.len(), 500);
    assert!(rows.windows(2).all(|p| p[0].updated_at >= p[1].updated_at));
    for kind in ["track", "release"] {
        assert!(rows
            .iter()
            .any(|r| r.kind == kind && r.target_id == "Job-501" && r.signal == "love"));
    }
    for (id, signal) in [
        ("job-501", "dislike"),
        ("hide", "hide"),
        ("ban", "ban"),
        ("future", "snooze"),
    ] {
        assert!(rows.iter().any(|r| r.target_id == id && r.signal == signal));
    }
    for id in ["past", "equal", "private", "Job-1"] {
        assert!(!rows.iter().any(|r| r.target_id == id));
    }
    assert_eq!(rows[0].snooze_until, Some(101));
    drop(store);
    for owner in ["absent", "collection-owner' OR true --"] {
        let store = catalogue_store_for_postgres_test(
            client(false).await,
            false,
            Err(StoreError),
            Rc::new(Cell::new(0)),
        );
        assert!(store.taste_list(owner, 100).await.unwrap().is_empty());
    }
    let store = catalogue_store_for_postgres_test(
        client(false).await,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    );
    assert!(store
        .taste_list("collection-owner", u64::MAX)
        .await
        .is_err());
    let app = seeded().await;
    assert!(app.batch_execute("SELECT 1/0").await.is_err());
    let store =
        catalogue_store_for_postgres_test(app, false, Err(StoreError), Rc::new(Cell::new(0)));
    assert!(store.taste_list("collection-owner", 100).await.is_err());
}

#[tokio::test]
async fn likes_collection_is_bounded_scoped_and_fails_closed() {
    async fn seeded() -> Client {
        let app = client(false).await;
        app.batch_execute("BEGIN;
            INSERT INTO public.users (id,email,created_at) VALUES ('likes-owner','likes-owner@example.invalid',1), ('likes-other','likes-other@example.invalid',1);
            INSERT INTO public.account_taste SELECT 'likes-owner', 'track', 'Job-' || i, 'love', NULL, i, i FROM generate_series(1, 501) i;
            INSERT INTO public.account_taste VALUES
                ('likes-owner','release','Job-501','love',NULL,502,502),
                ('likes-owner','track','job-501','dislike',NULL,503,503),
                ('likes-owner','track','hide','hide',NULL,504,504),
                ('likes-owner','track','ban','ban',NULL,505,505),
                ('likes-owner','track','past','snooze',99,506,506),
                ('likes-owner','track','equal','snooze',100,507,507),
                ('likes-owner','track','future','snooze',101,508,508),
                ('likes-other','track','private','love',NULL,999,999)").await.unwrap();
        app
    }
    let app = seeded().await;
    let store =
        catalogue_store_for_postgres_test(app, false, Err(StoreError), Rc::new(Cell::new(0)));
    let rows = store.likes_list("likes-owner").await.unwrap();
    assert_eq!(rows.len(), 500);
    assert!(rows.windows(2).all(|p| p[0].created_at >= p[1].created_at));
    assert!(rows
        .iter()
        .all(|r| r.signal == "love" && r.target_id != "private"));
    assert!(rows
        .iter()
        .any(|r| r.kind == "release" && r.target_id == "Job-501"));
    assert!(rows
        .iter()
        .any(|r| r.kind == "track" && r.target_id == "Job-501"));
    assert_eq!(rows[0].created_at, 502);
    assert!(!rows.iter().any(|r| r.target_id == "Job-1"));
    drop(store);
    let store = catalogue_store_for_postgres_test(
        seeded().await,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    );
    assert!(store
        .likes_list("likes-owner' OR true --")
        .await
        .unwrap()
        .is_empty());
    drop(store);
    let app = seeded().await;
    assert!(app.batch_execute("SELECT 1/0").await.is_err());
    let store =
        catalogue_store_for_postgres_test(app, false, Err(StoreError), Rc::new(Cell::new(0)));
    assert!(store.likes_list("likes-owner").await.is_err());
}

#[tokio::test]
async fn taste_get_reads_only_exact_owner_kind_and_identity() {
    use nntune_api::preference_rows::TasteDbRow;
    use nntune_api::target_identity::TargetKind::{Release, Track};
    const JOB: &str = "11111111-1111-1111-1111-AAAAAAAAAAAA";
    async fn seeded() -> Client {
        let app = client(false).await;
        app.batch_execute("BEGIN;
            INSERT INTO public.users (id,email,created_at) VALUES ('taste-reader','taste-reader@example.invalid',1), ('taste-other','taste-other@example.invalid',1);
            INSERT INTO public.account_taste VALUES
              ('taste-reader','track','11111111-1111-1111-1111-AAAAAAAAAAAA','snooze',5,1,2),
              ('taste-reader','release','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','love',NULL,3,4),
              ('taste-reader','track','bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb','love',NULL,3,4),
              ('taste-other','track','private','ban',NULL,6,7)").await.unwrap();
        app
    }
    let snooze = TasteDbRow {
        kind: "track".into(),
        target_id: JOB.into(),
        signal: "snooze".into(),
        snooze_until: Some(5),
        created_at: 1,
        updated_at: 2,
    };
    let love = TasteDbRow {
        kind: "release".into(),
        target_id: RELEASE.into(),
        signal: "love".into(),
        snooze_until: None,
        created_at: 3,
        updated_at: 4,
    };
    for (owner, kind, target, expected) in [
        ("taste-reader", Track, JOB, Some(snooze)),
        ("taste-reader", Release, RELEASE, Some(love)),
        (
            "taste-reader",
            Track,
            RECORDING,
            Some(TasteDbRow {
                kind: "track".into(),
                target_id: RECORDING.into(),
                signal: "love".into(),
                snooze_until: None,
                created_at: 3,
                updated_at: 4,
            }),
        ),
        ("taste-reader", Track, "private", None),
        ("taste-other", Track, JOB, None),
        ("taste-reader", Release, JOB, None),
        ("taste-reader", Track, RELEASE, None),
        (
            "taste-reader",
            Track,
            "11111111-1111-1111-1111-aaaaaaaaaaaa",
            None,
        ),
        ("taste-reader' OR true --", Track, JOB, None),
        ("taste-reader", Track, "missing", None),
    ] {
        let store = catalogue_store_for_postgres_test(
            seeded().await,
            false,
            Err(StoreError),
            Rc::new(Cell::new(0)),
        );
        assert_eq!(
            store.taste_get(owner, kind, target).await.unwrap(),
            expected
        );
    }
    let app = seeded().await;
    assert!(app.batch_execute("SELECT 1/0").await.is_err());
    let store =
        catalogue_store_for_postgres_test(app, false, Err(StoreError), Rc::new(Cell::new(0)));
    assert!(store.taste_get("taste-reader", Track, JOB).await.is_err());
}

#[tokio::test]
async fn taste_upsert_replays_and_fails_closed() {
    use nntune_api::target_identity::TargetKind::{Release, Track};
    use nntune_api::taste_signal::TasteSignal::{Ban, Love, Snooze};
    // Each injection supplies one connection to the production method.
    for (existing, kind, signal, until, now, created, expiry) in [
        (None, Track, Love, None, 10, 10, None),
        (
            Some(("love", None::<i64>, 10_i64)),
            Track,
            Love,
            None,
            20,
            10,
            None,
        ),
        (
            Some(("snooze", Some(50), 10)),
            Track,
            Love,
            None,
            30,
            30,
            None,
        ),
        (
            Some(("snooze", Some(25), 10)),
            Track,
            Love,
            None,
            30,
            30,
            None,
        ),
        (
            Some(("snooze", Some(25), 10)),
            Track,
            Snooze,
            Some(50),
            30,
            10,
            Some(50),
        ),
        (Some(("love", None, 10)), Track, Ban, None, 30, 10, None),
        (Some(("love", None, 10)), Release, Ban, None, 30, 30, None),
    ] {
        let app = client(false).await;
        app.batch_execute("BEGIN; INSERT INTO public.users (id,email,created_at) VALUES ('taste-writer','taste-writer@example.invalid',1)").await.unwrap();
        if let Some((old, old_until, old_created)) = existing {
            app.execute(
                "INSERT INTO public.account_taste VALUES ($1,'track',$2,$3,$4,$5,$5)",
                &[&"taste-writer", &"Job_A", &old, &old_until, &old_created],
            )
            .await
            .unwrap();
        }
        let store =
            catalogue_store_for_postgres_test(app, false, Err(StoreError), Rc::new(Cell::new(0)));
        let row = store
            .taste_upsert("taste-writer", kind, "Job_A", signal, until, now)
            .await
            .unwrap();
        assert_eq!(
            (
                row.kind.as_str(),
                row.target_id.as_str(),
                row.signal.as_str(),
                row.snooze_until,
                row.created_at,
                row.updated_at
            ),
            (
                kind.as_str(),
                "Job_A",
                signal.as_str(),
                expiry,
                created,
                now as i64
            )
        );
    }
    for (user, invalid) in [
        ("missing", false),
        ("taste-deleting", false),
        ("taste-writer", true),
    ] {
        let app = client(false).await;
        app.batch_execute("BEGIN; INSERT INTO public.users (id,email,created_at) VALUES ('taste-writer','taste-writer@example.invalid',1), ('taste-deleting','taste-deleting@example.invalid',1); UPDATE public.users SET deleting_at = now() WHERE id = 'taste-deleting'").await.unwrap();
        let store =
            catalogue_store_for_postgres_test(app, false, Err(StoreError), Rc::new(Cell::new(0)));
        assert!(store
            .taste_upsert(
                user,
                Track,
                "Job_A",
                if invalid { Snooze } else { Love },
                None,
                70
            )
            .await
            .is_err());
    }
}

#[tokio::test]
async fn preference_deletes_are_scoped_idempotent_and_fenced() {
    use nntune_api::target_identity::TargetKind::{Release, Track};
    let observer = client(false).await;
    observer.batch_execute("INSERT INTO public.users (id,email,created_at) VALUES ('delete-a','delete-a@example.invalid',1), ('delete-b','delete-b@example.invalid',1), ('delete-pending','delete-pending@example.invalid',1); UPDATE public.users SET deleting_at = now() WHERE id = 'delete-pending'; INSERT INTO public.account_taste VALUES ('delete-a','track','love','love',NULL,1,1), ('delete-a','track','dislike','dislike',NULL,1,1), ('delete-a','track','hide','hide',NULL,1,1), ('delete-a','track','ban','ban',NULL,1,1), ('delete-a','track','snooze','snooze',2,1,1), ('delete-a','track','Job_A','love',NULL,1,1), ('delete-a','track','job_a','love',NULL,1,1), ('delete-a','release','Job_A','love',NULL,1,1), ('delete-b','track','Job_A','love',NULL,1,1), ('delete-pending','track','Job_A','love',NULL,1,1)").await.unwrap();
    let mut results = Vec::new();
    for id in [
        "love", "dislike", "hide", "ban", "snooze", "missing", "love",
    ] {
        let store = catalogue_store_for_postgres_test(
            client(false).await,
            false,
            Err(StoreError),
            Rc::new(Cell::new(0)),
        );
        results.push(store.like_delete("delete-a", Track, id).await.unwrap());
    }
    let store = catalogue_store_for_postgres_test(
        client(false).await,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    );
    let cleared = store
        .taste_delete("delete-a", Track, "snooze")
        .await
        .unwrap();
    let store = catalogue_store_for_postgres_test(
        client(false).await,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    );
    let repeat = store
        .taste_delete("delete-a", Track, "snooze")
        .await
        .unwrap();
    let store = catalogue_store_for_postgres_test(
        client(false).await,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    );
    let isolated = store
        .taste_delete("delete-a", Track, "Job_A")
        .await
        .unwrap();
    let mut errors = Vec::new();
    for owner in ["missing", "delete-pending"] {
        let store = catalogue_store_for_postgres_test(
            client(false).await,
            false,
            Err(StoreError),
            Rc::new(Cell::new(0)),
        );
        errors.push(store.taste_delete(owner, Track, "Job_A").await.is_err());
    }
    let failed = client(false).await;
    failed.batch_execute("BEGIN").await.unwrap();
    assert!(failed.batch_execute("SELECT 1/0").await.is_err());
    let store =
        catalogue_store_for_postgres_test(failed, false, Err(StoreError), Rc::new(Cell::new(0)));
    let db_error = store.like_delete("delete-b", Track, "Job_A").await.is_err();
    let rows = observer.query("SELECT user_id, kind, target_id, signal FROM public.account_taste WHERE user_id IN ('delete-a','delete-b','delete-pending') ORDER BY user_id, kind, target_id", &[]).await.unwrap();
    let states: Vec<(String, String, String, String)> = rows
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect();
    observer
        .batch_execute(
            "DELETE FROM public.users WHERE id IN ('delete-a','delete-b','delete-pending')",
        )
        .await
        .unwrap();
    assert_eq!(results, [true, false, false, false, false, false, false]);
    assert!(cleared && !repeat && isolated && errors.into_iter().all(|x| x) && db_error);
    let expected = [
        ("delete-a", "release", "Job_A", "love"),
        ("delete-a", "track", "ban", "ban"),
        ("delete-a", "track", "dislike", "dislike"),
        ("delete-a", "track", "hide", "hide"),
        ("delete-a", "track", "job_a", "love"),
        ("delete-b", "track", "Job_A", "love"),
        ("delete-pending", "track", "Job_A", "love"),
    ];
    assert_eq!(
        states,
        expected.map(|(a, b, c, d)| (a.into(), b.into(), c.into(), d.into()))
    );
}

#[tokio::test]
async fn taste_upsert_isolates_owners_and_rolls_back_invalid_replacement() {
    use nntune_api::target_identity::TargetKind::Track;
    use nntune_api::taste_signal::TasteSignal::{Ban, Snooze};
    let accounts = disposable_accounts::Accounts::new(2).await;
    let (a, b) = (accounts.id(0), accounts.id(1));
    let observer = &accounts.observer;
    let result = async {
    observer.execute("INSERT INTO public.account_taste VALUES ($1,'track','job','love',NULL,1,1), ($2,'track','job','love',NULL,2,2)", &[&a, &b]).await?;
    let store = catalogue_store_for_postgres_test(
        client(false).await,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    );
    let changed = store
        .taste_upsert(a, Track, "job", Ban, None, 3)
        .await;
    let store = catalogue_store_for_postgres_test(
        client(false).await,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    );
    let rejected = store
        .taste_upsert(a, Track, "job", Snooze, None, 4)
        .await;
    let rows = observer.query("SELECT user_id, signal, updated_at FROM public.account_taste WHERE user_id = ANY($1)", &[&vec![a, b]]).await?;
    let states: Vec<(String, String, i64)> = rows.iter().map(|row| (row.get(0), row.get(1), row.get(2))).collect();
    Ok::<_, tokio_postgres::Error>((changed, rejected, states))
    }.await;
    let expected = [
        (a.to_owned(), "ban".to_owned(), 3),
        (b.to_owned(), "love".to_owned(), 2),
    ];
    accounts.cleanup().await;
    let (changed, rejected, states) = result.unwrap();
    assert!(changed.is_ok());
    assert!(rejected.is_err());
    assert_eq!(states.len(), 2);
    assert!(expected.iter().all(|row| states.contains(row)));
}

#[tokio::test]
async fn import_like_counts_only_one_transition_and_fences_replay() {
    use nntune_api::target_identity::TargetKind::Track;
    let accounts = disposable_accounts::Accounts::new(1).await;
    let owner = accounts.id(0);
    let db = &accounts.observer;
    let result = async {
        let ((a, _), (b, _)) = (store(false, false).await, store(false, false).await);
        let counts = tokio::join!(a.import_like(owner, Track, "job", 10), b.import_like(owner, Track, "job", 20));
        db.execute("UPDATE public.users SET deleting_at = now() WHERE id = $1", &[&owner]).await?;
        let (c, _) = store(false, false).await;
        let fenced = c.import_like(owner, Track, "job", 30).await;
        let row = db.query_one("SELECT signal, snooze_until, created_at, updated_at FROM public.account_taste WHERE user_id = $1", &[&owner]).await?;
        Ok::<_, tokio_postgres::Error>((counts, fenced, row))
    }.await;
    accounts.cleanup().await;
    let (counts, fenced, row) = result.unwrap();
    let added_at: i64 = match counts {
        (Ok(true), Ok(false)) => 10,
        (Ok(false), Ok(true)) => 20,
        other => panic!("expected one addition: {other:?}"),
    };
    assert!(fenced.is_err());
    assert_eq!(row.get::<_, &str>(0), "love");
    assert_eq!(row.get::<_, Option<i64>>(1), None);
    assert_eq!(row.get::<_, i64>(2), added_at);
    assert_eq!(row.get::<_, i64>(3), added_at);
}

#[tokio::test]
async fn taste_upsert_waits_for_deletion_fence() {
    use nntune_api::target_identity::TargetKind::Track;
    use nntune_api::taste_signal::TasteSignal::Ban;
    use std::time::Duration;
    let locker = client(false).await;
    let writer = client(false).await;
    let accounts = disposable_accounts::Accounts::new(1).await;
    let account = accounts.id(0).to_owned();
    let observer = &accounts.observer;
    let outcome = async {
        let writer_pid: i32 = writer.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
        observer.execute("INSERT INTO public.account_taste VALUES ($1,'track','old','love',NULL,1,1)", &[&account]).await?;
        locker.batch_execute("BEGIN").await?;
        locker.execute("UPDATE public.users SET deleting_at = now() WHERE id = $1", &[&account]).await?;
        let store = catalogue_store_for_postgres_test(writer, false, Err(StoreError), Rc::new(Cell::new(0)));
        let write = store.taste_upsert(&account, Track, "old", Ban, None, 10);
        let release = async {
            let observed = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let waiting: bool = observer.query_one(
                        "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1 AND NOT granted)",
                        &[&writer_pid],
                    ).await?.get(0);
                    if waiting {
                        return Ok::<_, tokio_postgres::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await;
            locker.batch_execute("COMMIT").await?;
            match observed {
                Ok(result) => result.map(|()| true),
                Err(_) => Ok(false),
            }
        };
        let (result, observed) = tokio::join!(write, release);
        let row = observer.query_one("SELECT signal, created_at, updated_at FROM public.account_taste WHERE user_id = $1 AND target_id = 'old'", &[&account]).await?;
        let state: (String, i64, i64) = (row.get(0), row.get(1), row.get(2));
        Ok::<_, tokio_postgres::Error>((result, observed?, state))
    }.await;
    let _ = locker.batch_execute("ROLLBACK").await;
    accounts.cleanup().await;
    let check = client(false).await;
    let remaining: i64 = check
        .query_one(
            "SELECT count(*) FROM public.account_taste WHERE user_id = $1",
            &[&account],
        )
        .await
        .unwrap()
        .get(0);
    let (result, observed, state) = outcome.unwrap();
    assert!(observed, "writer must wait on the deleting account");
    assert!(result.is_err());
    assert_eq!(state, ("love".into(), 1, 1));
    assert_eq!(remaining, 0, "account deletion cascades existing taste");
}

#[tokio::test]
async fn preference_delete_waits_for_deletion_fence() {
    use nntune_api::target_identity::TargetKind::Track;
    use std::time::Duration;
    let observer = client(false).await;
    let locker = client(false).await;
    let writer = client(false).await;
    let pid: i32 = writer
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    observer.batch_execute("INSERT INTO public.users (id,email,created_at) VALUES ('delete-race','delete-race@example.invalid',1); INSERT INTO public.account_taste VALUES ('delete-race','track','old','love',NULL,1,1)").await.unwrap();
    locker
        .batch_execute(
            "BEGIN; UPDATE public.users SET deleting_at = now() WHERE id = 'delete-race'",
        )
        .await
        .unwrap();
    let store =
        catalogue_store_for_postgres_test(writer, false, Err(StoreError), Rc::new(Cell::new(0)));
    let delete = store.like_delete("delete-race", Track, "old");
    let release = async {
        let observed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let waiting: bool = observer
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1 AND NOT granted)",
                        &[&pid],
                    )
                    .await
                    .unwrap()
                    .get(0);
                if waiting {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        locker.batch_execute("COMMIT").await.unwrap();
        observed
    };
    let (result, observed) = tokio::join!(delete, release);
    let count: i64 = observer
        .query_one(
            "SELECT count(*) FROM public.account_taste WHERE user_id = 'delete-race'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    observer
        .batch_execute("DELETE FROM public.users WHERE id = 'delete-race'")
        .await
        .unwrap();
    assert!(observed, "delete must wait on the account fence");
    assert!(result.is_err());
    assert_eq!(count, 1);
}

#[tokio::test]
async fn absent_mirror_library_preserves_app_rows_and_occurrences() {
    let (store, attempts) = store(true, false).await;
    let rows = store.catalogue_library("u1").await.unwrap();
    let page: Vec<_> = [
        Some(RECORDING),
        Some(RECORDING),
        Some("stored-invalid"),
        None,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, recording)| LibraryPageRow {
        job_id: if recording.is_some() {
            "job-1"
        } else {
            "job-0"
        }
        .into(),
        release_mbid: recording.map(|_| RELEASE.into()),
        recording_mbids: if recording.is_some() {
            vec![RECORDING.into(), RECORDING.into(), "stored-invalid".into()]
        } else {
            vec![]
        },
        recording_mbid: recording.map(str::to_owned),
        recording_ord: if recording.is_some() { i as i64 + 1 } else { 0 },
        recording_job_match: false,
    })
    .collect();
    assert_eq!(rows, assemble_library_rows(&page, &[], &[], &[]));
    assert_ne!(
        nntune_api::catalogue::library_entry(&rows[0]).occurrence_key,
        nntune_api::catalogue::library_entry(&rows[1]).occurrence_key
    );
    assert_eq!(attempts.get(), 0, "absence is not a failed connection");
}

#[tokio::test]
async fn empty_app_membership_never_connects_in_either_mode() {
    for configured in [false, true] {
        let (s, attempts) = store(false, configured).await;
        assert!(s.catalogue_library("u1").await.unwrap().is_empty());
        assert_eq!(attempts.get(), 0);
        let (s, attempts) = store(false, configured).await;
        assert!(s.catalogue_artists("u1").await.unwrap().is_empty());
        assert_eq!(attempts.get(), 0);
        let (s, attempts) = store(false, configured).await;
        assert!(s.catalogue_releases("u1").await.unwrap().is_empty());
        assert_eq!(attempts.get(), 0);
        let (s, attempts) = store(false, configured).await;
        assert!(s.catalogue_tracks("u1").await.unwrap().is_empty());
        assert_eq!(attempts.get(), 0);
    }
}

#[tokio::test]
async fn empty_effective_release_recordings_need_no_mirror() {
    for configured in [false, true] {
        for recordings in [vec![], vec!["stored-invalid".to_owned()]] {
            let app = client(true).await;
            app.execute(
                "UPDATE library SET recording_mbids = $1 WHERE user_id = 'u1'",
                &[&recordings],
            )
            .await
            .unwrap();
            let attempts = Rc::new(Cell::new(0));
            let s = catalogue_store_for_postgres_test(
                app,
                configured,
                Err(StoreError),
                attempts.clone(),
            );
            assert!(s
                .catalogue_release_tracks("u1", RELEASE)
                .await
                .unwrap()
                .is_empty());
            assert_eq!(attempts.get(), 0);
        }
    }
}

#[tokio::test]
async fn nonempty_entities_fail_without_dialing_an_absent_binding() {
    for configured in [false, true] {
        let expected_attempts = usize::from(configured);
        let (s, attempts) = store(true, configured).await;
        assert!(s.catalogue_artists("u1").await.is_err());
        assert_eq!(attempts.get(), expected_attempts);
        let (s, attempts) = store(true, configured).await;
        assert!(s.catalogue_releases("u1").await.is_err());
        assert_eq!(attempts.get(), expected_attempts);
        let (s, attempts) = store(true, configured).await;
        assert!(s.catalogue_tracks("u1").await.is_err());
        assert_eq!(attempts.get(), expected_attempts);
        let (s, attempts) = store(true, configured).await;
        assert!(s.catalogue_release("u1", RELEASE).await.is_err());
        assert_eq!(attempts.get(), expected_attempts);
        let (s, attempts) = store(true, configured).await;
        assert!(s.catalogue_release_tracks("u1", RELEASE).await.is_err());
        assert_eq!(attempts.get(), expected_attempts);
    }
}

#[tokio::test]
async fn configured_library_connect_and_statement_failures_never_degrade() {
    let (s, attempts) = store(true, true).await;
    assert!(s.catalogue_library("u1").await.is_err());
    assert_eq!(attempts.get(), 1);
    // A connected mirror without its required tables must propagate the real
    // statement failure, not serve the same null-enriched rows as absence.
    let attempts = Rc::new(Cell::new(0));
    let s = catalogue_store_for_postgres_test(
        client(true).await,
        true,
        Ok(client(false).await),
        attempts.clone(),
    );
    assert!(s.catalogue_library("u1").await.is_err());
    assert_eq!(attempts.get(), 1);
}

#[tokio::test]
async fn release_membership_hides_nonmembers_before_mirror_state() {
    for configured in [false, true] {
        for mbid in ["malformed", OTHER_RELEASE] {
            let (s, attempts) = store(true, configured).await;
            assert!(s.catalogue_release("u1", mbid).await.unwrap().is_none());
            assert_eq!(attempts.get(), 0);
            let (s, attempts) = store(true, configured).await;
            assert!(s
                .catalogue_release_tracks("u1", mbid)
                .await
                .unwrap()
                .is_empty());
            assert_eq!(attempts.get(), 0);
        }
    }
}

#[tokio::test]
async fn subsonic_snapshot_fails_closed_when_member_mirror_is_unavailable() {
    for configured in [false, true] {
        let attempts = Rc::new(Cell::new(0));
        let store = catalogue_store_for_postgres_test(
            client(true).await,
            configured,
            Err(StoreError),
            attempts.clone(),
        );
        assert!(store.subsonic_snapshot_catalogue("u1").await.is_err());
        assert_eq!(attempts.get(), usize::from(configured));
    }
}

#[tokio::test]
async fn subsonic_snapshot_empty_membership_does_not_connect_to_mirror() {
    for configured in [false, true] {
        let attempts = Rc::new(Cell::new(0));
        let store = catalogue_store_for_postgres_test(
            client(false).await,
            configured,
            Err(StoreError),
            attempts.clone(),
        );
        let (artists, releases, tracks) = store.subsonic_snapshot_catalogue("u1").await.unwrap();
        assert!(artists.is_empty() && releases.is_empty() && tracks.is_empty());
        assert_eq!(attempts.get(), 0);
    }
}

const SNAPSHOT_ARTIST: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const SNAPSHOT_RELEASE: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

/// Mirror-cluster client for the snapshot read: the `mb_*` temp tables the
/// production mirror statements query, on a second disposable connection
/// (temp tables are per-session, so app and mirror stay separate).
async fn mirror_client() -> Client {
    let client = disposable_accounts::connect_guarded().await.unwrap();
    client
        .batch_execute(
            "SET search_path = pg_temp;
             CREATE TEMP TABLE mb_artist (mbid uuid PRIMARY KEY, name text, sort_name text, type text, country text);
             CREATE TEMP TABLE mb_release (mbid uuid PRIMARY KEY, title text, release_type text, year smallint, artist_mbid uuid);
             CREATE TEMP TABLE mb_recording (mbid uuid PRIMARY KEY, title text, duration_ms integer);
             CREATE TEMP TABLE mb_track (release_mbid uuid, recording_mbid uuid, medium_no smallint, track_no smallint, PRIMARY KEY (release_mbid, medium_no, track_no))",
        )
        .await
        .unwrap();
    client
}

#[tokio::test]
async fn subsonic_snapshot_accepts_exact_cap_track_page() {
    for cap in [2, 3] {
        let recordings = vec![
            "00000001-0000-4000-8000-000000000001".to_string(),
            "00000002-0000-4000-8000-000000000002".to_string(),
        ];
        let app = client(false).await;
        app.execute(
            "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', 'boundary', $1, $2)",
            &[&SNAPSHOT_RELEASE, &recordings],
        )
        .await
        .unwrap();
        let mirror = mirror_client().await;
        mirror.batch_execute(&format!(
            "INSERT INTO mb_artist VALUES ('{SNAPSHOT_ARTIST}', 'Artist', 'Artist', 'Person', 'US');
             INSERT INTO mb_release VALUES ('{SNAPSHOT_RELEASE}', 'Album', 'Album', 2001, '{SNAPSHOT_ARTIST}');"
        )).await.unwrap();
        mirror.execute(
            "INSERT INTO mb_recording SELECT g::uuid, 'Song', 200000 FROM unnest($1::text[]) AS g",
            &[&recordings],
        ).await.unwrap();
        mirror.execute(
            &format!("INSERT INTO mb_track SELECT '{SNAPSHOT_RELEASE}'::uuid, g::uuid, 1, position::smallint FROM unnest($1::text[]) WITH ORDINALITY AS tracks(g, position)"),
            &[&recordings],
        ).await.unwrap();
        let attempts = Rc::new(Cell::new(0));
        let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts.clone());
        let reached = mark_fetch_boundary(&store, "track");
        let result = store.subsonic_snapshot_catalogue_with_cap("u1", cap).await;
        assert!(reached.get(), "track fetch boundary must be reachable");
        assert_eq!(
            result.unwrap().2.len(),
            2,
            "a page at or below cap is complete"
        );
        assert_eq!(attempts.get(), 1);
    }
}

/// Over-cap (cap+1 and beyond) mirror pages are refused by the in-database
/// preflight count, for each of the artist, release and track reads.
#[tokio::test]
async fn subsonic_snapshot_rejects_over_cap_pages_in_query() {
    // 3 member releases stay under the membership cap (4) and reach the mirror.
    for (distinct_artists, rows) in [(true, 3_u32), (true, 4), (false, 3), (false, 4)] {
        let ids: Vec<String> = (1..=rows).map(|n| synthetic_uuid(2, n)).collect();
        let app = client(false).await;
        app.execute(
            "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) SELECT 'u1', r, r, ARRAY[]::text[] FROM unnest($1::text[]) AS r",
            &[&ids],
        ).await.unwrap();
        let mirror = mirror_client().await;
        mirror.execute(
            "INSERT INTO mb_artist SELECT g::uuid, 'A', 'A', 'Person', 'US' FROM unnest($1::text[]) AS g",
            &[&ids],
        ).await.unwrap();
        mirror.execute(
            "INSERT INTO mb_release SELECT g::uuid, 'R', 'Album', 2001, CASE WHEN $2 THEN g::uuid ELSE $3::text::uuid END FROM unnest($1::text[]) AS g",
            &[&ids, &distinct_artists, &ids[0]],
        ).await.unwrap();
        let phase = if distinct_artists {
            "artist"
        } else {
            "release"
        };
        let (accepted, reached, attempts) = snapshot_fetch_boundary(app, mirror, 2, phase).await;
        assert!(!accepted, "{rows} rows over cap 2 must fail closed");
        assert!(
            !reached,
            "{phase} fetch must not be reached after rejection"
        );
        assert_eq!(attempts, 1);
    }
    let recordings: Vec<String> = (1..=3_u32).map(|n| synthetic_uuid(3, n)).collect();
    let app = client(false).await;
    app.execute(
        "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', 'tracks', $1, $2)",
        &[&SNAPSHOT_RELEASE, &recordings],
    ).await.unwrap();
    let mirror = mirror_client().await;
    mirror.batch_execute(&format!(
        "INSERT INTO mb_artist VALUES ('{SNAPSHOT_ARTIST}', 'Artist', 'Artist', 'Person', 'US');
         INSERT INTO mb_release VALUES ('{SNAPSHOT_RELEASE}', 'Album', 'Album', 2001, '{SNAPSHOT_ARTIST}');"
    )).await.unwrap();
    mirror
        .execute(
            "INSERT INTO mb_recording SELECT g::uuid, 'Song', 1000 FROM unnest($1::text[]) AS g",
            &[&recordings],
        )
        .await
        .unwrap();
    mirror.execute(
        &format!("INSERT INTO mb_track SELECT '{SNAPSHOT_RELEASE}'::uuid, g::uuid, 1, position::smallint FROM unnest($1::text[]) WITH ORDINALITY AS tracks(g, position)"),
        &[&recordings],
    ).await.unwrap();
    let (accepted, reached, _) = snapshot_fetch_boundary(app, mirror, 2, "track").await;
    assert!(!accepted, "3 tracks over cap 2 must fail closed");
    assert!(!reached, "track fetch must not be reached after rejection");
}

#[tokio::test]
async fn subsonic_snapshot_accepts_exact_cap_artist_and_release_pages() {
    const SECOND_ARTIST: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    const SECOND_RELEASE: &str = "ffffffff-ffff-4fff-8fff-ffffffffffff";
    // Two artists exercise the first query; one artist and two releases
    // reach the second query without hitting the artist boundary first.
    for distinct_artists in [true, false] {
        for cap in [2, 3] {
            let app = client(false).await;
            for release in [SNAPSHOT_RELEASE, SECOND_RELEASE] {
                app.execute(
                    "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', $1, $1, ARRAY[]::text[])",
                    &[&release],
                )
                .await
                .unwrap();
            }
            let mirror = mirror_client().await;
            let second_artist = if distinct_artists {
                SECOND_ARTIST
            } else {
                SNAPSHOT_ARTIST
            };
            mirror.batch_execute(&format!(
                "INSERT INTO mb_artist VALUES ('{SNAPSHOT_ARTIST}', 'First', 'First', 'Person', 'US'), ('{SECOND_ARTIST}', 'Second', 'Second', 'Person', 'US');
                 INSERT INTO mb_release VALUES ('{SNAPSHOT_RELEASE}', 'First', 'Album', 2001, '{SNAPSHOT_ARTIST}'), ('{SECOND_RELEASE}', 'Second', 'Album', 2002, '{second_artist}');"
            )).await.unwrap();
            let attempts = Rc::new(Cell::new(0));
            let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts.clone());
            let phase = if distinct_artists {
                "artist"
            } else {
                "release"
            };
            let reached = mark_fetch_boundary(&store, phase);
            let result = store.subsonic_snapshot_catalogue_with_cap("u1", cap).await;
            assert!(reached.get(), "{phase} fetch boundary must be reachable");
            let (artists, releases, tracks) = result.unwrap();
            assert_eq!(artists.len(), if distinct_artists { 2 } else { 1 });
            assert_eq!(releases.len(), 2);
            assert!(tracks.is_empty());
            assert_eq!(attempts.get(), 1);
        }
    }
}

/// More member tracks than the native `CATALOGUE_MAX_PAGE_SIZE` (50): the
/// Subsonic snapshot read must return all of them, so a regression back to
/// the paged reads fails this test instead of silently dropping the 51st
/// song from the stream membership gate.
#[tokio::test]
async fn subsonic_snapshot_returns_every_member_track() {
    let recordings: Vec<String> = (1..=51u32)
        .map(|g| format!("{g:08x}-0000-4000-8000-{g:012x}"))
        .collect();
    let app = client(false).await;
    app.execute(
        "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', 'job-51', $1, $2)",
        &[&SNAPSHOT_RELEASE, &recordings],
    )
    .await
    .unwrap();
    let mirror = mirror_client().await;
    // Scalar MBIDs go in as SQL literals (synthetic constants): binding a
    // `&str` to a `$1::uuid` slot fails tokio-postgres's type check, so the
    // production code binds text arrays (`query_typed`, `Type::TEXT_ARRAY`)
    // instead.
    mirror
        .execute(
            &format!(
                "INSERT INTO mb_artist VALUES ('{SNAPSHOT_ARTIST}'::uuid, 'Artist A', 'A, Artist', 'Person', 'US')"
            ),
            &[],
        )
        .await
        .unwrap();
    mirror
        .execute(
            &format!(
                "INSERT INTO mb_release VALUES ('{SNAPSHOT_RELEASE}'::uuid, 'Album 51', 'Album', 2001, '{SNAPSHOT_ARTIST}'::uuid)"
            ),
            &[],
        )
        .await
        .unwrap();
    mirror
        .execute(
            "INSERT INTO mb_recording SELECT g::uuid, 'Song ' || g, 200000 FROM unnest($1::text[]) AS g",
            &[&recordings],
        )
        .await
        .unwrap();
    mirror
        .execute(
            &format!(
                "INSERT INTO mb_track SELECT '{SNAPSHOT_RELEASE}'::uuid, g::uuid, 1, position::smallint FROM unnest($1::text[]) WITH ORDINALITY AS tracks(g, position)"
            ),
            &[&recordings],
        )
        .await
        .unwrap();
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts.clone());
    let (artists, releases, tracks) = store.subsonic_snapshot_catalogue("u1").await.unwrap();
    assert_eq!(artists.len(), 1);
    assert_eq!(releases.len(), 1);
    assert_eq!(tracks.len(), 51, "the 50-row native page must not truncate");
    assert!(tracks.iter().any(|t| t.recording_mbid == recordings[50]));
    assert_eq!(attempts.get(), 1, "one mirror connection for three reads");
}

fn synthetic_uuid(kind: u32, n: u32) -> String {
    format!("{n:08x}-{kind:04x}-4000-8000-{n:012x}")
}

/// Per-field repeat counts (in characters of `filler`) for one synthetic
/// mirror fixture. Everything else stays a short constant.
#[derive(Clone, Copy)]
struct FieldLens {
    artist_name: i32,
    sort_name: i32,
    release_title: i32,
    track_title: i32,
}

const SHORT: FieldLens = FieldLens {
    artist_name: 1,
    sort_name: 1,
    release_title: 1,
    track_title: 1,
};

/// `n` member releases, each with its own artist and exactly one recording.
/// Returns the app client and mirror client ready for the production read.
async fn sized_fixture(n: u32, lens: FieldLens, filler: &str) -> (Client, Client) {
    let releases: Vec<String> = (1..=n).map(|i| synthetic_uuid(1, i)).collect();
    let recordings: Vec<String> = (1..=n).map(|i| synthetic_uuid(2, i)).collect();
    let app = client(false).await;
    app.execute(
        "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) \
         SELECT 'u1', r.release, r.release, ARRAY[r.recording] \
         FROM unnest($1::text[], $2::text[]) AS r(release, recording)",
        &[&releases, &recordings],
    )
    .await
    .unwrap();
    let mirror = mirror_client().await;
    mirror
        .execute(
            "INSERT INTO mb_artist SELECT r::uuid, repeat($2::text, $3::int), repeat($2::text, $4::int), 'Person', 'US' FROM unnest($1::text[]) AS r",
            &[&releases, &filler, &lens.artist_name, &lens.sort_name],
        )
        .await
        .unwrap();
    mirror
        .execute(
            "INSERT INTO mb_release SELECT r::uuid, repeat($2::text, $3::int), 'Album', 2001, r::uuid FROM unnest($1::text[]) AS r",
            &[&releases, &filler, &lens.release_title],
        )
        .await
        .unwrap();
    mirror
        .execute(
            "INSERT INTO mb_recording SELECT g::uuid, repeat($2::text, $3::int), 1000 FROM unnest($1::text[]) AS g",
            &[&recordings, &filler, &lens.track_title],
        )
        .await
        .unwrap();
    mirror
        .execute(
            "INSERT INTO mb_track SELECT r.release::uuid, r.recording::uuid, 1, 1 FROM unnest($1::text[], $2::text[]) AS r(release, recording)",
            &[&releases, &recordings],
        )
        .await
        .unwrap();
    (app, mirror)
}

fn mark_fetch_boundary(
    store: &impl SubsonicSnapshotCatalogueStore,
    phase: &'static str,
) -> Rc<Cell<bool>> {
    let reached = Rc::new(Cell::new(false));
    let marker = reached.clone();
    store.set_preflight_hook(
        phase,
        Box::new(move || {
            Box::pin(async move {
                marker.set(true);
                Ok(())
            })
        }),
    );
    reached
}

async fn snapshot_fetch_boundary(
    app: Client,
    mirror: Client,
    cap: i64,
    phase: &'static str,
) -> (bool, bool, usize) {
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts.clone());
    let reached = mark_fetch_boundary(&store, phase);
    let accepted = store
        .subsonic_snapshot_catalogue_with_cap("u1", cap)
        .await
        .is_ok();
    (accepted, reached.get(), attempts.get())
}

#[tokio::test]
async fn snapshot_fetch_markers_are_reachable_on_admitted_rows() {
    for phase in [
        "membership-release",
        "membership-pairs",
        "artist",
        "release",
        "track",
    ] {
        let (app, mirror) = sized_fixture(1, SHORT, "x").await;
        let attempts = Rc::new(Cell::new(0));
        let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts);
        let reached = mark_fetch_boundary(&store, phase);
        store
            .subsonic_snapshot_catalogue_with_cap("u1", 3)
            .await
            .unwrap();
        assert!(reached.get(), "{phase} fetch boundary must be reachable");
    }
}

async fn snapshot_of(
    app: Client,
    mirror: Client,
    cap: i64,
) -> (
    Result<
        (
            Vec<nntune_api::catalogue::ArtistDbRow>,
            Vec<nntune_api::catalogue::ReleaseDbRow>,
            Vec<nntune_api::catalogue::TrackDbRow>,
        ),
        StoreError,
    >,
    usize,
) {
    let attempts = Rc::new(Cell::new(0));
    let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts.clone());
    let result = store.subsonic_snapshot_catalogue_with_cap("u1", cap).await;
    (result, attempts.get())
}

async fn shared_connection(schema: &str) -> Result<Client, tokio_postgres::Error> {
    let config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("CI disposable database is required; never silently skip")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(config.get_hostaddrs().is_empty(), "no address overrides");
    assert!(
        matches!(config.get_hosts(), [tokio_postgres::config::Host::Tcp(host)]
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    );
    let (client, connection) = config.connect(NoTls).await?;
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute(&format!(
            "SET statement_timeout = '3s'; SET search_path = {schema}, pg_catalog"
        ))
        .await?;
    Ok(client)
}

async fn create_shared_app(client: &Client, schema: &str) -> Result<(), tokio_postgres::Error> {
    client.batch_execute(&format!(
        "CREATE SCHEMA {schema}; CREATE TABLE {schema}.library \
         (user_id text, job_id text, release_mbid text, recording_mbids text[], idempotency_key text PRIMARY KEY DEFAULT gen_random_uuid()::text)"
    )).await
}

async fn create_shared_mirror(client: &Client, schema: &str) -> Result<(), tokio_postgres::Error> {
    client.batch_execute(&format!(
        "CREATE SCHEMA {schema};
         CREATE TABLE {schema}.mb_artist (mbid uuid PRIMARY KEY, name text, sort_name text, type text, country text);
         CREATE TABLE {schema}.mb_release (mbid uuid PRIMARY KEY, title text, release_type text, year smallint, artist_mbid uuid);
         CREATE TABLE {schema}.mb_recording (mbid uuid PRIMARY KEY, title text, duration_ms integer);
         CREATE TABLE {schema}.mb_track (release_mbid uuid, recording_mbid uuid, medium_no smallint, track_no smallint, PRIMARY KEY (release_mbid, medium_no, track_no))"
    )).await
}

#[tokio::test]
async fn shared_fixture_writer_commit_is_visible_to_snapshot_reader() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Only generated identifiers are interpolated into DDL and search paths.
    let prefix = format!(
        "nntune_t_{}_{}_{}",
        std::process::id(),
        nonce,
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let app_schema = format!("{prefix}_a");
    let mirror_schema = format!("{prefix}_m");
    let app_writer = shared_connection(&app_schema).await.unwrap();
    let mirror_writer = shared_connection(&mirror_schema).await.unwrap();
    let mut app_created = false;
    let mut mirror_created = false;
    let result: Result<(), String> = async {
        create_shared_app(&app_writer, &app_schema).await.map_err(|e| e.to_string())?;
        app_created = true;
        create_shared_mirror(&mirror_writer, &mirror_schema).await.map_err(|e| e.to_string())?;
        mirror_created = true;
        app_writer.execute(
            "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', 'shared', $1, ARRAY[$2])",
            &[&SNAPSHOT_RELEASE, &RECORDING],
        ).await.map_err(|e| e.to_string())?;
        mirror_writer.batch_execute(&format!(
            "INSERT INTO mb_artist VALUES ('{SNAPSHOT_ARTIST}', 'Before', 'Sort', 'Person', 'US');
             INSERT INTO mb_release VALUES ('{SNAPSHOT_RELEASE}', 'Album', 'Album', 2001, '{SNAPSHOT_ARTIST}');
             INSERT INTO mb_recording VALUES ('{RECORDING}', 'Song', 1000);
             INSERT INTO mb_track VALUES ('{SNAPSHOT_RELEASE}', '{RECORDING}', 1, 1)"
        )).await.map_err(|e| e.to_string())?;
        // This independent autocommit update finishes before the reader connects.
        mirror_writer.execute("UPDATE mb_artist SET name = 'After'", &[]).await.map_err(|e| e.to_string())?;
        let app = shared_connection(&app_schema).await.map_err(|e| e.to_string())?;
        let mirror = shared_connection(&mirror_schema).await.map_err(|e| e.to_string())?;
        let visible: String = mirror.query_one("SELECT name FROM mb_artist", &[]).await.map_err(|e| e.to_string())?.get(0);
        let (snapshot, attempts) = snapshot_of(app, mirror, 10).await;
        let (artists, releases, tracks) = snapshot.map_err(|_| "snapshot failed".to_owned())?;
        if visible != "After" || attempts != 1 || artists.len() != 1 || releases.len() != 1 || tracks.len() != 1
            || artists[0].mbid != SNAPSHOT_ARTIST || artists[0].name != "After"
            || releases[0].mbid != SNAPSHOT_RELEASE || releases[0].title != "Album"
            || tracks[0].release_mbid != SNAPSHOT_RELEASE || tracks[0].recording_mbid != RECORDING || tracks[0].title != "Song" {
            return Err("shared fixture returned incomplete or incorrect values".to_owned());
        }
        Ok(())
    }.await;
    let app_cleanup = if app_created {
        app_writer
            .batch_execute(&format!("DROP SCHEMA {app_schema} CASCADE"))
            .await
    } else {
        Ok(())
    };
    let mirror_cleanup = if mirror_created {
        mirror_writer
            .batch_execute(&format!("DROP SCHEMA {mirror_schema} CASCADE"))
            .await
    } else {
        Ok(())
    };
    app_cleanup.unwrap();
    mirror_cleanup.unwrap();
    result.unwrap();
}

#[tokio::test]
async fn synthetic_fixture_round_trips_complete_catalogue() {
    let (app, mirror) = sized_fixture(2, SHORT, "x").await;
    let (result, attempts) = snapshot_of(app, mirror, 10).await;
    let (artists, releases, tracks) = result.unwrap();
    assert_eq!((artists.len(), releases.len(), tracks.len()), (2, 2, 2));
    for n in 1..=2 {
        let release = synthetic_uuid(1, n);
        let recording = synthetic_uuid(2, n);
        let artist = artists.iter().find(|row| row.mbid == release).unwrap();
        assert_eq!((&*artist.name, &*artist.sort_name), ("x", "x"));
        assert_eq!(artist.artist_type.as_deref(), Some("Person"));
        assert_eq!(artist.country.as_deref(), Some("US"));
        let album = releases.iter().find(|row| row.mbid == release).unwrap();
        assert_eq!(&album.title, "x");
        assert_eq!(album.artist_mbid.as_deref(), Some(release.as_str()));
        let track = tracks
            .iter()
            .find(|row| row.recording_mbid == recording)
            .unwrap();
        assert_eq!(
            (&*track.release_mbid, &*track.title),
            (release.as_str(), "x")
        );
        assert_eq!((track.medium_no, track.track_no), (Some(1), Some(1)));
    }
    assert_eq!(attempts, 1);
}

#[tokio::test]
async fn snapshot_membership_sentinels_reject_before_mirror() {
    for pair_mode in [false, true] {
        // Expose the four exact-cap members in two batches so each mirror
        // page stays within its separate cap of two rows.
        for (count, visible) in [(3_u32, 0..0), (4, 0..2), (4, 2..4), (5, 0..0)] {
            let ids: Vec<String> = (1..=count).map(|n| synthetic_uuid(1, n)).collect();
            let visible = &ids[visible];
            let app = client(false).await;
            let mirror = mirror_client().await;
            let phase = if pair_mode {
                app.execute(
                    "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', 'pairs', $1, $2)",
                    &[&SNAPSHOT_RELEASE, &ids],
                ).await.unwrap();
                mirror.execute(
                    "INSERT INTO mb_release VALUES ($1::text::uuid, 'Album', 'Album', 2001, NULL)",
                    &[&SNAPSHOT_RELEASE],
                ).await.unwrap();
                mirror.execute(
                    "INSERT INTO mb_recording SELECT g::uuid, 'Song', 1000 FROM unnest($1::text[]) AS g",
                    &[&visible],
                ).await.unwrap();
                mirror.execute(
                    "INSERT INTO mb_track SELECT $1::text::uuid, g::uuid, 1, position::smallint FROM unnest($2::text[]) WITH ORDINALITY AS tracks(g, position)",
                    &[&SNAPSHOT_RELEASE, &visible],
                ).await.unwrap();
                "membership-pairs"
            } else {
                app.execute(
                    "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) SELECT 'u1', r, r, ARRAY[]::text[] FROM unnest($1::text[]) AS r",
                    &[&ids],
                ).await.unwrap();
                mirror.execute(
                    "INSERT INTO mb_release SELECT r::uuid, 'Album', 'Album', 2001, NULL FROM unnest($1::text[]) AS r",
                    &[&visible],
                ).await.unwrap();
                "membership-release"
            };
            let attempts = Rc::new(Cell::new(0));
            let store = catalogue_store_for_postgres_test(app, true, Ok(mirror), attempts.clone());
            let reached = mark_fetch_boundary(&store, phase);
            let result = store.subsonic_snapshot_catalogue_with_cap("u1", 2).await;
            assert_eq!(
                reached.get(),
                count <= 4,
                "{phase} fetch boundary, count={count}"
            );
            if count > 4 {
                assert!(result.is_err(), "{phase} must reject the sentinel");
                assert_eq!(attempts.get(), 0);
            } else {
                let (_, releases, tracks) = result.unwrap();
                let mut returned: Vec<_> = if pair_mode {
                    tracks
                        .into_iter()
                        .map(|track| {
                            assert_eq!(track.release_mbid, SNAPSHOT_RELEASE);
                            track.recording_mbid
                        })
                        .collect()
                } else {
                    releases.into_iter().map(|release| release.mbid).collect()
                };
                returned.sort_unstable();
                assert_eq!(returned.as_slice(), visible, "{phase}, count={count}");
            }
        }
    }
    let app = client(false).await;
    let malformed: Vec<String> = (0..5).map(|n| format!("invalid-{n}")).collect();
    app.execute(
        "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', 'flood', $1, $2)",
        &[&SNAPSHOT_RELEASE, &malformed],
    ).await.unwrap();
    let (result, attempts) = snapshot_of(app, mirror_client().await, 2).await;
    assert!(
        result.is_err(),
        "malformed pairs must count before filtering"
    );
    assert_eq!(attempts, 0);
    for cap in [0, -1, i64::MAX] {
        let (result, attempts) = snapshot_of(client(false).await, mirror_client().await, cap).await;
        assert!(result.is_err());
        assert_eq!(attempts, 0);
    }
}

#[tokio::test]
async fn snapshot_overlong_membership_ids_keep_valid_neighbours() {
    let (app, mirror) = sized_fixture(1, SHORT, "a").await;
    let long = "x".repeat(5000);
    app.execute(
        "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', 'long-release', $1, ARRAY[$2]), ('u1', 'long-recording', $3, ARRAY[$1])",
        &[&long, &synthetic_uuid(2, 1), &synthetic_uuid(1, 1)],
    ).await.unwrap();
    let (result, attempts) = snapshot_of(app, mirror, 100).await;
    let (artists, releases, tracks) = result.unwrap();
    assert_eq!((artists.len(), releases.len(), tracks.len()), (1, 1, 1));
    assert_eq!(tracks[0].recording_mbid, synthetic_uuid(2, 1));
    assert_eq!(attempts, 1);
}

#[tokio::test]
async fn snapshot_mirror_field_octet_boundaries() {
    let cases: [fn(i32) -> FieldLens; 4] = [
        |n| FieldLens {
            artist_name: n,
            ..SHORT
        },
        |n| FieldLens {
            sort_name: n,
            ..SHORT
        },
        |n| FieldLens {
            release_title: n,
            ..SHORT
        },
        |n| FieldLens {
            track_title: n,
            ..SHORT
        },
    ];
    for (filler, ok, bad) in [("a", 1024, 1025), ("é", 512, 513)] {
        for (index, make) in cases.iter().enumerate() {
            let (app, mirror) = sized_fixture(1, make(ok), filler).await;
            let (result, _) = snapshot_of(app, mirror, 100).await;
            let (artists, releases, tracks) = result.unwrap();
            assert_eq!((artists.len(), releases.len(), tracks.len()), (1, 1, 1));
            let value = match index {
                0 => &artists[0].name,
                1 => &artists[0].sort_name,
                2 => &releases[0].title,
                _ => &tracks[0].title,
            };
            assert_eq!(*value, filler.repeat(ok as usize));
            let (app, mirror) = sized_fixture(1, make(bad), filler).await;
            let (result, _) = snapshot_of(app, mirror, 100).await;
            assert!(result.is_err(), "field {index} with {filler:?}");
        }
    }
}

#[tokio::test]
async fn snapshot_mirror_aggregate_boundaries() {
    // 245 * (36 + 1 + 6 + 2 + 1024) + (36 + 1 + 6 + 2 + 194) = 262144.
    for (last_length, accepted) in [(194_i32, true), (195, false)] {
        let (app, mirror) = sized_fixture(
            246,
            FieldLens {
                sort_name: 1024,
                ..SHORT
            },
            "a",
        )
        .await;
        mirror
            .execute(
                "UPDATE mb_artist SET sort_name = repeat('a', $1) WHERE mbid = $2::text::uuid",
                &[&last_length, &synthetic_uuid(1, 246)],
            )
            .await
            .unwrap();
        let (result, _) = snapshot_of(app, mirror, 1000).await;
        if accepted {
            let (artists, releases, tracks) = result.unwrap();
            assert_eq!(
                (artists.len(), releases.len(), tracks.len()),
                (246, 246, 246)
            );
            assert!(artists
                .iter()
                .any(|artist| artist.sort_name == "a".repeat(194)));
        } else {
            assert!(result.is_err());
        }
    }
    for lens in [
        FieldLens {
            artist_name: 1024,
            ..SHORT
        },
        FieldLens {
            sort_name: 1024,
            ..SHORT
        },
        FieldLens {
            release_title: 1024,
            ..SHORT
        },
        FieldLens {
            track_title: 1024,
            ..SHORT
        },
    ] {
        let (app, mirror) = sized_fixture(200, lens, "a").await;
        let (result, _) = snapshot_of(app, mirror, 1000).await;
        let (artists, releases, tracks) = result.unwrap();
        assert_eq!(
            (artists.len(), releases.len(), tracks.len()),
            (200, 200, 200)
        );
        let (app, mirror) = sized_fixture(257, lens, "a").await;
        let (result, _) = snapshot_of(app, mirror, 1000).await;
        assert!(result.is_err());
    }
}

#[tokio::test]
async fn snapshot_preflight_fetches_keep_the_admitted_snapshot() {
    use std::time::{SystemTime, UNIX_EPOCH};
    for (phase, update, select) in [
        (
            "artist",
            "UPDATE mb_artist SET name = repeat('x', 6)",
            "SELECT name FROM mb_artist",
        ),
        (
            "release",
            "UPDATE mb_release SET title = repeat('x', 6)",
            "SELECT title FROM mb_release",
        ),
        (
            "track",
            "UPDATE mb_recording SET title = repeat('x', 6)",
            "SELECT title FROM mb_recording",
        ),
    ] {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let schema = format!("nntune_preflight_{}_{}", std::process::id(), nonce);
        let writer = shared_connection(&schema).await.unwrap();
        create_shared_mirror(&writer, &schema).await.unwrap();
        let result: Result<(), String> = async {
            writer.batch_execute(&format!(
                "INSERT INTO mb_artist VALUES ('{SNAPSHOT_ARTIST}', 'Artist', 'Sort', 'Person', 'US');
                 INSERT INTO mb_release VALUES ('{SNAPSHOT_RELEASE}', 'Album', 'Album', 2001, '{SNAPSHOT_ARTIST}');
                 INSERT INTO mb_recording VALUES ('{RECORDING}', 'Song', 1000);
                 INSERT INTO mb_track VALUES ('{SNAPSHOT_RELEASE}', '{RECORDING}', 1, 1)"
            )).await.map_err(|e| e.to_string())?;
            let app = client(false).await;
            app.execute("INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', 'original', $1, ARRAY[$2])", &[&SNAPSHOT_RELEASE, &RECORDING]).await.map_err(|e| e.to_string())?;
            let reader = shared_connection(&schema).await.map_err(|e| e.to_string())?;
            let mutation = shared_connection(&schema).await.map_err(|e| e.to_string())?;
            let store = catalogue_store_for_postgres_test(app, true, Ok(reader), Rc::new(Cell::new(0)));
            store.set_preflight_hook(phase, Box::new(move || Box::pin(async move {
                mutation.batch_execute(update).await.map_err(|_| StoreError)?;
                Ok(())
            })));
            let (artists, releases, tracks) = store.subsonic_snapshot_catalogue_with_cap("u1", 10).await.map_err(|_| format!("{phase}: snapshot failed"))?;
            let changed: String = writer.query_one(select, &[]).await.map_err(|e| e.to_string())?.get(0);
            if changed != "xxxxxx" || artists.len() != 1 || releases.len() != 1 || tracks.len() != 1
                || artists[0].mbid != SNAPSHOT_ARTIST || artists[0].name != "Artist"
                || releases[0].mbid != SNAPSHOT_RELEASE || releases[0].title != "Album"
                || tracks[0].recording_mbid != RECORDING || tracks[0].title != "Song" {
                return Err(format!("{phase}: writer or snapshot mismatch"));
            }
            Ok(())
        }.await;
        writer
            .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
            .await
            .unwrap();
        result.unwrap();
    }
}

#[tokio::test]
async fn snapshot_mirror_queries_share_one_committed_snapshot() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let schema = format!("nntune_mirror_{}_{}", std::process::id(), nonce);
    let writer = shared_connection(&schema).await.unwrap();
    create_shared_mirror(&writer, &schema).await.unwrap();
    let result: Result<(), String> = async {
        writer.batch_execute(&format!(
            "INSERT INTO mb_artist VALUES ('{SNAPSHOT_ARTIST}', 'Artist', 'Sort', 'Person', 'US');
             INSERT INTO mb_release VALUES ('{SNAPSHOT_RELEASE}', 'Album', 'Album', 2001, '{SNAPSHOT_ARTIST}');
             INSERT INTO mb_recording VALUES ('{RECORDING}', 'Song', 1000);
             INSERT INTO mb_track VALUES ('{SNAPSHOT_RELEASE}', '{RECORDING}', 1, 1)"
        )).await.map_err(|e| e.to_string())?;
        let app = client(false).await;
        app.execute(
            "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', 'original', $1, ARRAY[$2])",
            &[&SNAPSHOT_RELEASE, &RECORDING],
        ).await.map_err(|e| e.to_string())?;
        let reader = shared_connection(&schema).await.map_err(|e| e.to_string())?;
        let mutation = shared_connection(&schema).await.map_err(|e| e.to_string())?;
        let store = catalogue_store_for_postgres_test(app, true, Ok(reader), Rc::new(Cell::new(0)));
        store.set_mirror_hook(Box::new(move || Box::pin(async move {
            mutation.batch_execute("UPDATE mb_release SET title = 'Changed'; UPDATE mb_recording SET title = 'Changed'")
                .await.map_err(|_| StoreError)?;
            Ok(())
        })));
        let (artists, releases, tracks) = store.subsonic_snapshot_catalogue_with_cap("u1", 10)
            .await.map_err(|_| "snapshot failed".to_owned())?;
        let release_title: String = writer.query_one("SELECT title FROM mb_release", &[]).await.map_err(|e| e.to_string())?.get(0);
        let track_title: String = writer.query_one("SELECT title FROM mb_recording", &[]).await.map_err(|e| e.to_string())?.get(0);
        if release_title != "Changed" || track_title != "Changed"
            || artists.len() != 1 || releases.len() != 1 || tracks.len() != 1
            || artists[0].mbid != SNAPSHOT_ARTIST || artists[0].name != "Artist"
            || releases[0].mbid != SNAPSHOT_RELEASE || releases[0].title != "Album"
            || tracks[0].recording_mbid != RECORDING || tracks[0].title != "Song" {
            return Err("mirror snapshot mixed old and new states".into());
        }
        Ok(())
    }.await;
    writer
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
    result.unwrap();
}

#[tokio::test]
async fn snapshot_membership_queries_share_one_committed_snapshot() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let schema = format!("nntune_member_{}_{}", std::process::id(), nonce);
    let writer = shared_connection(&schema).await.unwrap();
    create_shared_app(&writer, &schema).await.unwrap();
    let result: Result<(), String> = async {
        writer.execute(
            "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', 'original', $1, ARRAY[$2])",
            &[&SNAPSHOT_RELEASE, &RECORDING],
        ).await.map_err(|e| e.to_string())?;
        let reader = shared_connection(&schema).await.map_err(|e| e.to_string())?;
        let mirror = mirror_client().await;
        mirror.batch_execute(&format!(
            "INSERT INTO mb_artist VALUES ('{SNAPSHOT_ARTIST}', 'Artist', 'Sort', 'Person', 'US');
             INSERT INTO mb_release VALUES ('{SNAPSHOT_RELEASE}', 'Album', 'Album', 2001, '{SNAPSHOT_ARTIST}');
             INSERT INTO mb_recording VALUES ('{RECORDING}', 'Song', 1000);
             INSERT INTO mb_track VALUES ('{SNAPSHOT_RELEASE}', '{RECORDING}', 1, 1)"
        )).await.map_err(|e| e.to_string())?;
        let mutation = shared_connection(&schema).await.map_err(|e| e.to_string())?;
        let attempts = Rc::new(Cell::new(0));
        let store = catalogue_store_for_postgres_test(reader, true, Ok(mirror), attempts);
        store.set_membership_hook(Box::new(move || Box::pin(async move {
            mutation.execute("UPDATE library SET recording_mbids = ARRAY[]::text[]", &[])
                .await.map_err(|_| StoreError)?;
            Ok(())
        })));
        let (artists, releases, tracks) = store.subsonic_snapshot_catalogue_with_cap("u1", 10)
            .await.map_err(|_| "snapshot failed".to_owned())?;
        let committed: Vec<String> = writer.query_one("SELECT recording_mbids FROM library", &[])
            .await.map_err(|e| e.to_string())?.get(0);
        if !committed.is_empty() || artists.len() != 1 || releases.len() != 1 || tracks.len() != 1
            || releases[0].mbid != SNAPSHOT_RELEASE || tracks[0].recording_mbid != RECORDING {
            return Err("membership snapshot mixed old and new states".into());
        }
        Ok(())
    }.await;
    writer
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
    result.unwrap();
}

#[tokio::test]
async fn private_session_delete_is_scoped_and_rolls_back() {
    let accounts = disposable_accounts::Accounts::new(2).await;
    let owner = accounts.id(0).to_owned();
    let other = accounts.id(1).to_owned();
    let observer = &accounts.observer;
    let result: Result<(), String> = async {
        observer.execute("INSERT INTO public.private_sessions VALUES ($1,'A',1), ($1,'a',2), ($1,'keep',3), ($2,'A',4)", &[&owner, &other]).await.map_err(|e| e.to_string())?;
        let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
        let reached = Rc::new(Cell::new(false));
        let marker = reached.clone();
        store.set_preflight_hook("private-session-delete", Box::new(move || Box::pin(async move {
            marker.set(true);
            Err(StoreError)
        })));
        if !matches!(store.delete_private_session(&owner, "A").await, Err(StoreError)) || !reached.get() {
            return Err("delete rollback injection was not reached".into());
        }
        // Acquire the account fence before checking the rollback.
        observer.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
        observer.query_one("SELECT id FROM public.users WHERE id = $1 FOR UPDATE", &[&owner]).await.map_err(|e| e.to_string())?;
        let count: i64 = observer.query_one("SELECT count(*) FROM public.private_sessions WHERE user_id = $1 AND session_id = 'A'", &[&owner]).await.map_err(|e| e.to_string())?.get(0);
        observer.batch_execute("COMMIT").await.map_err(|e| e.to_string())?;
        if count != 1 { return Err("delete did not roll back".into()); }
        for (id, session) in [(&owner, "A"), (&owner, "A"), (&owner, "missing"), (&other, "a")] {
            let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
            store.delete_private_session(id, session).await.map_err(|_| "valid delete failed".to_string())?;
        }
        let missing = format!("{owner}-missing");
        observer.execute("UPDATE public.users SET deleting_at = now() WHERE id = $1", &[&owner]).await.map_err(|e| e.to_string())?;
        for id in [&owner, &missing] {
            let store = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
            if !matches!(store.delete_private_session(id, "a").await, Err(StoreError)) { return Err("unavailable account accepted".into()); }
        }
        let failed = private_client().await?;
        failed.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
        if failed.batch_execute("SELECT 1/0").await.is_ok() { return Err("failure injection failed".into()); }
        let store = catalogue_store_for_postgres_test(failed, false, Err(StoreError), Rc::new(Cell::new(0)));
        if !matches!(store.delete_private_session(&other, "A").await, Err(StoreError)) { return Err("database error was not opaque".into()); }
        drop(store);
        let rows = observer.query("SELECT user_id,session_id,expires_at FROM public.private_sessions WHERE user_id IN ($1,$2)", &[&owner, &other]).await.map_err(|e| e.to_string())?;
        let mut actual: Vec<(String,String,i64)> = rows.iter().map(|r| (r.get(0),r.get(1),r.get(2))).collect();
        let mut expected = vec![(owner.clone(),"a".into(),2), (owner.clone(),"keep".into(),3), (other.clone(),"A".into(),4)];
        actual.sort(); expected.sort();
        if actual != expected { return Err("delete changed unrelated sessions".into()); }
        Ok(())
    }.await;
    let _ = observer.batch_execute("ROLLBACK").await;
    accounts.cleanup().await;
    result.unwrap();
}

#[tokio::test]
async fn private_session_delete_respects_account_fence() {
    let accounts = disposable_accounts::Accounts::new(2).await;
    let owner = accounts.id(0).to_owned();
    let other = accounts.id(1).to_owned();
    let observer = &accounts.observer;
    let locker = client(false).await;
    let result: Result<(), String> = async {
        observer.execute("INSERT INTO public.private_sessions VALUES ($1,'target',1), ($2,'target',2)", &[&owner, &other]).await.map_err(|e| e.to_string())?;
        for deleting in [true, false] {
            locker.batch_execute("BEGIN").await.map_err(|e| e.to_string())?;
            if deleting {
                locker.execute("UPDATE public.users SET deleting_at = now() WHERE id = $1", &[&owner]).await.map_err(|e| e.to_string())?;
            } else {
                locker.query_one("SELECT id FROM public.users WHERE id = $1 FOR UPDATE", &[&owner]).await.map_err(|e| e.to_string())?;
            }
            let locker_pid: i32 = locker.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
            let writer = private_client().await?;
            let writer_pid: i32 = writer.query_one("SELECT pg_backend_pid()", &[]).await.map_err(|e| e.to_string())?.get(0);
            let store = catalogue_store_for_postgres_test(writer, false, Err(StoreError), Rc::new(Cell::new(0)));
            let delete = store.delete_private_session(&owner, if deleting { "missing" } else { "target" });
            tokio::pin!(delete);
            blocker_observer::observe_blocked(observer, locker_pid, writer_pid, delete.as_mut()).await?;
            let unrelated = catalogue_store_for_postgres_test(private_client().await?, false, Err(StoreError), Rc::new(Cell::new(0)));
            unrelated.delete_private_session(&other, "missing").await.map_err(|_| "unrelated account blocked".to_string())?;
            let count: i64 = observer.query_one("SELECT count(*) FROM public.private_sessions WHERE user_id = $1 AND session_id = 'target'", &[&owner]).await.map_err(|e| e.to_string())?.get(0);
            if count != 1 { return Err("delete ran before account lock released".into()); }
            locker.batch_execute("COMMIT").await.map_err(|e| e.to_string())?;
            if delete.await.is_err() != deleting { return Err("unexpected result after account fence".into()); }
            if deleting { observer.execute("UPDATE public.users SET deleting_at = NULL WHERE id = $1", &[&owner]).await.map_err(|e| e.to_string())?; }
        }
        let rows = observer.query("SELECT user_id FROM public.private_sessions WHERE user_id IN ($1,$2)", &[&owner, &other]).await.map_err(|e| e.to_string())?;
        if rows.len() != 1 || rows[0].get::<_, String>(0) != other { return Err("unexpected rows after account fence".into()); }
        Ok(())
    }.await;
    let _ = locker.batch_execute("ROLLBACK").await;
    accounts.cleanup().await;
    result.unwrap();
}
