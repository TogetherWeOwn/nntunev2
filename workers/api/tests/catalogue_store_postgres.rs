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
