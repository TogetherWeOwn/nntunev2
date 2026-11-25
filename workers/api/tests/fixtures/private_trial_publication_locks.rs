use super::*;
use tokio_postgres::{error::SqlState, Transaction};

const LOCK_DB: &str = "postgresql://postgres:ci-throwaway@127.0.0.1:5432/nntunev2_staging";
const CONTRACT: &str = include_str!("../../../../scripts/private_trial_publication_locks.sql");
const OWNER_LOCK: &str = include_str!("../../src/handlers/private_trial/lock.sql");
const SESSION: &str = "synthetic-publication-session";
const LOCK_PROBES: &[&str] = &[
    "SELECT 1 FROM public.users WHERE id = 'catalogue-owner' FOR NO KEY UPDATE NOWAIT",
    "SELECT 1 FROM public.users WHERE id = 'library-invitee' FOR NO KEY UPDATE NOWAIT",
    "SELECT 1 FROM public.sessions WHERE id = 'synthetic-publication-session' FOR NO KEY UPDATE NOWAIT",
    "SELECT 1 FROM public.invite_codes WHERE id = 901 FOR NO KEY UPDATE NOWAIT",
    "SELECT 1 FROM public.ingest_jobs WHERE id = 'catalogue-new' FOR NO KEY UPDATE NOWAIT",
    "SELECT 1 FROM public.tracks WHERE release_id = 'ab310000-0000-4000-8000-000000000000'
       AND medium_no = 1 AND track_no = 99 FOR NO KEY UPDATE NOWAIT",
];

async fn contract(
    client: &Client,
    action: &str,
    role_oid: u32,
) -> Result<(), tokio_postgres::Error> {
    client
        .query_one(
            "SELECT set_config('nntune.private_trial_lock_action', $1, false),
                set_config('nntune.private_trial_writer_oid', $2, false)",
            &[&action, &role_oid.to_string()],
        )
        .await
        .unwrap();
    let result = client.batch_execute(CONTRACT).await;
    if result.is_err() {
        client.batch_execute("ROLLBACK").await.unwrap();
    }
    result
}

async fn lock_owner(client: &Transaction<'_>, now: i64) -> Result<String, tokio_postgres::Error> {
    client
        .query_one(
            OWNER_LOCK,
            &[
                &ISSUER,
                &SESSION,
                &901_i64,
                &now,
                &(now + 300),
                &0_i32,
                &(now + 300),
                &(now + 300),
            ],
        )
        .await
        .map(|row| row.get(0))
}

async fn assert_blocked(observer: &Client, probes: &[&str]) {
    for probe in probes {
        let denied = observer.query_one(*probe, &[]).await.expect_err(probe);
        assert_eq!(
            denied.code(),
            Some(&SqlState::LOCK_NOT_AVAILABLE),
            "{probe}"
        );
    }
}

async fn assert_permission_denied(client: &Client, sql: &str, guard: bool) {
    let error = client.execute(sql, &[]).await.expect_err(sql);
    let error = error.as_db_error().unwrap();
    assert_eq!(
        error.code(),
        &SqlState::INSUFFICIENT_PRIVILEGE,
        "{sql}: {error}"
    );
    assert_eq!(
        error.message() == "private trial writer may only lock this relation",
        guard,
        "{sql}: {error}"
    );
}

