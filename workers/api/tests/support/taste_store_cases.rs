//! Exercises the shipped preference SQL against CI's migrated disposable DB.
use nntune_api::taste::*;
use tokio_postgres::{Config, NoTls};

#[tokio::test]
async fn preference_sql_is_owner_scoped_replay_safe_and_erased_with_account() {
    let config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("CI disposable database is required; never silently skip")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(config.get_hostaddrs().is_empty());
    assert!(
        matches!(config.get_hosts(), [tokio_postgres::config::Host::Tcp(host)]
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    );
    let (mut client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL statement_timeout = '3s';
        INSERT INTO public.users (id, email, created_at) VALUES
          ('taste-a','taste-a@example.invalid',1000), ('taste-b','taste-b@example.invalid',1000);
        INSERT INTO public.library (idempotency_key,job_id,user_id,release_mbid,recording_mbids) VALUES
          ('taste-owned','owned-job','taste-a','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',ARRAY['bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb']),
          ('taste-shared','shared-job',NULL,'cccccccc-cccc-4ccc-8ccc-cccccccccccc',ARRAY[]::text[]);")
        .await.unwrap();
    let a = "taste-a";
    let b = "taste-b";
    let track = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    for (owner, id, expected) in [
        (a, track, true),
        (b, track, false),
        (a, "owned-job", true),
        (b, "owned-job", false),
        (b, "shared-job", true),
    ] {
        assert_eq!(
            !tx.query(member_track_target_statement(), &[&owner, &id])
                .await
                .unwrap()
                .is_empty(),
            expected
        );
    }
    for (owner, release, expected) in [
        (a, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", true),
        (b, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", false),
        (b, "cccccccc-cccc-4ccc-8ccc-cccccccccccc", true),
    ] {
        assert_eq!(
            !tx.query(member_release_target_statement(), &[&owner, &release])
                .await
                .unwrap()
                .is_empty(),
            expected
        );
    }
    for now in [1000_i64, 1001] {
        assert_eq!(
            tx.query(
                taste_upsert_statement(),
                &[&a, &"track", &track, &"love", &None::<i64>, &now]
            )
            .await
            .unwrap()
            .len(),
            1
        );
    }
    let row = tx
        .query_one(taste_get_statement(), &[&a, &"track", &track])
        .await
        .unwrap();
    assert_eq!(row.get::<_, i64>("created_at"), 1000);
    assert_eq!(row.get::<_, i64>("updated_at"), 1001);
    assert!(tx
        .query(taste_get_statement(), &[&b, &"track", &track])
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        tx.query(like_delete_statement(), &[&b, &"track", &track])
            .await
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        tx.query(likes_list_statement(), &[&a, &500_i64])
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(tx
        .query(likes_list_statement(), &[&b, &500_i64])
        .await
        .unwrap()
        .is_empty());

    tx.query(
        taste_upsert_statement(),
        &[&a, &"track", &track, &"snooze", &Some(1060_i64), &1002_i64],
    )
    .await
    .unwrap()
    .len();
    assert_eq!(
        tx.query(like_delete_statement(), &[&a, &"track", &track])
            .await
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        tx.query(taste_list_statement(), &[&a, &1059_i64, &500_i64])
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(tx
        .query(taste_list_statement(), &[&a, &1060_i64, &500_i64])
        .await
        .unwrap()
        .is_empty());

    tx.query(
        private_session_upsert_statement(),
        &[&a, &"same-session", &2000_i64],
    )
    .await
    .unwrap()
    .len();
    for (owner, now, expected) in [(a, 1999_i64, true), (a, 2000, false), (b, 1999, false)] {
        assert_eq!(
            !tx.query(
                private_session_active_statement(),
                &[&owner, &"same-session", &now]
            )
            .await
            .unwrap()
            .is_empty(),
            expected
        );
    }
    let private_key = "play:taste-a:owned-job:private";
    assert_eq!(
        tx.query(
            private_play_mark_statement(),
            &[&a, &private_key, &1000_i64]
        )
        .await
        .unwrap()
        .len(),
        1
    );
    assert_eq!(
        tx.query(
            private_play_mark_statement(),
            &[&a, &private_key, &1000_i64]
        )
        .await
        .unwrap()
        .len(),
        0
    );
    assert_eq!(
        tx.query(private_play_probe_statement(), &[&a, &private_key])
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(tx
        .query(private_play_probe_statement(), &[&b, &private_key])
        .await
        .unwrap()
        .is_empty());
    // Marks alone are not history. Real metered bytes remain billable even
    // when the corresponding play is excluded from history.
    assert!(tx
        .query(history_list_statement(), &[&a, &50_i64])
        .await
        .unwrap()
        .is_empty());
    tx.batch_execute("INSERT INTO public.stream_play_ledger
        (idempotency_key,user_id,track_id,period_month,stream_seconds,bytes_served,recorded_at) VALUES
        ('play:taste-a:owned-job:private','taste-a','owned-job','1970-01',1,48000,1000),
        ('play:taste-a:owned-job:public','taste-a','owned-job','1970-01',1,48000,1001),
        ('legacy-random-window','taste-a','owned-job','1970-01',1,48000,1002),
        ('play:taste-b:shared-job:other','taste-b','shared-job','1970-01',1,48000,1003);")
        .await.unwrap();
    let rows = tx
        .query(history_list_statement(), &[&a, &50_i64])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get::<_, String>("idempotency_key"),
        "play:taste-a:owned-job:public"
    );
    assert_eq!(
        rows[0].get::<_, i64>("stream_seconds"),
        1,
        "production row decoder expects int8"
    );
    let billed: i64 = tx
        .query_one(
            "SELECT sum(stream_seconds)::bigint FROM public.stream_play_ledger WHERE user_id = $1",
            &[&a],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(billed, 3);
    tx.query(private_session_delete_statement(), &[&b, &"same-session"])
        .await
        .unwrap()
        .len();
    assert_eq!(
        tx.query(
            private_session_active_statement(),
            &[&a, &"same-session", &1999_i64]
        )
        .await
        .unwrap()
        .len(),
        1
    );

    // created_at is the like time: same-love replay keeps it, any
    // non-love -> love transition (expired snooze included) restamps it.
    let other = "transition-target";
    for (signal, until, now, created) in [
        ("hide", None, 2000_i64, 2000_i64),
        ("love", None, 2010, 2010),
        ("love", None, 2020, 2010),
        ("snooze", Some(2025_i64), 2030, 2010),
        ("love", None, 2040, 2040),
        ("love", None, 2050, 2040),
    ] {
        assert_eq!(
            tx.query(
                taste_upsert_statement(),
                &[&a, &"track", &other, &signal, &until, &now]
            )
            .await
            .unwrap()
            .len(),
            1
        );
        let row = tx
            .query_one(taste_get_statement(), &[&a, &"track", &other])
            .await
            .unwrap();
        assert_eq!(row.get::<_, i64>("created_at"), created, "{signal}@{now}");
        assert_eq!(row.get::<_, i64>("updated_at"), now);
    }
    let liked = tx
        .query(likes_list_statement(), &[&a, &500_i64])
        .await
        .unwrap();
    assert_eq!(liked[0].get::<_, String>("target_id"), other);
    tx.query(like_delete_statement(), &[&a, &"track", &other])
        .await
        .unwrap()
        .len();

    tx.execute(
        "UPDATE public.users SET deleting_at = to_timestamp(1100) WHERE id = $1",
        &[&a],
    )
    .await
    .unwrap();
    assert_eq!(
        tx.query(
            taste_upsert_statement(),
            &[
                &a,
                &"track",
                &"new-target",
                &"love",
                &None::<i64>,
                &1101_i64
            ]
        )
        .await
        .unwrap()
        .len(),
        0
    );
    assert_eq!(
        tx.query(
            private_session_upsert_statement(),
            &[&a, &"new-session", &2000_i64]
        )
        .await
        .unwrap()
        .len(),
        0
    );
    assert_eq!(
        tx.query(
            private_play_mark_statement(),
            &[&a, &"play:taste-a:owned-job:new", &1101_i64]
        )
        .await
        .unwrap()
        .len(),
        0
    );
    assert_eq!(
        tx.query(taste_delete_statement(), &[&a, &"track", &track])
            .await
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        tx.query(like_delete_statement(), &[&a, &"track", &track])
            .await
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        tx.query(private_session_delete_statement(), &[&a, &"same-session"])
            .await
            .unwrap()
            .len(),
        0
    );
    // Library's pre-existing RESTRICT relationship is erased by its existing
    // executor; the new account-owned data must then cascade with the user.
    tx.execute("DELETE FROM public.library WHERE user_id = $1", &[&a])
        .await
        .unwrap();
    tx.execute("DELETE FROM public.users WHERE id = $1", &[&a])
        .await
        .unwrap();
    for sql in [
        "SELECT count(*) FROM public.account_taste WHERE user_id = $1",
        "SELECT count(*) FROM public.private_sessions WHERE user_id = $1",
        "SELECT count(*) FROM public.private_plays WHERE user_id = $1",
    ] {
        assert_eq!(tx.query_one(sql, &[&a]).await.unwrap().get::<_, i64>(0), 0);
    }
    assert_eq!(
        tx.query(history_list_statement(), &[&b, &50_i64])
            .await
            .unwrap()
            .len(),
        1
    );
    tx.rollback().await.unwrap();
}

/// Privacy activation / new-acquisition race, through the real `PgStore`
/// methods (only connection acquisition is injected). Unlike the test above,
/// these COMMIT to CI's disposable `nntune_ci`, so every row is namespaced by
/// a per-test id prefix, removed before and after, and each connection is
/// named so `pg_stat_activity` can prove who is blocked on the account row.
/// Blocking is observed, never slept for: waits poll the lock state under a
/// hard deadline.
mod privacy_race {
    use nntune_api::handlers::{Store, StoreError};
    use nntune_api::playback::LeaseAcquire;
    use nntune_api::worker_entry::catalogue_store_for_postgres_test;
    use std::cell::Cell;
    use std::future::Future;
    use std::rc::Rc;
    use std::time::{Duration, Instant};
    use tokio::task::{spawn_local, JoinHandle, LocalSet};
    use tokio_postgres::{Client, Config, NoTls};

    const NOW: u64 = 1_800_000_000;
    const TTL: u64 = 45;
    const PRIVATE_UNTIL: u64 = NOW + 3_600;
    const DEADLINE: Duration = Duration::from_secs(10);

    /// Unique per operation and account, so concurrently running cases never
    /// observe each other's blocked backends.
    fn app_name(app: &str, user: &str) -> String {
        format!("{app}-{user}")
    }

    async fn connect(app: &str) -> Client {
        let mut config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
            .expect("CI disposable database is required; never silently skip")
            .parse()
            .unwrap();
        assert_eq!(config.get_dbname(), Some("nntune_ci"));
        assert!(config.get_hostaddrs().is_empty(), "no address overrides");
        assert!(
            matches!(config.get_hosts(), [tokio_postgres::config::Host::Tcp(host)]
            if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
        );
        config.application_name(app);
        let (client, connection) = config.connect(NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap() });
        // Default search_path: the production statements resolve `users`.
        client
            .batch_execute("SET statement_timeout = '15s'")
            .await
            .unwrap();
        client
    }

    /// A real `PgStore` over a dedicated named connection. A store spends its
    /// single injected connection on its first call, so use one per call.
    async fn store(app: &str) -> impl Store {
        catalogue_store_for_postgres_test(
            connect(app).await,
            false,
            Err(StoreError),
            Rc::new(Cell::new(0)),
        )
    }

    async fn bounded<T>(handle: JoinHandle<T>) -> T {
        tokio::time::timeout(DEADLINE, handle)
            .await
            .expect("operation did not finish before the deadline")
            .expect("operation task panicked")
    }

    #[derive(Clone)]
    struct Env {
        obs: Rc<Client>,
        prefix: &'static str,
    }

    impl Env {
        async fn user(&self, name: &str) -> String {
            let id = format!("{}{name}", self.prefix);
            self.obs
                .execute(
                    "INSERT INTO public.users (id, email, created_at) VALUES ($1, $2, 1000)",
                    &[&id, &format!("{id}@example.invalid")],
                )
                .await
                .unwrap();
            id
        }

        fn key(user: &str, track: &str, play: &str) -> String {
            nntune_api::ledger::derive_play_idempotency_key(user, track, play).unwrap()
        }

        async fn marks(&self, user: &str) -> Vec<String> {
            self.obs
                .query(
                    "SELECT play_key FROM public.private_plays WHERE user_id = $1 ORDER BY play_key",
                    &[&user],
                )
                .await
                .unwrap()
                .iter()
                .map(|r| r.get(0))
                .collect()
        }

        async fn lease(&self, user: &str) -> Option<(String, String, i64)> {
            self.obs
                .query_opt(
                    "SELECT session_id, play_id, epoch FROM public.playback_leases WHERE user_id = $1",
                    &[&user],
                )
                .await
                .unwrap()
                .map(|r| (r.get(0), r.get(1), r.get(2)))
        }

        async fn sessions(&self, user: &str) -> i64 {
            self.obs
                .query_one(
                    "SELECT count(*) FROM public.private_sessions WHERE user_id = $1",
                    &[&user],
                )
                .await
                .unwrap()
                .get(0)
        }

        /// Hold the account row exactly as a concurrent writer would, in an
        /// open transaction on its own connection.
        async fn hold(&self, user: &str) -> Client {
            let holder = connect(&app_name("race-holder", user)).await;
            holder.batch_execute("BEGIN").await.unwrap();
            holder
                .query_one(
                    "SELECT id FROM public.users WHERE id = $1 FOR UPDATE",
                    &[&user],
                )
                .await
                .unwrap();
            holder
        }

        /// Wait (bounded, polling the lock manager) until the named
        /// connection is parked on a lock.
        async fn blocked(&self, app: &str, user: &str) {
            let app = app_name(app, user);
            let start = Instant::now();
            loop {
                let n: i64 = self
                    .obs
                    .query_one(
                        "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND application_name = $1 AND application_name LIKE $2 AND wait_event_type = 'Lock'",
                        &[&app, &format!("%{}%", self.prefix)],
                    )
                    .await
                    .unwrap()
                    .get(0);
                if n > 0 {
                    return;
                }
                assert!(start.elapsed() < DEADLINE, "{app} never blocked on a lock");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        async fn cleanup(&self, fault: bool) {
            let pattern = format!("{}%", self.prefix);
            if fault {
                self.obs
                    .batch_execute(
                        "DROP TRIGGER IF EXISTS race_fault ON public.private_plays;
                         DROP FUNCTION IF EXISTS public.race_fault_private_plays();",
                    )
                    .await
                    .unwrap();
            }
            // Ledger rows have no cascade from users; the rest cascade.
            self.obs
                .execute(
                    "DELETE FROM public.stream_play_ledger WHERE user_id LIKE $1",
                    &[&pattern],
                )
                .await
                .unwrap();
            self.obs
                .execute("DELETE FROM public.users WHERE id LIKE $1", &[&pattern])
                .await
                .unwrap();
        }
    }

    /// Run one case on a local set with guaranteed pre/post cleanup (also on
    /// panic: the body is awaited as a task and the panic is re-raised after).
    async fn run<F, Fut>(prefix: &'static str, fault: bool, body: F)
    where
        F: FnOnce(Env) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        LocalSet::new()
            .run_until(async {
                let env = Env {
                    obs: Rc::new(connect(&format!("race-observer-{prefix}")).await),
                    prefix,
                };
                env.cleanup(fault).await;
                let outcome = spawn_local(body(env.clone())).await;
                env.cleanup(fault).await;
                if let Err(e) = outcome {
                    std::panic::resume_unwind(e.into_panic());
                }
            })
            .await;
    }

    fn upsert(
        app: &'static str,
        user: &str,
        session: &str,
    ) -> JoinHandle<Result<bool, StoreError>> {
        let (user, session) = (user.to_owned(), session.to_owned());
        spawn_local(async move {
            store(&app_name(app, &user))
                .await
                .private_session_upsert(&user, &session, PRIVATE_UNTIL)
                .await
        })
    }

    fn acquire(
        app: &'static str,
        user: &str,
        session: &str,
        track: &str,
        play: &str,
        takeover: bool,
    ) -> JoinHandle<Result<LeaseAcquire, StoreError>> {
        let (user, session, track, play) = (
            user.to_owned(),
            session.to_owned(),
            track.to_owned(),
            play.to_owned(),
        );
        spawn_local(async move {
            store(&app_name(app, &user))
                .await
                .acquire_playback_lease(&user, &session, &track, &play, NOW, NOW + TTL, takeover)
                .await
        })
    }

    fn acquired(r: Result<LeaseAcquire, StoreError>, epoch: u64) {
        match r {
            Ok(LeaseAcquire::Acquired { epoch: got }) => assert_eq!(got, epoch),
            Ok(LeaseAcquire::Held { .. }) => panic!("expected Acquired, got Held"),
            Err(_) => panic!("expected Acquired, got a store error"),
        }
    }

    #[tokio::test]
    async fn activation_then_new_acquire_block_together_and_the_play_is_marked() {
        run("race-a-", false, |env| async move {
            let u = env.user("u").await;
            let holder = env.hold(&u).await;
            let act = upsert("race-act", &u, "s1");
            env.blocked("race-act", &u).await;
            let acq = acquire("race-acq", &u, "s1", "t1", "p1", false);
            env.blocked("race-acq", &u).await;
            assert!(!act.is_finished() && !acq.is_finished());
            assert_eq!(env.sessions(&u).await, 0);
            assert!(env.lease(&u).await.is_none());

            holder.batch_execute("COMMIT").await.unwrap();
            assert!(bounded(act).await.unwrap());
            acquired(bounded(acq).await, 1);
            assert_eq!(env.marks(&u).await, [Env::key(&u, "t1", "p1")]);

            // The same session's next NEW acquisition stays private.
            acquired(
                bounded(acquire("race-acq2", &u, "s1", "t2", "p2", false)).await,
                2,
            );
            assert_eq!(
                env.marks(&u).await,
                [Env::key(&u, "t1", "p1"), Env::key(&u, "t2", "p2")]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn acquire_queued_before_activation_is_public_and_continuation_stays_public() {
        run("race-b-", false, |env| async move {
            let u = env.user("u").await;
            let holder = env.hold(&u).await;
            let acq = acquire("race-acq", &u, "s1", "t1", "p1", false);
            env.blocked("race-acq", &u).await;
            let act = upsert("race-act", &u, "s1");
            env.blocked("race-act", &u).await;

            holder.batch_execute("COMMIT").await.unwrap();
            acquired(bounded(acq).await, 1);
            assert!(bounded(act).await.unwrap());
            // Serialised before activation: this play was legitimately public.
            assert!(env.marks(&u).await.is_empty());

            // Continuation after the toggle is neither marked nor refused.
            let touched = store(&app_name("race-touch", &u))
                .await
                .touch_playback_lease(&u, "s1", "p1", 1, NOW + 10, NOW + 10 + TTL)
                .await;
            assert!(touched.unwrap());
            assert!(env.marks(&u).await.is_empty());

            acquired(
                bounded(acquire("race-acq2", &u, "s1", "t2", "p2", false)).await,
                2,
            );
            assert_eq!(env.marks(&u).await, [Env::key(&u, "t2", "p2")]);
        })
        .await;
    }

    #[tokio::test]
    async fn continuation_of_a_private_play_stays_private_after_deactivation() {
        run("race-c-", false, |env| async move {
            let u = env.user("u").await;
            assert!(bounded(upsert("race-act", &u, "s1")).await.unwrap());
            acquired(
                bounded(acquire("race-acq", &u, "s1", "t1", "p1", false)).await,
                1,
            );
            let private = [Env::key(&u, "t1", "p1")];
            assert_eq!(env.marks(&u).await, private);

            store(&app_name("race-del", &u))
                .await
                .private_session_delete(&u, "s1")
                .await
                .unwrap();
            assert_eq!(env.sessions(&u).await, 0);
            let touched = store(&app_name("race-touch", &u))
                .await
                .touch_playback_lease(&u, "s1", "p1", 1, NOW + 10, NOW + 10 + TTL)
                .await;
            assert!(touched.unwrap());
            assert_eq!(env.marks(&u).await, private, "mark is never un-set");

            // A NEW play after deactivation is public.
            acquired(
                bounded(acquire("race-acq2", &u, "s1", "t2", "p2", false)).await,
                2,
            );
            assert_eq!(env.marks(&u).await, private);
        })
        .await;
    }

    #[tokio::test]
    async fn blocked_deactivation_and_acquire_serialise_in_arrival_order() {
        run("race-d-", false, |env| async move {
            let u = env.user("u").await;
            assert!(bounded(upsert("race-act", &u, "s1")).await.unwrap());
            let holder = env.hold(&u).await;
            let del = spawn_local({
                let u = u.clone();
                async move {
                    store(&app_name("race-del", &u))
                        .await
                        .private_session_delete(&u, "s1")
                        .await
                }
            });
            env.blocked("race-del", &u).await;
            let acq = acquire("race-acq", &u, "s1", "t1", "p1", false);
            env.blocked("race-acq", &u).await;
            assert_eq!(env.sessions(&u).await, 1, "delete is not visible early");

            holder.batch_execute("COMMIT").await.unwrap();
            bounded(del).await.unwrap();
            acquired(bounded(acq).await, 1);
            assert_eq!(env.sessions(&u).await, 0);
            assert!(env.marks(&u).await.is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn mark_failure_rolls_back_and_preserves_the_incumbent_lease() {
        run("race-fail-", true, |env| async move {
            env.obs
                .batch_execute(
                    "CREATE OR REPLACE FUNCTION public.race_fault_private_plays()
                     RETURNS trigger LANGUAGE plpgsql AS $f$
                     BEGIN
                       IF NEW.user_id LIKE 'race-fail-%' THEN
                         RAISE EXCEPTION 'injected private mark failure';
                       END IF;
                       RETURN NEW;
                     END $f$;
                     DROP TRIGGER IF EXISTS race_fault ON public.private_plays;
                     CREATE TRIGGER race_fault BEFORE INSERT ON public.private_plays
                       FOR EACH ROW EXECUTE FUNCTION public.race_fault_private_plays();",
                )
                .await
                .unwrap();
            let takeover_user = env.user("takeover").await;
            let replace_user = env.user("replace").await;

            // Public incumbents, then privacy turns on for the new session.
            acquired(
                bounded(acquire(
                    "race-acq",
                    &takeover_user,
                    "old",
                    "t0",
                    "pold",
                    false,
                ))
                .await,
                1,
            );
            acquired(
                bounded(acquire(
                    "race-acq",
                    &replace_user,
                    "same",
                    "t0",
                    "pold",
                    false,
                ))
                .await,
                1,
            );
            assert!(bounded(upsert("race-act", &takeover_user, "new"))
                .await
                .unwrap());
            assert!(bounded(upsert("race-act", &replace_user, "same"))
                .await
                .unwrap());

            let a = bounded(acquire(
                "race-acq",
                &takeover_user,
                "new",
                "t1",
                "pnew",
                true,
            ))
            .await;
            let b = bounded(acquire(
                "race-acq",
                &replace_user,
                "same",
                "t1",
                "pnew",
                false,
            ))
            .await;
            assert!(a.is_err() && b.is_err(), "a failed mark must fail closed");
            for (user, session) in [(&takeover_user, "old"), (&replace_user, "same")] {
                assert_eq!(
                    env.lease(user).await,
                    Some((session.to_owned(), "pold".to_owned(), 1)),
                    "incumbent lease and epoch survive the rolled-back attempt"
                );
                assert!(env.marks(user).await.is_empty());
            }

            // Recovery once the fault is gone: marked and acquired together.
            env.obs
                .batch_execute("DROP TRIGGER race_fault ON public.private_plays")
                .await
                .unwrap();
            acquired(
                bounded(acquire(
                    "race-acq",
                    &takeover_user,
                    "new",
                    "t1",
                    "pnew",
                    true,
                ))
                .await,
                2,
            );
            assert_eq!(
                env.marks(&takeover_user).await,
                [Env::key(&takeover_user, "t1", "pnew")]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn refused_acquire_leaves_no_private_mark() {
        run("race-e-", false, |env| async move {
            let u = env.user("u").await;
            acquired(
                bounded(acquire("race-acq", &u, "old", "t0", "pold", false)).await,
                1,
            );
            assert!(bounded(upsert("race-act", &u, "new")).await.unwrap());
            let held = bounded(acquire("race-acq2", &u, "new", "t1", "pnew", false)).await;
            assert!(matches!(held, Ok(LeaseAcquire::Held { .. })));
            assert!(env.marks(&u).await.is_empty());
            assert_eq!(env.lease(&u).await.unwrap().1, "pold");
        })
        .await;
    }

    #[tokio::test]
    async fn private_marks_hide_history_but_never_billing() {
        run("race-h-", false, |env| async move {
            let u = env.user("u").await;
            assert!(bounded(upsert("race-act", &u, "s1")).await.unwrap());
            acquired(
                bounded(acquire("race-acq", &u, "s1", "trk", "ppriv", false)).await,
                1,
            );
            store(&app_name("race-del", &u))
                .await
                .private_session_delete(&u, "s1")
                .await
                .unwrap();
            acquired(
                bounded(acquire("race-acq2", &u, "s1", "trk", "ppub", false)).await,
                2,
            );
            for (i, play) in ["ppriv", "ppub"].into_iter().enumerate() {
                env.obs
                    .execute(
                        "INSERT INTO public.stream_play_ledger
                         (idempotency_key,user_id,track_id,period_month,stream_seconds,bytes_served,recorded_at)
                         VALUES ($1,$2,'trk','1970-01',1,48000,$3)",
                        &[&Env::key(&u, "trk", play), &u, &(2000 + i as i64)],
                    )
                    .await
                    .unwrap();
            }
            let rows = store(&app_name("race-hist", &u)).await.history_list(&u).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert!(rows[0].play_id.contains("ppub") && !rows[0].play_id.contains("ppriv"));
            let billed: i64 = env
                .obs
                .query_one(
                    "SELECT sum(stream_seconds)::bigint FROM public.stream_play_ledger WHERE user_id = $1",
                    &[&u],
                )
                .await
                .unwrap()
                .get(0);
            assert_eq!(billed, 2, "the private play is still metered");
        })
        .await;
    }

    #[tokio::test]
    async fn the_account_lock_is_per_user_and_marks_never_cross_accounts() {
        run("race-x-", false, |env| async move {
            let a = env.user("a").await;
            let b = env.user("b").await;
            let c = env.user("c").await;
            for u in [&a, &b] {
                assert!(bounded(upsert("race-act", u, "s1")).await.unwrap());
            }
            let holder = env.hold(&a).await;
            let blocked = acquire("race-acq-a", &a, "s1", "t1", "p1", false);
            env.blocked("race-acq-a", &a).await;

            // Other accounts proceed while A's row is held.
            acquired(
                bounded(acquire("race-acq-b", &b, "s1", "t1", "p1", false)).await,
                1,
            );
            acquired(
                bounded(acquire("race-acq-c", &c, "s1", "t1", "p1", false)).await,
                1,
            );
            assert!(!blocked.is_finished());
            assert!(env.marks(&a).await.is_empty());
            assert_eq!(env.marks(&b).await, [Env::key(&b, "t1", "p1")]);
            assert!(env.marks(&c).await.is_empty(), "no session, no mark");

            holder.batch_execute("COMMIT").await.unwrap();
            acquired(bounded(blocked).await, 1);
            assert_eq!(env.marks(&a).await, [Env::key(&a, "t1", "p1")]);
            assert_eq!(env.marks(&b).await, [Env::key(&b, "t1", "p1")]);
        })
        .await;
    }

    #[tokio::test]
    async fn account_deletion_during_the_wait_fences_the_acquire_and_the_activation() {
        run("race-z-", false, |env| async move {
            let u = env.user("u").await;
            assert!(bounded(upsert("race-act", &u, "s1")).await.unwrap());
            let holder = env.hold(&u).await;
            let acq = acquire("race-acq", &u, "s1", "t1", "p1", false);
            env.blocked("race-acq", &u).await;
            holder
                .execute(
                    "UPDATE public.users SET deleting_at = now() WHERE id = $1",
                    &[&u],
                )
                .await
                .unwrap();
            holder.batch_execute("COMMIT").await.unwrap();

            assert!(!matches!(
                bounded(acq).await,
                Ok(LeaseAcquire::Acquired { .. })
            ));
            assert!(env.lease(&u).await.is_none());
            assert!(env.marks(&u).await.is_empty());
            assert!(!bounded(upsert("race-act2", &u, "s2")).await.unwrap());
            assert_eq!(env.sessions(&u).await, 1, "no new session after the fence");
        })
        .await;
    }
}
