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
