//! Production catalogue reads with only connection acquisition injected.
//! CI's disposable PostgreSQL owns the app rows; no mirror service is required.
use nntune_api::catalogue::{assemble_library_rows, LibraryPageRow};
use nntune_api::handlers::{Store, StoreError};
use nntune_api::worker_entry::{catalogue_store_for_postgres_test, SubsonicSnapshotCatalogueStore};
use std::cell::Cell;
use std::rc::Rc;
use tokio_postgres::{Client, Config, NoTls};

const RELEASE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const RECORDING: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const OTHER_RELEASE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

async fn client(populated: bool) -> Client {
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
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client.batch_execute(
        "SET statement_timeout = '3s'; SET search_path = pg_temp;
         CREATE TEMP TABLE library (user_id TEXT, job_id TEXT, release_mbid TEXT, recording_mbids TEXT[], idempotency_key TEXT PRIMARY KEY DEFAULT gen_random_uuid()::text)",
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
async fn shared_memberships_use_production_queries_without_private_leaks() {
    use nntune_api::catalogue::*;
    let app = client(true).await;
    app.execute(
        "INSERT INTO library VALUES (NULL, 'shared', $1, ARRAY[$2])",
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
        assert_eq!(page.len(), if caller == "u1" { 5 } else { 1 });
        assert!(page.iter().all(|r| r.get::<_, String>(0) != "private"));
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
async fn taste_upsert_isolates_owners_and_rolls_back_invalid_replacement() {
    use nntune_api::target_identity::TargetKind::Track;
    use nntune_api::taste_signal::TasteSignal::{Ban, Snooze};
    let observer = client(false).await;
    observer.batch_execute("INSERT INTO public.users (id,email,created_at) VALUES ('taste-iso-a','taste-iso-a@example.invalid',1), ('taste-iso-b','taste-iso-b@example.invalid',1); INSERT INTO public.account_taste VALUES ('taste-iso-a','track','job','love',NULL,1,1), ('taste-iso-b','track','job','love',NULL,2,2)").await.unwrap();
    let store = catalogue_store_for_postgres_test(
        client(false).await,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    );
    let changed = store
        .taste_upsert("taste-iso-a", Track, "job", Ban, None, 3)
        .await;
    let store = catalogue_store_for_postgres_test(
        client(false).await,
        false,
        Err(StoreError),
        Rc::new(Cell::new(0)),
    );
    let rejected = store
        .taste_upsert("taste-iso-a", Track, "job", Snooze, None, 4)
        .await;
    let rows = observer.query("SELECT user_id, signal, updated_at FROM public.account_taste WHERE user_id IN ('taste-iso-a','taste-iso-b') ORDER BY user_id", &[]).await.unwrap();
    let states: Vec<(String, String, i64)> = rows
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect();
    observer
        .batch_execute("DELETE FROM public.users WHERE id IN ('taste-iso-a','taste-iso-b')")
        .await
        .unwrap();
    assert!(changed.is_ok());
    assert!(rejected.is_err());
    assert_eq!(
        states,
        vec![
            ("taste-iso-a".into(), "ban".into(), 3),
            ("taste-iso-b".into(), "love".into(), 2)
        ]
    );
}

#[tokio::test]
async fn taste_upsert_waits_for_deletion_fence() {
    use nntune_api::target_identity::TargetKind::Track;
    use nntune_api::taste_signal::TasteSignal::Ban;
    use std::time::Duration;
    let observer = client(false).await;
    let locker = client(false).await;
    let writer = client(false).await;
    let writer_pid: i32 = writer
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    observer.batch_execute("INSERT INTO public.users (id,email,created_at) VALUES ('taste-race','taste-race@example.invalid',1); INSERT INTO public.account_taste VALUES ('taste-race','track','old','love',NULL,1,1)").await.unwrap();
    locker
        .batch_execute("BEGIN; UPDATE public.users SET deleting_at = now() WHERE id = 'taste-race'")
        .await
        .unwrap();
    let store =
        catalogue_store_for_postgres_test(writer, false, Err(StoreError), Rc::new(Cell::new(0)));
    let write = store.taste_upsert("taste-race", Track, "old", Ban, None, 10);
    let release = async {
        let observed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let waiting: bool = observer
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1 AND NOT granted)",
                        &[&writer_pid],
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
    let (result, observed) = tokio::join!(write, release);
    let row = observer.query_one("SELECT signal, created_at, updated_at FROM public.account_taste WHERE user_id = 'taste-race' AND target_id = 'old'", &[]).await.unwrap();
    let state: (String, i64, i64) = (row.get(0), row.get(1), row.get(2));
    observer
        .batch_execute("DELETE FROM public.users WHERE id = 'taste-race'")
        .await
        .unwrap();
    let remaining: i64 = observer
        .query_one(
            "SELECT count(*) FROM public.account_taste WHERE user_id = 'taste-race'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(observed, "writer must wait on the deleting account");
    assert!(result.is_err());
    assert_eq!(state, ("love".into(), 1, 1));
    assert_eq!(remaining, 0, "account deletion cascades existing taste");
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
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute(
            "SET statement_timeout = '3s'; SET search_path = pg_temp;
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
async fn subsonic_snapshot_rejects_a_capped_query_result() {
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
        let result = store.subsonic_snapshot_catalogue_with_cap("u1", cap).await;
        if cap == 2 {
            assert!(result.is_err(), "a full SQL page must fail closed");
        } else {
            assert_eq!(result.unwrap().2.len(), 2, "a partial page is complete");
        }
        assert_eq!(attempts.get(), 1);
    }
}

#[tokio::test]
async fn subsonic_snapshot_rejects_capped_artist_and_release_pages() {
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
            let result = store.subsonic_snapshot_catalogue_with_cap("u1", cap).await;
            if cap == 2 {
                assert!(
                    result.is_err(),
                    "a full artist or release page must fail closed"
                );
            } else {
                let (artists, releases, tracks) = result.unwrap();
                assert_eq!(artists.len(), if distinct_artists { 2 } else { 1 });
                assert_eq!(releases.len(), 2);
                assert!(tracks.is_empty());
            }
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
        for count in [3_u32, 4, 5] {
            let ids: Vec<String> = (1..=count).map(|n| synthetic_uuid(1, n)).collect();
            let app = client(false).await;
            if pair_mode {
                app.execute(
                    "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) VALUES ('u1', 'pairs', $1, $2)",
                    &[&SNAPSHOT_RELEASE, &ids],
                ).await.unwrap();
            } else {
                app.execute(
                    "INSERT INTO library (user_id, job_id, release_mbid, recording_mbids) SELECT 'u1', r, r, ARRAY[]::text[] FROM unnest($1::text[]) AS r",
                    &[&ids],
                ).await.unwrap();
            }
            let (result, attempts) = snapshot_of(app, mirror_client().await, 2).await;
            assert_eq!(result.is_ok(), count < 4, "pair={pair_mode}, count={count}");
            if count >= 4 {
                assert_eq!(attempts, 0);
            }
        }
    }
    let app = client(false).await;
    let malformed: Vec<String> = (0..4).map(|n| format!("invalid-{n}")).collect();
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