#[tokio::test]
async fn scoped_writer_locks_and_publishes_without_base_updates() {
    let observer = connect_fixture(LOCK_DB).await;
    let fixture = publication_fixture::PublicationFixture::load(&observer, JOB).await;
    let source = fixture.source();
    observer
        .batch_execute(concat!(
            "CREATE ROLE trial_lock_writer LOGIN NOINHERIT;
         CREATE ROLE trial_lock_reader LOGIN NOINHERIT;
         CREATE ROLE trial_lock_executor NOINHERIT;
         GRANT USAGE, CREATE ON SCHEMA public TO trial_lock_executor;
         ALTER TABLE public.users OWNER TO trial_lock_executor;
         ALTER TABLE public.sessions OWNER TO trial_lock_executor;
         ALTER TABLE public.invite_codes OWNER TO trial_lock_executor;
         ALTER TABLE public.ingest_jobs OWNER TO trial_lock_executor;
         ALTER TABLE public.tracks OWNER TO trial_lock_executor;
         UPDATE public.users SET email = 'listener@example.test' WHERE id = 'library-invitee';
         UPDATE public.invite_codes SET email = 'listener@example.test' WHERE id = 901;",
            include_str!("private_trial_publication_access.sql"),
        ))
        .await
        .unwrap();
    let now: i64 = observer.query_one(
        "INSERT INTO public.sessions (id, user_id, created_at, expires_at)
         SELECT $1, $2, now_sec, now_sec + 300 FROM (SELECT extract(epoch FROM now())::bigint) t(now_sec)
         RETURNING created_at", &[&SESSION, &ISSUER],
    ).await.unwrap().get(0);
    let writer_oid = observer
        .query_one("SELECT 'trial_lock_writer'::regrole::oid", &[])
        .await
        .unwrap()
        .get(0);
    let reader_oid = observer
        .query_one("SELECT 'trial_lock_reader'::regrole::oid", &[])
        .await
        .unwrap()
        .get(0);
    let mut writer = connect_fixture(LOCK_DB).await;
    writer
        .batch_execute("SET SESSION AUTHORIZATION trial_lock_writer")
        .await
        .unwrap();
    let tx = writer.transaction().await.unwrap();
    assert_eq!(
        lock_owner(&tx, now).await.unwrap_err().code(),
        Some(&SqlState::INSUFFICIENT_PRIVILEGE)
    );
    tx.rollback().await.unwrap();
    let executor = connect_fixture(LOCK_DB).await;
    executor
        .batch_execute("SET SESSION AUTHORIZATION trial_lock_executor")
        .await
        .unwrap();
    assert!(executor
        .query_one(
            "SELECT current_user = 'trial_lock_executor' AND session_user = current_user
                AND NOT current_setting('is_superuser')::boolean",
            &[],
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    contract(&executor, "grant", writer_oid).await.unwrap();
    assert!(observer.query_one(
        "SELECT count(*) FILTER (WHERE attname = 'id') = 5 AND bool_and(
             NOT has_table_privilege($1::oid, attrelid, 'UPDATE')
             AND has_column_privilege($1::oid, attrelid, attnum, 'UPDATE') = (attname = 'id')
             AND NOT has_column_privilege($2::oid, attrelid, attnum, 'UPDATE'))
         FROM pg_attribute
         WHERE attrelid IN ('public.users'::regclass, 'public.sessions'::regclass,
              'public.invite_codes'::regclass, 'public.ingest_jobs'::regclass, 'public.tracks'::regclass)
           AND attnum > 0 AND NOT attisdropped", &[&writer_oid, &reader_oid],
    ).await.unwrap().get::<_, bool>(0));

    let reader = connect_fixture(LOCK_DB).await;
    reader
        .batch_execute("SET SESSION AUTHORIZATION trial_lock_reader")
        .await
        .unwrap();
    let policy = fixture.policy(ISSUER);
    assert!(page(&reader, RECIPIENT, &policy).await.unwrap().is_empty());
    let occurrence = fixture.occurrence(JOB, ISSUER, MBID);
    let mut prior = None;
    for replay in [false, true] {
        let mut tx = writer.transaction().await.unwrap();
        assert_eq!(lock_owner(&tx, now).await.unwrap(), RECIPIENT);
        assert_blocked(&observer, &LOCK_PROBES[..4]).await;
        let track = stage(&mut tx, &occurrence, &source).await.unwrap();
        assert_blocked(&observer, &LOCK_PROBES[4..5]).await;
        if replay {
            assert_eq!(prior.as_ref(), Some(&track));
            assert_blocked(&observer, &LOCK_PROBES[5..]).await;
        }
        assert!(stage_admission(
            &tx,
            Some(&policy),
            Admission {
                issuer: ISSUER,
                recipient: RECIPIENT,
                invite_id: 901,
                track_id: &track,
                job_id: JOB,
                plaintext_sha256: source.plaintext_sha256,
            }
        )
        .await
        .unwrap());
        assert!(tx
            .query_one(
                include_str!("../../src/handlers/private_trial/reference.sql"),
                &[&track, &(now + 300)]
            )
            .await
            .unwrap()
            .get::<_, bool>(0));
        tx.commit().await.unwrap();
        for probe in LOCK_PROBES {
            observer.query_one(*probe, &[]).await.unwrap();
        }
        let listed = page(&reader, RECIPIENT, &policy).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, track);
        prior = Some(track);
    }

    for table in ["users", "sessions", "invite_codes", "ingest_jobs", "tracks"] {
        for predicate in ["true", "false"] {
            let sql = format!("UPDATE public.{table} SET id = id WHERE {predicate}");
            assert_permission_denied(&writer, &sql, true).await;
            assert_permission_denied(&reader, &sql, false).await;
        }
    }
    for (table, columns) in [
        (
            "users",
            &(["deleting_at", "role", "email", "invite_code_hash"])[..],
        ),
        (
            "sessions",
            &[
                "revoked",
                "user_id",
                "created_at",
                "expires_at",
                "rotation_counter",
            ],
        ),
        (
            "invite_codes",
            &[
                "used",
                "kind",
                "code_hash",
                "issued_by",
                "email",
                "redeemed_by",
                "redeemed_at",
                "expires_at",
                "revoked_at",
            ],
        ),
        ("ingest_jobs", &["user_id", "status", "accepted_seq"]),
        (
            "tracks",
            &[
                "release_id",
                "mbid",
                "medium_no",
                "track_no",
                "title",
                "duration_ms",
            ],
        ),
    ] {
        for column in columns {
            assert_permission_denied(
                &writer,
                &format!("UPDATE public.{table} SET {column} = DEFAULT"),
                false,
            )
            .await;
        }
    }
    assert_permission_denied(
        &reader,
        "DELETE FROM public.private_library_admissions",
        false,
    )
    .await;
    assert_permission_denied(
        &reader,
        "UPDATE public.private_library_admissions SET accepted_seq = accepted_seq",
        false,
    )
    .await;
    assert_permission_denied(&reader, "INSERT INTO public.tracks DEFAULT VALUES", false).await;

    // The owner can still change protected state.
    assert_eq!(
        observer
            .execute(
                "UPDATE public.invite_codes SET revoked_at = 3 WHERE id = 901",
                &[]
            )
            .await
            .unwrap(),
        1
    );
    assert!(page(&reader, RECIPIENT, &policy).await.unwrap().is_empty());
    observer
        .batch_execute("ALTER ROLE trial_lock_writer RENAME TO trial_lock_writer_renamed")
        .await
        .unwrap();
    assert_permission_denied(&writer, "UPDATE public.users SET id = id", true).await;
    let assumed = connect_fixture(LOCK_DB).await;
    assumed
        .batch_execute("SET ROLE trial_lock_writer_renamed")
        .await
        .unwrap();
    assert_permission_denied(&assumed, "UPDATE public.users SET id = id", true).await;
    assert!(contract(&executor, "revoke", reader_oid).await.is_err());
    assert_permission_denied(&writer, "UPDATE public.users SET id = id", true).await;
    // A stray grant on the last table must leave even the earlier guards in place.
    executor
        .batch_execute("GRANT UPDATE (duration_ms) ON public.tracks TO trial_lock_writer_renamed")
        .await
        .unwrap();
    assert!(contract(&executor, "revoke", writer_oid).await.is_err());
    assert_permission_denied(&writer, "UPDATE public.users SET id = id", true).await;
    executor
        .batch_execute(
            "REVOKE UPDATE (duration_ms) ON public.tracks FROM trial_lock_writer_renamed",
        )
        .await
        .unwrap();
    contract(&executor, "revoke", writer_oid).await.unwrap();
    assert!(observer
        .query_one(
            "SELECT to_regprocedure('public.reject_private_trial_publication_update()') IS NULL
           AND NOT EXISTS (SELECT FROM pg_trigger WHERE tgname = 'private_trial_lock_only')
           AND NOT EXISTS (SELECT FROM unnest(ARRAY['public.users', 'public.sessions',
                'public.invite_codes', 'public.ingest_jobs', 'public.tracks']) r
                WHERE has_any_column_privilege($1::oid, r::regclass, 'UPDATE'))",
            &[&writer_oid],
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    assert_permission_denied(&writer, "UPDATE public.users SET id = id", false).await;
    assert_eq!(
        writer
            .query(
                "SELECT id FROM public.users WHERE id = 'catalogue-owner'",
                &[]
            )
            .await
            .unwrap()
            .len(),
        1
    );
}
